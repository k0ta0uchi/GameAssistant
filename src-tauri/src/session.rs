use chrono::{DateTime, Local, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

use crate::ai_client::{AiClient, AiGenerateOptions, ChatMessage};
use crate::asr::{
    AsrEngine, WakeWordAction, WakeWordConfig, WakeWordDecision, WakeWordMode, WakeWordPhase,
};
use crate::audio_input::AudioInputManager;
use crate::lance_memory::{self, MemoryItem, StoredMemory, SummaryBackfillApplyResult};
use crate::local_summary::LocalSummaryService;
use crate::logger::LogManager;
use crate::memory_v2::policy::{Policy, PrivacyAdmission};
use crate::memory_v2::repository::{
    MemoryRepository, SummaryBatchInput, SummaryProcessDecision, SummaryProcessMode,
    SummaryStatusRecord, SUMMARY_MODEL_ID as MEMORY_SUMMARY_MODEL_ID,
    SUMMARY_PROMPT_VERSION as MEMORY_SUMMARY_PROMPT_VERSION,
};
use crate::summary_failure::{reason_from_error, SummaryFailureReason};
use crate::tts::{TtsManager, TtsSettings};
use crate::web_search::WebSearchClient;
use crate::window_capture;

pub(crate) fn memory_event_log_message(
    event_id: &str,
    event_type: &str,
    source: &str,
    content: &str,
    status: &str,
) -> String {
    format!(
        "event_id={} type={} source={} bytes={} chars={} status={}",
        event_id,
        event_type,
        source,
        content.len(),
        content.chars().count(),
        status
    )
}

/// Automatic note generation is part of the session-stop workflow.  Older
/// portable installs may not have the setting key yet, so a missing value
/// follows the feature's documented default (enabled); only an explicit
/// `false` opts out.
fn automatic_blog_post_enabled(settings: &serde_json::Value) -> bool {
    settings
        .get("create_blog_post")
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

/// Upper bound for waiting on in-flight raw-save tasks after Stop Session.
/// Guards are RAII so the drain cannot leak; this bound only prevents a hung
/// backend write from delaying blog generation forever.
const SESSION_EVENT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Safety bounds for the persisted blog fallback.  The normal in-memory ring
/// is already capped, but a remounted/empty UI can force a LanceDB fallback;
/// keep that path both session-scoped and bounded before it reaches Gemini.
const BLOG_FALLBACK_MAX_EVENTS: usize = 200;
const BLOG_MAX_SOURCE_BYTES: usize = 64 * 1024;

/// Utterances containing any of these fragments stop TTS playback and cancel
/// prompt collection. The finalized utterance itself is still persisted; this
/// predicate only decides the extra stop action.
fn contains_stop_word(text: &str) -> bool {
    ["ストップ", "だまって", "静かに"]
        .iter()
        .any(|stop_word| text.contains(stop_word))
}

/// Build a collision-safe Markdown path for one session's article.  The stamp
/// keeps articles sortable per session; when two stops land within the same
/// second (regeneration, double stop), a numeric suffix keeps both articles
/// instead of silently overwriting the first one.
fn unique_blog_path(blogs_dir: &std::path::Path, stamp: &str) -> PathBuf {
    let mut candidate = blogs_dir.join(format!("{}.md", stamp));
    let mut counter = 2;
    while candidate.exists() {
        candidate = blogs_dir.join(format!("{}_{}.md", stamp, counter));
        counter += 1;
    }
    candidate
}

/// Only speech that came through an active ASR stream is curated while a
/// session is running.  Twitch/chat and older raw rows remain available to
/// the Memory Manager, where the user can explicitly run Process all.
fn live_asr_summary_event(event_type: &str) -> bool {
    matches!(event_type, "user_speech" | "discord_speech")
}

fn wake_word_config_from_settings(settings: &serde_json::Value) -> (WakeWordConfig, bool) {
    let requested_engine = settings
        .get("wake_word_engine")
        .and_then(|value| value.as_str())
        .unwrap_or("whisper_vad");
    let engine_supported = requested_engine == "whisper_vad";

    let custom_wake_words = settings
        .get("custom_wake_words")
        .and_then(|value| {
            value
                .as_str()
                .map(|text| {
                    text.split([',', '、'])
                        .map(str::trim)
                        .filter(|word| !word.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .or_else(|| {
                    value.as_array().map(|words| {
                        words
                            .iter()
                            .filter_map(|word| word.as_str())
                            .map(str::trim)
                            .filter(|word| !word.is_empty())
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                })
        })
        .unwrap_or_default();

    let mode = match settings
        .get("wake_word_mode")
        .and_then(|value| value.as_str())
    {
        Some("disabled") => WakeWordMode::Disabled,
        Some("all_final_speech") | Some("all") => WakeWordMode::AllFinalSpeech,
        Some("require_wake_word") | Some("whisper_vad") => WakeWordMode::RequireWakeWord,
        _ => WakeWordMode::RequireWakeWord,
    };

    let cooldown_ms = settings
        .get("wake_word_cooldown_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(crate::asr::DEFAULT_WAKE_WORD_COOLDOWN_MS)
        .clamp(1_500, 2_000);

    (
        WakeWordConfig::new(custom_wake_words)
            .with_mode(mode)
            .with_cooldown_ms(cooldown_ms),
        engine_supported,
    )
}

fn prompt_collection_from_decision(decision: &WakeWordDecision) -> bool {
    matches!(decision.phase, WakeWordPhase::AwaitingPrompt)
        || (decision.phase == WakeWordPhase::Armed && decision.clean_prompt.trim().is_empty())
}

fn stale_wake_word_decision(
    stream: &str,
    text: &str,
    is_final: bool,
    generation: u64,
) -> WakeWordDecision {
    WakeWordDecision {
        stream: stream.to_string(),
        engine: crate::asr::WAKE_WORD_ENGINE,
        session_generation: generation,
        is_final,
        phase: WakeWordPhase::Idle,
        wake_word_checked: false,
        wake_word_detected: false,
        clean_prompt: text.to_string(),
        action: WakeWordAction::Ignored,
        should_acknowledge: false,
        is_prompt: false,
        cooldown_active: false,
        duplicate_suppressed: false,
    }
}

fn wake_decision_log_message(
    event_id: &str,
    stream: &str,
    text: &str,
    decision: &WakeWordDecision,
    collecting: bool,
) -> String {
    format!(
        "{} engine={} phase={:?} checked={} triggered={} final={} cooldown_active={} duplicate_suppressed={} action={:?} collecting={}",
        memory_event_log_message(event_id, "wake_word", stream, text, "checked"),
        decision.engine,
        decision.phase,
        decision.wake_word_checked,
        decision.wake_word_detected,
        decision.is_final,
        decision.cooldown_active,
        decision.duplicate_suppressed,
        decision.action,
        collecting,
    )
}

/// Convert persisted rows into a bounded blog fallback.  Stop-time callers
/// pass the session's captured UTC boundary; rows with malformed timestamps
/// are excluded rather than risking unrelated historical content in the
/// prompt.  The no-boundary case is retained for the manual command but is
/// still capped by [`BLOG_FALLBACK_MAX_EVENTS`].
fn persisted_blog_fallback_events(
    memories: Vec<StoredMemory>,
    session_started_at: Option<&DateTime<Utc>>,
) -> Vec<SessionEvent> {
    let mut events = Vec::with_capacity(memories.len().min(BLOG_FALLBACK_MAX_EVENTS));
    for memory in memories {
        if let Some(start) = session_started_at {
            let Ok(occurred_at) = DateTime::parse_from_rfc3339(&memory.timestamp) else {
                continue;
            };
            if occurred_at.with_timezone(&Utc) < *start {
                continue;
            }
        }
        events.push(SessionEvent {
            id: memory.id,
            r#type: memory.memory_type,
            author: memory.source,
            content: memory.document,
            timestamp: memory.timestamp,
        });
        if events.len() >= BLOG_FALLBACK_MAX_EVENTS {
            break;
        }
    }
    events
}

/// Admit text at the memory boundary.  The live session may retain the
/// original text for an AI request, but every memory-facing copy must be a
/// validated, idempotently redacted value.
fn admit_redacted_memory_text(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    Policy::default()
        .admit_raw_text(input, PrivacyAdmission::public())
        .ok()
        .map(|text| text.as_str().trim().to_string())
        .filter(|text| !text.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: String,
    pub r#type: String, // "twitch_chat" | "user_speech" | "ai_response" | "auto_commentary"
    pub author: String,
    pub content: String,
    pub timestamp: String,
}

/// Immutable identity captured by every callback/task belonging to one
/// Start→Stop interval. A task may finish its already-admitted raw write after
/// Stop, but it must never perform prompt/AI/UI work for a newer session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionContext {
    session_id: String,
    generation: u64,
    started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct PersistedAsrEvent {
    event: SessionEvent,
    decision: WakeWordDecision,
    stop_word_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBackfillProgress {
    pub state: String,
    pub processed: usize,
    pub total: usize,
    pub queued: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Number of completed summaries durably committed during this pass.
    pub persisted: usize,
    /// Number of rows excluded by the shared admission boundary. These rows
    /// advance scan progress but are never summary-skipped or mutated.
    #[serde(default)]
    pub excluded: usize,
    /// Number of candidate rows sent through inference. This is separate from
    /// `processed`, which only advances after a durable terminal commit.
    #[serde(default)]
    pub attempted: usize,
    /// Number of corrective/runtime retries, kept separate from final row
    /// counts. A row that fails after one retry still contributes one
    /// `failed` row and one `retry_count`.
    #[serde(default)]
    pub retry_count: usize,
    /// Rows that have not reached a durable terminal outcome yet.
    #[serde(default)]
    pub remaining: usize,
    /// Machine-readable row warning counts. Values never contain source text.
    #[serde(default)]
    pub reason_counts: std::collections::BTreeMap<String, usize>,
    /// Set only for a run-fatal failure. Row warnings must not populate this.
    #[serde(default)]
    pub fatal_error: Option<String>,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBackfillStart {
    pub accepted: bool,
    pub progress: MemoryBackfillProgress,
}

enum BackfillResult {
    Completed {
        summary: SummaryBatchInput,
        warning: Option<String>,
        retry_count: usize,
    },
    Skipped {
        id: String,
        reason: String,
        retry_count: usize,
    },
    Fallback {
        id: String,
        reason: String,
        retry_count: usize,
    },
    Fatal {
        reason: String,
        retry_count: usize,
    },
}

const BACKFILL_MAX_SOURCE_CHARS: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackfillLogSeverity {
    Info,
    Warning,
    Error,
}

fn backfill_log_severity(status: &str) -> BackfillLogSeverity {
    match status {
        "fatal" | "error" => BackfillLogSeverity::Error,
        "fallback" | "skipped" | "warning" | "projection_warning" => BackfillLogSeverity::Warning,
        _ => BackfillLogSeverity::Info,
    }
}

/// Structured backfill diagnostics intentionally contain identifiers and
/// reason codes only. In particular, neither source documents nor model
/// requests/responses are accepted by this formatter.
fn backfill_log_message(
    run_id: &str,
    phase: &str,
    event_id: &str,
    chunk: usize,
    processed: usize,
    remaining: usize,
    status: &str,
    reason: &str,
) -> String {
    backfill_log_message_with_counters(
        run_id, phase, event_id, chunk, 1, processed, remaining, 0, 0, 0, 0, status, reason,
    )
}

fn backfill_log_message_with_counters(
    run_id: &str,
    phase: &str,
    event_id: &str,
    chunk: usize,
    attempt: usize,
    processed: usize,
    remaining: usize,
    persisted: usize,
    skipped: usize,
    failed: usize,
    retry_count: usize,
    status: &str,
    reason: &str,
) -> String {
    let reason_code = backfill_reason_code(reason);
    format!(
        "run_id={} phase={} event_id={} chunk={} attempt={} status={} reason={} counters=processed:{};remaining:{};persisted:{};skipped:{};failed:{};retry_count:{}",
        run_id,
        phase,
        event_id,
        chunk,
        attempt,
        status,
        reason_code,
        processed,
        remaining,
        persisted,
        skipped,
        failed,
        retry_count,
    )
}

fn increment_reason(progress: &mut MemoryBackfillProgress, reason: &str) {
    let code = backfill_reason_code(reason);
    if !code.is_empty() {
        *progress.reason_counts.entry(code).or_insert(0) += 1;
    }
}

fn record_final_reason(
    progress: &mut MemoryBackfillProgress,
    finalized_event_ids: &mut std::collections::HashSet<String>,
    event_id: &str,
    reason: &str,
) -> bool {
    if finalized_event_ids.insert(event_id.to_string()) {
        increment_reason(progress, reason);
        true
    } else {
        false
    }
}

fn refresh_backfill_remaining(progress: &mut MemoryBackfillProgress) {
    progress.remaining = progress.total.saturating_sub(progress.processed);
}

fn set_backfill_fatal(progress: &mut MemoryBackfillProgress, reason: &str) {
    let safe_reason = backfill_reason_code(reason);
    progress.state = "error".into();
    progress.fatal_error = Some(safe_reason.clone());
    // `error` is retained for older clients; it now mirrors only a fatal
    // reason and never receives row-local warnings.
    progress.error = Some(safe_reason);
    refresh_backfill_remaining(progress);
}

fn backfill_reason_code(reason: &str) -> String {
    for code in [
        "policy_excluded",
        "metadata_echo",
        "ungrounded_summary",
        "invalid_model_output",
        "embedding_failed",
        "invalid_embedding",
        "inference_failed",
        "model_declined",
        "empty_source",
        "source_too_long",
        "summary_runtime_timeout",
        "summary_runtime_failed",
        "summary_queue_failed",
        "journal_commit_failed",
        "scan_failed",
        "repository_open_failed",
        "queue_failed",
        "backfill_fatal",
        "fact_validation_failed",
        "subject_unresolved",
        "projection_resync_failed",
        "summary_raw_event_read_failed",
        "summary_fact_read_failed",
        "summary_status_read_failed",
        "candidate_admission_changed",
        "candidate_admission_invalid",
        "summary_ready",
        "scan_complete",
        "candidate_admission",
        "queue_complete",
        "durable_commit",
        "completed_with_warnings",
        "success",
        "queue_empty",
        "unknown_reason",
        "row_warning",
    ] {
        if reason.contains(code) {
            return code.to_string();
        }
    }
    if reason.contains("delete tombstone") || reason.contains("deleted_tombstone") {
        return "delete_tombstone".to_string();
    }
    if reason.contains("durable summary") {
        return "durable_summary_exists".to_string();
    }
    if reason.contains("terminal status") {
        return "terminal_status".to_string();
    }
    if reason.contains("concurrent") {
        return "concurrent_state_changed".to_string();
    }
    "unknown_reason".to_string()
}

fn inference_failure_reason(error: &str) -> &'static str {
    match reason_from_error(error) {
        SummaryFailureReason::MetadataEcho => "metadata_echo",
        SummaryFailureReason::UngroundedSummary => "ungrounded_summary",
        SummaryFailureReason::InvalidModelOutput => "invalid_model_output",
        SummaryFailureReason::EmptySource => "empty_source",
        SummaryFailureReason::SourceTooLong => "source_too_long",
        SummaryFailureReason::SummaryRuntimeTimeout => "summary_runtime_timeout",
        SummaryFailureReason::SummaryRuntimeFailed => "summary_runtime_failed",
        SummaryFailureReason::SummaryQueueFailed => "summary_queue_failed",
        SummaryFailureReason::InferenceFailed => "inference_failed",
    }
}

fn retry_count_for_reason(reason: &str) -> usize {
    match backfill_reason_code(reason).as_str() {
        "invalid_model_output"
        | "metadata_echo"
        | "ungrounded_summary"
        | "summary_runtime_timeout"
        | "summary_runtime_failed" => 1,
        _ => 0,
    }
}

fn runtime_failure_reason(error: &str) -> Option<&'static str> {
    let classified = reason_from_error(error);
    if classified.is_runtime_failure() {
        return Some(classified.as_str());
    }
    let lower = error.to_ascii_lowercase();
    let contract_violation = lower.contains("contract violation")
        || lower.contains("invalid_model_output")
        || lower.contains("metadata_echo")
        || lower.contains("ungrounded_summary")
        || lower.contains("summary response is not json")
        || lower.contains("summary response has no boolean")
        || lower.contains("summary response has no message content");
    if contract_violation {
        return None;
    }
    if lower.contains("queue full") || lower.contains("queue is not running") {
        Some("summary_queue_failed")
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("summary_runtime_timeout")
    {
        Some("summary_runtime_timeout")
    } else if lower.contains("server is unavailable")
        || lower.contains("server returned")
        || lower.contains("request failed")
        || lower.contains("read summary response")
        || lower.contains("summary worker dropped")
        || lower.contains("llama-server")
        || lower.contains("runtime setup is not ready")
        || lower.contains("model is not installed")
        || lower.contains("model failed validation")
        || lower.contains("not bundled")
        || lower.contains("terms acknowledgement")
        || lower.contains("summary is disabled")
        || lower.contains("summary_runtime_failed")
    {
        Some("summary_runtime_failed")
    } else {
        None
    }
}

fn record_backfill_commit(
    progress: &mut MemoryBackfillProgress,
    committed_rows: usize,
    requested_skipped: usize,
    requested_fallback: usize,
    result: &SummaryBackfillApplyResult,
) {
    // A retry or duplicate completion must not turn one final row into two
    // terminal rows. In the normal chunk path `committed_rows` is exactly the
    // number of unique candidates; the capacity clamp also makes this helper
    // idempotent when a stale caller replays an already-terminal chunk.
    let commit_capacity = committed_rows.min(progress.total.saturating_sub(progress.processed));
    let requested_skipped = requested_skipped.min(commit_capacity);
    let requested_fallback = requested_fallback.min(commit_capacity);
    let persisted = result.persisted.min(commit_capacity);
    let terminal_failed = result.terminal_failed.min(commit_capacity);
    let terminal_skipped = result.terminal_skipped.min(commit_capacity);
    progress.persisted = progress.persisted.saturating_add(persisted);
    progress.skipped = progress
        .skipped
        .saturating_add(requested_skipped)
        .saturating_add(terminal_skipped);
    progress.failed = progress
        .failed
        .saturating_add(requested_fallback)
        .saturating_add(terminal_failed);
    // A concurrent writer can make a queued row a same-version terminal
    // result while this chunk is committing. The repository intentionally
    // treats that as an idempotent no-op; account for the row as a concurrent
    // skip so the candidate terminal invariant remains visible to callers.
    let accounted = result
        .persisted
        .min(commit_capacity)
        .saturating_add(requested_skipped)
        .saturating_add(requested_fallback)
        .saturating_add(terminal_failed)
        .saturating_add(terminal_skipped);
    let concurrent_skips = commit_capacity.saturating_sub(accounted);
    if concurrent_skips > 0 {
        progress.skipped = progress.skipped.saturating_add(concurrent_skips);
        *progress
            .reason_counts
            .entry("concurrent_state_changed".into())
            .or_insert(0) += concurrent_skips;
    }
    progress.processed = progress
        .processed
        .saturating_add(commit_capacity)
        .min(progress.total);
    for exclusion in &result.exclusions {
        increment_reason(progress, &backfill_reason_code(&exclusion.reason));
    }
    refresh_backfill_remaining(progress);
}

fn finalize_backfill_progress(
    progress: &mut MemoryBackfillProgress,
    total: usize,
    fatal_stop: bool,
) {
    refresh_backfill_remaining(progress);
    if fatal_stop || progress.fatal_error.is_some() {
        progress.state = "error".into();
        progress.message = format!(
            "{} / {} memories processed; {} persisted; {} failed; {} remaining",
            progress.processed, total, progress.persisted, progress.failed, progress.remaining
        );
        return;
    }

    progress.state = "completed".into();
    progress.message = if progress.failed > 0 || !progress.reason_counts.is_empty() {
        format!(
            "{} memories processed; {} persisted; {} failed (warnings)",
            progress.processed, progress.persisted, progress.failed
        )
    } else {
        format!(
            "{} memories processed; {} persisted",
            progress.processed, progress.persisted
        )
    };
    progress.error = None;
    progress.fatal_error = None;
    progress.remaining = 0;
}

/// A row can enter the backfill queue only when the shared public admission
/// helper accepts its type and redaction-safe source. No allowlist is
/// duplicated here; live capture and retry use the same policy boundary.
fn is_backfill_candidate(row: &StoredMemory) -> bool {
    matches!(
        lance_memory::summary_admission(&row.memory_type, &row.document),
        lance_memory::SummaryAdmission::Eligible
    )
}

struct BackfillPartition {
    candidates: Vec<StoredMemory>,
    skipped_reasons: std::collections::BTreeMap<String, usize>,
    non_candidates: usize,
}

fn should_backfill_row(
    row: &StoredMemory,
    durable_summary_event_ids: &std::collections::HashSet<String>,
) -> bool {
    !durable_summary_event_ids.contains(&MemoryRepository::canonical_event_id(&row.id))
}

fn backfill_skip_reason(
    row: &StoredMemory,
    durable_summary_event_ids: &std::collections::HashSet<String>,
    repairable_summary_event_ids: &std::collections::HashSet<String>,
    deleted_summary_event_ids: &std::collections::HashSet<String>,
    durable_summary_statuses: &std::collections::HashMap<String, SummaryStatusRecord>,
) -> Option<String> {
    let canonical = MemoryRepository::canonical_event_id(&row.id);
    if deleted_summary_event_ids.contains(&canonical) {
        return Some("ユーザーが削除済みの要約のため対象外 (delete tombstone)".into());
    }
    // A legacy or known-invalid automatic Fact is an explicit repair target,
    // even though Fact existence would otherwise trigger the completed/skip
    // branch below.  Confirmed/edited Facts are deliberately absent from this
    // set and remain protected.
    if repairable_summary_event_ids.contains(&canonical) {
        return None;
    }
    let has_summary_fact = !should_backfill_row(row, durable_summary_event_ids);
    let now = Utc::now().to_rfc3339();
    match crate::memory_v2::repository::summary_processing_decision(
        durable_summary_statuses.get(&canonical),
        has_summary_fact,
        SummaryProcessMode::ProcessAll,
        Some(&now),
    ) {
        SummaryProcessDecision::Deleted => {
            Some("ユーザーが削除済みの要約のため対象外 (delete tombstone)".into())
        }
        SummaryProcessDecision::SkipCompleted => {
            Some("既存の確定済み要約があるため対象外 (durable summary exists)".into())
        }
        SummaryProcessDecision::SkipSkipped
        | SummaryProcessDecision::RetryRequired
        | SummaryProcessDecision::LeaseActive => Some(
            durable_summary_statuses
                .get(&canonical)
                .and_then(|status| status.reason.as_deref())
                .unwrap_or("既に終端状態のため対象外 (terminal status)")
                .to_string(),
        ),
        SummaryProcessDecision::Process
        | SummaryProcessDecision::ResumeStalePending
        | SummaryProcessDecision::ReprocessLegacy => None,
        SummaryProcessDecision::SkipLegacy => Some("legacy status is not applicable".into()),
    }
}

#[cfg(test)]
fn partition_backfill_rows(
    memories: &[StoredMemory],
    durable_summary_event_ids: &std::collections::HashSet<String>,
    deleted_summary_event_ids: &std::collections::HashSet<String>,
    durable_summary_statuses: &std::collections::HashMap<String, SummaryStatusRecord>,
) -> BackfillPartition {
    partition_backfill_rows_with_repairable(
        memories,
        durable_summary_event_ids,
        &std::collections::HashSet::new(),
        deleted_summary_event_ids,
        durable_summary_statuses,
    )
}

fn partition_backfill_rows_with_repairable(
    memories: &[StoredMemory],
    durable_summary_event_ids: &std::collections::HashSet<String>,
    repairable_summary_event_ids: &std::collections::HashSet<String>,
    deleted_summary_event_ids: &std::collections::HashSet<String>,
    durable_summary_statuses: &std::collections::HashMap<String, SummaryStatusRecord>,
) -> BackfillPartition {
    let mut candidates = Vec::with_capacity(memories.len());
    let mut skipped = std::collections::BTreeMap::new();
    let mut non_candidates = 0usize;
    for row in memories {
        if !is_backfill_candidate(row) {
            non_candidates = non_candidates.saturating_add(1);
            continue;
        }
        if let Some(reason) = backfill_skip_reason(
            row,
            durable_summary_event_ids,
            repairable_summary_event_ids,
            deleted_summary_event_ids,
            durable_summary_statuses,
        ) {
            *skipped.entry(reason).or_insert(0) += 1;
        } else {
            candidates.push(row.clone());
        }
    }
    BackfillPartition {
        candidates,
        skipped_reasons: skipped,
        non_candidates,
    }
}

async fn persist_backfill_chunk(
    repository: &MemoryRepository,
    root_dir: &std::path::Path,
    summaries: Vec<SummaryBatchInput>,
    skipped_ids: Vec<String>,
    fallback_reasons: Vec<(String, String)>,
) -> Result<SummaryBackfillApplyResult, String> {
    lance_memory::apply_summary_backfill_batch_with_reasons_with_repository(
        repository,
        root_dir,
        summaries,
        skipped_ids,
        fallback_reasons,
    )
    .await
}

#[derive(Clone)]
pub struct SessionManager {
    root_dir: PathBuf,
    ai_client: Arc<AiClient>,
    tts_mgr: Arc<TtsManager>,
    search_client: Arc<WebSearchClient>,
    pub asr_engine: Arc<AsrEngine>,
    audio_input_mgr: Arc<AudioInputManager>,
    events: Arc<Mutex<Vec<SessionEvent>>>,
    is_active: Arc<AtomicBool>,
    auto_commentary_active: Arc<AtomicBool>,
    pub is_collecting_prompt: Arc<AtomicBool>,
    last_speak_time: Arc<Mutex<Instant>>,
    log_mgr: Arc<LogManager>,
    summary_service: Arc<LocalSummaryService>,
    memory_backfill_in_flight: Arc<AtomicBool>,
    pending_event_tasks: Arc<AtomicUsize>,
    event_tasks_idle: Arc<Notify>,
    session_lifecycle: Arc<Mutex<()>>,
    /// Monotonic session epoch. It changes at both Start and Stop so a
    /// callback captured by an older ASR registration cannot cross a boundary.
    session_generation: Arc<AtomicU64>,
    session_id: Arc<Mutex<String>>,
    /// Stopped sessions keep a bounded in-memory archive until their detached
    /// blog task has taken its snapshot. This avoids reading the mutable
    /// current-session ring after a rapid Stop→Start transition.
    session_archives: Arc<Mutex<std::collections::HashMap<String, Vec<SessionEvent>>>>,
    /// Monotonic Start Session boundary used for first-final latency metrics.
    session_started_instant: Arc<Mutex<Option<Instant>>>,
    /// Correlates the readiness-gated Start Session request with its first
    /// finalized ASR latency record.  Direct/native callers leave this empty.
    session_start_request_id: Arc<Mutex<Option<String>>>,
    first_asr_finalized_logged: Arc<AtomicBool>,
    /// UTC boundary captured when Start Session is accepted.  The stop-time
    /// blog fallback uses this to recover only raw rows from the session being
    /// closed, even when the in-memory ring has been cleared or remounted.
    session_started_at: Arc<Mutex<Option<DateTime<Utc>>>>,
}

/// Guard used for every asynchronous event callback that can still append a
/// session event after the stop button is pressed.  Blog generation waits for
/// these guards so its snapshot cannot race the final ASR/Twitch write.
struct EventTaskGuard {
    pending: Arc<AtomicUsize>,
    idle: Arc<Notify>,
}

impl Drop for EventTaskGuard {
    fn drop(&mut self) {
        if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
}

impl SessionManager {
    pub fn new(root_dir: PathBuf, tts_mgr: Arc<TtsManager>, log_mgr: Arc<LogManager>) -> Self {
        let summary_service = Arc::new(LocalSummaryService::new(root_dir.clone()));
        let asr_engine = Arc::new(AsrEngine::new());
        asr_engine.ws_client.set_log_manager(log_mgr.clone());
        Self {
            root_dir,
            ai_client: Arc::new(AiClient::new()),
            tts_mgr,
            search_client: Arc::new(WebSearchClient::new()),
            asr_engine,
            audio_input_mgr: Arc::new(AudioInputManager::new(log_mgr.clone())),
            events: Arc::new(Mutex::new(Vec::new())),
            is_active: Arc::new(AtomicBool::new(false)),
            auto_commentary_active: Arc::new(AtomicBool::new(false)),
            is_collecting_prompt: Arc::new(AtomicBool::new(false)),
            last_speak_time: Arc::new(Mutex::new(Instant::now())),
            log_mgr,
            summary_service,
            memory_backfill_in_flight: Arc::new(AtomicBool::new(false)),
            pending_event_tasks: Arc::new(AtomicUsize::new(0)),
            event_tasks_idle: Arc::new(Notify::new()),
            session_lifecycle: Arc::new(Mutex::new(())),
            session_generation: Arc::new(AtomicU64::new(0)),
            session_id: Arc::new(Mutex::new(String::new())),
            session_archives: Arc::new(Mutex::new(std::collections::HashMap::new())),
            session_started_instant: Arc::new(Mutex::new(None)),
            session_start_request_id: Arc::new(Mutex::new(None)),
            first_asr_finalized_logged: Arc::new(AtomicBool::new(false)),
            session_started_at: Arc::new(Mutex::new(None)),
        }
    }

    fn begin_event_task(&self) -> EventTaskGuard {
        self.pending_event_tasks.fetch_add(1, Ordering::SeqCst);
        EventTaskGuard {
            pending: self.pending_event_tasks.clone(),
            idle: self.event_tasks_idle.clone(),
        }
    }

    /// Wait until every raw-save task spawned by ASR/Twitch callbacks has
    /// released its guard, bounded by [`SESSION_EVENT_DRAIN_TIMEOUT`].
    /// Guards are RAII, so they cannot leak indefinitely; the bound only
    /// keeps a hung backend write from stalling the blog generation forever.
    /// Returns the number of tasks still pending when the bound was hit
    /// (zero means the drain completed cleanly).
    async fn wait_for_event_tasks_bounded(&self) -> usize {
        self.wait_for_event_tasks_with_timeout(SESSION_EVENT_DRAIN_TIMEOUT)
            .await
    }

    async fn wait_for_event_tasks_with_timeout(&self, timeout: std::time::Duration) -> usize {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register interest before checking the counter so a guard that
            // drops between the two steps cannot be missed.
            let notified = self.event_tasks_idle.notified();
            let pending = self.pending_event_tasks.load(Ordering::SeqCst);
            if pending == 0 {
                return 0;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.pending_event_tasks.load(Ordering::SeqCst);
            }
        }
    }

    fn current_session_context(&self) -> Option<SessionContext> {
        if !self.is_active.load(Ordering::SeqCst) {
            return None;
        }
        let session_id = self.session_id.lock().clone();
        let started_at = self.session_started_at.lock().clone()?;
        if session_id.is_empty() {
            return None;
        }
        Some(SessionContext {
            session_id,
            generation: self.session_generation.load(Ordering::SeqCst),
            started_at,
        })
    }

    fn is_current_session(&self, context: &SessionContext) -> bool {
        self.is_active.load(Ordering::SeqCst)
            && self.session_generation.load(Ordering::SeqCst) == context.generation
            && self.session_id.lock().as_str() == context.session_id
    }

    /// A summary that was admitted by a session may finish after Stop.  It is
    /// still useful to notify the dashboard while no replacement session is
    /// active, but it must not leak into a newer session's live Fact stream.
    fn allows_fact_ui_emit(&self, context: &SessionContext) -> bool {
        let _lifecycle_guard = self.session_lifecycle.lock();
        if !self.is_active.load(Ordering::SeqCst) {
            return true;
        }
        self.session_generation.load(Ordering::SeqCst) == context.generation
            && self.session_id.lock().as_str() == context.session_id
    }

    /// Begin one session atomically and return the identity to capture in all
    /// callbacks registered below. Existing events are archived before the
    /// mutable current ring is cleared, so a rapid restart cannot make the
    /// previous blog task read the new session's history.
    fn begin_session(&self) -> Option<SessionContext> {
        let _lifecycle_guard = self.session_lifecycle.lock();
        if self.is_active.swap(true, Ordering::SeqCst) {
            return None;
        }

        let previous_id = std::mem::take(&mut *self.session_id.lock());
        let previous_events = {
            let mut events = self.events.lock();
            std::mem::take(&mut *events)
        };
        if !previous_id.is_empty() && !previous_events.is_empty() {
            self.session_archives
                .lock()
                .insert(previous_id, previous_events);
        }

        let generation = self
            .session_generation
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let session_id = uuid::Uuid::new_v4().to_string();
        let started_at = Utc::now();
        *self.session_id.lock() = session_id.clone();
        *self.session_started_instant.lock() = Some(Instant::now());
        *self.session_start_request_id.lock() = None;
        self.first_asr_finalized_logged
            .store(false, Ordering::SeqCst);
        *self.session_started_at.lock() = Some(started_at);
        *self.last_speak_time.lock() = Instant::now();
        self.is_collecting_prompt.store(false, Ordering::SeqCst);
        self.asr_engine.begin_wake_word_session(generation);

        Some(SessionContext {
            session_id,
            generation,
            started_at,
        })
    }

    fn append_event_to_session(&self, event: SessionEvent, context: Option<&SessionContext>) {
        let _lifecycle_guard = self.session_lifecycle.lock();
        let current_id = self.session_id.lock().clone();
        let target_archive = context
            .filter(|ctx| ctx.session_id != current_id)
            .map(|ctx| ctx.session_id.clone());
        if let Some(session_id) = target_archive {
            let mut archives = self.session_archives.lock();
            let archived = archives.entry(session_id).or_default();
            if archived.iter().any(|item| item.id == event.id) {
                return;
            }
            archived.push(event);
            if archived.len() > 1000 {
                let remove = archived.len() - 1000;
                archived.drain(..remove);
            }
            return;
        }

        let mut events = self.events.lock();
        if events.iter().any(|item| item.id == event.id) {
            return;
        }
        events.push(event);
        if events.len() > 100 {
            events.remove(0);
        }
    }

    fn stop_session_internal(&self) -> Option<SessionContext> {
        let _lifecycle_guard = self.session_lifecycle.lock();
        let context = self.current_session_context();

        // Deactivate first so callbacks racing the capture shutdown are
        // rejected; then stop capture and invalidate the generation. Raw
        // tasks that already passed the callback admission gate are still
        // drained by stop_session_with_services.
        self.is_active.store(false, Ordering::SeqCst);
        self.auto_commentary_active.store(false, Ordering::SeqCst);
        self.audio_input_mgr.stop();
        self.asr_engine.ws_client.reset_audio();
        self.asr_engine.reset_wake_word_on_stop();
        self.is_collecting_prompt.store(false, Ordering::SeqCst);
        self.session_generation.fetch_add(1, Ordering::SeqCst);

        if let Some(ref ctx) = context {
            let mut current_events = self.events.lock();
            let events = std::mem::take(&mut *current_events);
            if !events.is_empty() {
                self.session_archives
                    .lock()
                    .insert(ctx.session_id.clone(), events);
            }
        }
        self.session_id.lock().clear();
        *self.session_started_at.lock() = None;
        *self.session_started_instant.lock() = None;
        *self.session_start_request_id.lock() = None;
        self.tts_mgr.stop_playback();
        self.log_mgr
            .info("Session", "Game Assistant AI Session stopped");
        context
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    pub async fn unload_local_summary_model(&self) {
        self.summary_service.unload().await;
    }

    /// 契約B: private `summary_service` の公開アクセサ。
    pub fn local_summary(&self) -> std::sync::Arc<crate::local_summary::LocalSummaryService> {
        self.summary_service.clone()
    }

    /// Queue an explicit summary retry for a previously terminal attempt.
    /// The raw event remains authoritative and is read back before spawning
    /// the same curation path used for newly captured events.
    pub async fn retry_summary(&self, event_id: &str) -> Result<bool, String> {
        let Some(row) = lance_memory::get_memory_by_event_id(&self.root_dir, event_id).await?
        else {
            return Ok(false);
        };
        if !is_backfill_candidate(&row) {
            return Err("summary retry is unsupported for this event".to_string());
        }
        if !lance_memory::retry_summary(&self.root_dir, &row.id).await? {
            return Ok(false);
        }
        let event = SessionEvent {
            id: row.id,
            r#type: row.memory_type,
            author: row.source,
            content: row.document,
            timestamp: row.timestamp,
        };
        let this = self.clone();
        tokio::spawn(async move {
            this.process_summary_candidate(event, None, None).await;
        });
        Ok(true)
    }

    /// Start an idempotent, non-blocking semantic pass over every raw memory.
    /// Completed summaries are skipped. Stale pending markers (for example
    /// after a crash), legacy/null rows, and projection-only completed rows
    /// are eligible for recovery. Durable terminal outcomes are left alone;
    /// the explicit retry command is the only way to reopen them. The raw
    /// document is never replaced.
    pub async fn start_memory_backfill(
        &self,
        app: AppHandle,
    ) -> Result<MemoryBackfillStart, String> {
        if self.memory_backfill_in_flight.swap(true, Ordering::SeqCst) {
            return Ok(MemoryBackfillStart {
                accepted: false,
                progress: MemoryBackfillProgress {
                    state: "running".into(),
                    processed: 0,
                    total: 0,
                    queued: 0,
                    skipped: 0,
                    failed: 0,
                    persisted: 0,
                    excluded: 0,
                    attempted: 0,
                    retry_count: 0,
                    remaining: 0,
                    reason_counts: std::collections::BTreeMap::new(),
                    fatal_error: None,
                    message: "all-memory semantic processing is already running".into(),
                    error: None,
                },
            });
        }

        // Return immediately so the progress bar appears while the potentially
        // minutes-long memory-v2 journal recovery and candidate scan run in
        // the background; every later update arrives as progress events.
        let initial = MemoryBackfillProgress {
            state: "running".into(),
            processed: 0,
            total: 0,
            queued: 0,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 0,
            attempted: 0,
            retry_count: 0,
            remaining: 0,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "memory-v2インデックスを準備しています...".into(),
            error: None,
        };
        let this = self.clone();
        let task_progress = initial.clone();
        let run_id = format!("backfill-{}", uuid::Uuid::new_v4().simple());
        tauri::async_runtime::spawn(async move {
            let mut progress = task_progress;
            let emit = |payload: &MemoryBackfillProgress| {
                let _ = app.emit("memory-manager-backfill-progress", payload);
            };
            let emit_log = |progress: &MemoryBackfillProgress,
                            severity: BackfillLogSeverity,
                            phase: &str,
                            chunk: usize,
                            event_id: &str,
                            status: &str,
                            reason: &str| {
                let reason_code = backfill_reason_code(reason);
                let message = backfill_log_message_with_counters(
                    &run_id,
                    phase,
                    event_id,
                    chunk,
                    retry_count_for_reason(&reason_code).saturating_add(1),
                    progress.processed,
                    progress.remaining,
                    progress.persisted,
                    progress.skipped,
                    progress.failed,
                    progress.retry_count,
                    status,
                    &reason_code,
                );
                match severity {
                    BackfillLogSeverity::Info => this.log_mgr.info("Memory", &message),
                    BackfillLogSeverity::Warning => this.log_mgr.warn("Memory", &message),
                    BackfillLogSeverity::Error => this.log_mgr.error("Memory", &message),
                }
            };
            let abort = |progress: &mut MemoryBackfillProgress, reason: &str| {
                set_backfill_fatal(progress, reason);
                progress.message = format!("all-memory processing stopped ({})", reason);
            };
            emit(&progress);

            let memories = match lance_memory::list_stored_memories(&this.root_dir).await {
                Ok(result) => result,
                Err(_error) => {
                    abort(&mut progress, "scan_failed");
                    emit_log(
                        &progress,
                        BackfillLogSeverity::Error,
                        "scan",
                        0,
                        "-",
                        "fatal",
                        "scan_failed",
                    );
                    emit(&progress);
                    this.memory_backfill_in_flight
                        .store(false, Ordering::SeqCst);
                    return;
                }
            };
            progress.total = memories.len();
            let total = memories.len();
            refresh_backfill_remaining(&mut progress);
            progress.message = format!("{} 件の記憶をスキャンしました", memories.len());
            emit_log(
                &progress,
                BackfillLogSeverity::Info,
                "scan",
                0,
                "-",
                "scanned",
                "scan_complete",
            );
            emit(&progress);

            // One repository instance is reused for queueing and every chunk
            // persistence; reopening it would recover the whole journal again.
            let repository = match MemoryRepository::open(&this.root_dir).await {
                Ok(repository) => repository,
                Err(_error) => {
                    abort(&mut progress, "repository_open_failed");
                    emit_log(
                        &progress,
                        BackfillLogSeverity::Error,
                        "open",
                        0,
                        "-",
                        "fatal",
                        "repository_open_failed",
                    );
                    emit(&progress);
                    this.memory_backfill_in_flight
                        .store(false, Ordering::SeqCst);
                    return;
                }
            };
            let (
                durable_summary_event_ids,
                repairable_summary_event_ids,
                deleted_summary_event_ids,
                durable_summary_statuses,
            ) = {
                let facts = match repository.read_facts().await {
                    Ok(facts) => facts,
                    Err(_error) => {
                        abort(&mut progress, "summary_fact_read_failed");
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "scan",
                            0,
                            "-",
                            "fatal",
                            "summary_fact_read_failed",
                        );
                        emit(&progress);
                        this.memory_backfill_in_flight
                            .store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let raw_events = match repository.read_raw_events().await {
                    Ok(events) => events,
                    Err(_error) => {
                        abort(&mut progress, "summary_raw_event_read_failed");
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "scan",
                            0,
                            "-",
                            "fatal",
                            "summary_raw_event_read_failed",
                        );
                        emit(&progress);
                        this.memory_backfill_in_flight
                            .store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let statuses = match repository.read_summary_statuses() {
                    Ok(statuses) => statuses,
                    Err(_error) => {
                        abort(&mut progress, "summary_status_read_failed");
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "scan",
                            0,
                            "-",
                            "fatal",
                            "summary_status_read_failed",
                        );
                        emit(&progress);
                        this.memory_backfill_in_flight
                            .store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let raw_events_by_id = raw_events
                    .iter()
                    .map(|event| (event.event_id(), event))
                    .collect::<std::collections::HashMap<_, _>>();
                let durable = facts
                    .iter()
                    .filter(|fact| fact.key().starts_with("summary-"))
                    .map(|fact| fact.source_event_id().to_string())
                    .collect::<std::collections::HashSet<_>>();
                let repairable = facts
                    .iter()
                    .filter(|fact| fact.key().starts_with("summary-"))
                    .filter(|fact| {
                        crate::memory_v2::repository::summary_fact_is_repairable(
                            fact,
                            statuses.get(&fact.source_event_id().to_string()),
                            raw_events_by_id.get(&fact.source_event_id()).copied(),
                        )
                    })
                    .map(|fact| fact.source_event_id().to_string())
                    .collect::<std::collections::HashSet<_>>();
                let deleted = statuses
                    .iter()
                    .filter_map(|(event_id, status)| {
                        (status.status == "deleted").then_some(event_id.clone())
                    })
                    .collect::<std::collections::HashSet<_>>();
                (durable, repairable, deleted, statuses)
            };

            // Admission is applied before queueing. Non-candidates advance
            // scan progress only; they never receive a pending/skipped status,
            // inference request, or vector mutation.
            let partition = partition_backfill_rows_with_repairable(
                &memories,
                &durable_summary_event_ids,
                &repairable_summary_event_ids,
                &deleted_summary_event_ids,
                &durable_summary_statuses,
            );
            let mut candidates = partition.candidates;
            progress.excluded = partition.non_candidates;
            progress.processed = progress
                .processed
                .saturating_add(partition.non_candidates)
                .min(progress.total);
            for (reason, count) in partition.skipped_reasons {
                progress.processed = progress.processed.saturating_add(count).min(progress.total);
                progress.skipped = progress.skipped.saturating_add(count);
                emit_log(
                    &progress,
                    BackfillLogSeverity::Warning,
                    "scan",
                    0,
                    "-",
                    "skipped",
                    &backfill_reason_code(&reason),
                );
            }
            refresh_backfill_remaining(&mut progress);
            if candidates.is_empty() {
                // Every row is excluded, already terminal, or has a durable
                // summary. A row warning does not make this run fatal.
                progress.state = "completed".into();
                progress.message = format!("{} memories processed", memories.len());
                progress.error = None;
                progress.fatal_error = None;
                refresh_backfill_remaining(&mut progress);
                emit_log(
                    &progress,
                    BackfillLogSeverity::Info,
                    "terminal",
                    0,
                    "-",
                    "completed",
                    "scan_complete",
                );
                emit(&progress);
                this.memory_backfill_in_flight
                    .store(false, Ordering::SeqCst);
                return;
            }
            progress.message = format!(
                "{} 件の記憶のうち {} 件を処理します",
                memories.len(),
                candidates.len()
            );
            emit_log(
                &progress,
                BackfillLogSeverity::Info,
                "queue",
                0,
                "-",
                "admitted",
                "candidate_admission",
            );
            emit(&progress);

            match lance_memory::queue_summary_backfill_batch_ids_with_repository(
                &repository,
                &this.root_dir,
                &candidates,
            )
            .await
            {
                Ok(admitted_ids) => {
                    let admitted: std::collections::HashSet<String> =
                        admitted_ids.iter().cloned().collect();
                    let not_admitted = candidates
                        .iter()
                        .filter(|row| !admitted.contains(&row.id))
                        .count();
                    if not_admitted > 0 {
                        // A concurrent terminal transition is already durable
                        // elsewhere; count it as a candidate skip without
                        // writing another status from this run.
                        progress.processed = progress
                            .processed
                            .saturating_add(not_admitted)
                            .min(progress.total);
                        progress.skipped = progress.skipped.saturating_add(not_admitted);
                    }
                    candidates.retain(|row| admitted.contains(&row.id));
                    progress.queued = progress.queued.saturating_add(admitted_ids.len());
                    progress.message = format!(
                        "{} memories queued; running Gemma inference",
                        progress.queued
                    );
                    emit_log(
                        &progress,
                        BackfillLogSeverity::Info,
                        "queue",
                        0,
                        "-",
                        "queued",
                        "queue_complete",
                    );
                }
                Err(_error) => {
                    abort(&mut progress, "queue_failed");
                    progress.message = "バックフィルのキュー登録に失敗しました".into();
                    emit_log(
                        &progress,
                        BackfillLogSeverity::Error,
                        "queue",
                        0,
                        "-",
                        "fatal",
                        "queue_failed",
                    );
                    emit(&progress);
                    this.memory_backfill_in_flight
                        .store(false, Ordering::SeqCst);
                    return;
                }
            };
            refresh_backfill_remaining(&mut progress);
            emit(&progress);
            if candidates.is_empty() {
                progress.state = "completed".into();
                progress.message = format!("{} memories processed", memories.len());
                progress.error = None;
                progress.fatal_error = None;
                refresh_backfill_remaining(&mut progress);
                emit_log(
                    &progress,
                    BackfillLogSeverity::Info,
                    "terminal",
                    0,
                    "-",
                    "completed",
                    "queue_empty",
                );
                emit(&progress);
                this.memory_backfill_in_flight
                    .store(false, Ordering::SeqCst);
                return;
            }

            // Inference remains serial, while persistence is checkpointed in
            // bounded chunks. A row-local warning never aborts a later chunk.
            const BACKFILL_CHUNK_SIZE: usize = 32;
            let mut summaries = Vec::with_capacity(BACKFILL_CHUNK_SIZE);
            let mut skipped_ids = Vec::new();
            let mut fallback_reasons: Vec<(String, String)> = Vec::new();
            let mut finalized_reason_event_ids = std::collections::HashSet::new();
            let mut chunk_size = 0usize;
            let mut chunk_index = 0usize;
            let mut fatal_stop = false;

            for stored in candidates {
                let log_event_id = stored.id.clone();
                let result = this.summarize_backfill_event(stored).await;
                match result {
                    BackfillResult::Completed {
                        summary,
                        warning,
                        retry_count,
                    } => {
                        progress.retry_count = progress.retry_count.saturating_add(retry_count);
                        if let Some(reason) = warning {
                            let _ = record_final_reason(
                                &mut progress,
                                &mut finalized_reason_event_ids,
                                &log_event_id,
                                &reason,
                            );
                            emit_log(
                                &progress,
                                backfill_log_severity("warning"),
                                "inference",
                                chunk_index + 1,
                                &log_event_id,
                                "warning",
                                &reason,
                            );
                        } else {
                            emit_log(
                                &progress,
                                BackfillLogSeverity::Info,
                                "inference",
                                chunk_index + 1,
                                &log_event_id,
                                "completed",
                                "summary_ready",
                            );
                        }
                        summaries.push(summary);
                    }
                    BackfillResult::Skipped {
                        id,
                        reason,
                        retry_count,
                    } => {
                        progress.retry_count = progress.retry_count.saturating_add(retry_count);
                        let is_new_final_row = record_final_reason(
                            &mut progress,
                            &mut finalized_reason_event_ids,
                            &log_event_id,
                            &reason,
                        );
                        if is_new_final_row {
                            skipped_ids.push(id);
                        }
                        emit_log(
                            &progress,
                            backfill_log_severity("skipped"),
                            "inference",
                            chunk_index + 1,
                            &log_event_id,
                            "skipped",
                            &reason,
                        );
                    }
                    BackfillResult::Fallback {
                        id,
                        reason,
                        retry_count,
                    } => {
                        progress.retry_count = progress.retry_count.saturating_add(retry_count);
                        let is_new_final_row = record_final_reason(
                            &mut progress,
                            &mut finalized_reason_event_ids,
                            &log_event_id,
                            &reason,
                        );
                        if is_new_final_row {
                            fallback_reasons.push((id, reason.clone()));
                        }
                        emit_log(
                            &progress,
                            backfill_log_severity("fallback"),
                            "inference",
                            chunk_index + 1,
                            &log_event_id,
                            "fallback",
                            &reason,
                        );
                    }
                    BackfillResult::Fatal {
                        reason,
                        retry_count,
                    } => {
                        progress.retry_count = progress.retry_count.saturating_add(retry_count);
                        set_backfill_fatal(&mut progress, &reason);
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "inference",
                            chunk_index + 1,
                            &log_event_id,
                            "fatal",
                            &reason,
                        );
                        fatal_stop = true;
                        // The current row has no durable terminal outcome.
                        // Any earlier rows in this partial chunk are flushed
                        // below before the run terminates.
                        break;
                    }
                }
                chunk_size = chunk_size.saturating_add(1);
                progress.attempted = progress.attempted.saturating_add(1);
                progress.message = format!(
                    "{}/{} memories attempted",
                    progress.processed.saturating_add(chunk_size),
                    total
                );
                refresh_backfill_remaining(&mut progress);
                emit(&progress);
                if chunk_size < BACKFILL_CHUNK_SIZE {
                    continue;
                }

                chunk_index = chunk_index.saturating_add(1);
                progress.message = "チャンクを永続保存しています...".into();
                emit(&progress);
                let committed_rows = chunk_size;
                let requested_skipped = skipped_ids.len();
                let requested_fallback = fallback_reasons.len();
                match persist_backfill_chunk(
                    &repository,
                    &this.root_dir,
                    std::mem::take(&mut summaries),
                    std::mem::take(&mut skipped_ids),
                    std::mem::take(&mut fallback_reasons),
                )
                .await
                {
                    Ok(result) => {
                        record_backfill_commit(
                            &mut progress,
                            committed_rows,
                            requested_skipped,
                            requested_fallback,
                            &result,
                        );
                        for exclusion in &result.exclusions {
                            let reason = backfill_reason_code(&exclusion.reason);
                            emit_log(
                                &progress,
                                BackfillLogSeverity::Warning,
                                "commit",
                                chunk_index,
                                &exclusion.entity_id,
                                "row_warning",
                                &reason,
                            );
                        }
                        if result.projection_error.is_some() {
                            increment_reason(&mut progress, "projection_resync_failed");
                            emit_log(
                                &progress,
                                BackfillLogSeverity::Warning,
                                "projection",
                                chunk_index,
                                "-",
                                "projection_warning",
                                "projection_resync_failed",
                            );
                        }
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Info,
                            "commit",
                            chunk_index,
                            "-",
                            "committed",
                            "durable_commit",
                        );
                        let _ = app.emit("memory-manager-summary-updated", progress.clone());
                    }
                    Err(_error) => {
                        set_backfill_fatal(&mut progress, "journal_commit_failed");
                        progress.message = "バックフィルの永続化に失敗しました".into();
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "commit",
                            chunk_index,
                            "-",
                            "fatal",
                            "journal_commit_failed",
                        );
                        fatal_stop = true;
                    }
                }
                chunk_size = 0;
                if fatal_stop {
                    break;
                }
            }

            // A runtime fatal can occur after several rows have accumulated in
            // the current chunk. Flush those rows so a later fatal does not
            // erase successful work; the row that raised the fatal remains
            // pending for a future stale-lease retry.
            if chunk_size > 0 {
                chunk_index = chunk_index.saturating_add(1);
                progress.message = "最後のチャンクを永続保存しています...".into();
                emit(&progress);
                let committed_rows = chunk_size;
                let requested_skipped = skipped_ids.len();
                let requested_fallback = fallback_reasons.len();
                match persist_backfill_chunk(
                    &repository,
                    &this.root_dir,
                    std::mem::take(&mut summaries),
                    std::mem::take(&mut skipped_ids),
                    std::mem::take(&mut fallback_reasons),
                )
                .await
                {
                    Ok(result) => {
                        record_backfill_commit(
                            &mut progress,
                            committed_rows,
                            requested_skipped,
                            requested_fallback,
                            &result,
                        );
                        for exclusion in &result.exclusions {
                            let reason = backfill_reason_code(&exclusion.reason);
                            emit_log(
                                &progress,
                                BackfillLogSeverity::Warning,
                                "commit",
                                chunk_index,
                                &exclusion.entity_id,
                                "row_warning",
                                &reason,
                            );
                        }
                        if result.projection_error.is_some() {
                            increment_reason(&mut progress, "projection_resync_failed");
                            emit_log(
                                &progress,
                                BackfillLogSeverity::Warning,
                                "projection",
                                chunk_index,
                                "-",
                                "projection_warning",
                                "projection_resync_failed",
                            );
                        }
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Info,
                            "commit",
                            chunk_index,
                            "-",
                            "committed",
                            "durable_commit",
                        );
                        let _ = app.emit("memory-manager-summary-updated", progress.clone());
                    }
                    Err(_error) => {
                        set_backfill_fatal(&mut progress, "journal_commit_failed");
                        progress.message = "バックフィルの永続化に失敗しました".into();
                        emit_log(
                            &progress,
                            BackfillLogSeverity::Error,
                            "commit",
                            chunk_index,
                            "-",
                            "fatal",
                            "journal_commit_failed",
                        );
                        fatal_stop = true;
                    }
                }
            }

            finalize_backfill_progress(&mut progress, total, fatal_stop);
            if progress.state == "completed" {
                emit_log(
                    &progress,
                    BackfillLogSeverity::Info,
                    "terminal",
                    chunk_index,
                    "-",
                    "completed",
                    if progress.failed > 0 || !progress.reason_counts.is_empty() {
                        "completed_with_warnings"
                    } else {
                        "success"
                    },
                );
            }
            emit(&progress);
            // The compatibility projection is refreshed by the UI once the
            // pass reaches a terminal state. Durable journal commits above are
            // retained even when that projection needs a later resync.
            let _ = app.emit("memory-manager-summary-updated", &progress);
            this.memory_backfill_in_flight
                .store(false, Ordering::SeqCst);
        });
        Ok(MemoryBackfillStart {
            accepted: true,
            progress: initial,
        })
    }

    /// Run Gemma and prepare a durable result without touching LanceDB or the
    /// memory-v2 journal.  Backfill persists these results in one batch after
    /// inference completes, avoiding repeated 500 MB journal recovery.
    async fn summarize_backfill_event(&self, row: StoredMemory) -> BackfillResult {
        let id = row.id.clone();
        let content = match lance_memory::admit_summary_document(&row.memory_type, &row.document) {
            Ok(content) => content,
            Err(lance_memory::SummaryAdmission::NotApplicable) => {
                return BackfillResult::Fatal {
                    reason: "candidate_admission_changed".into(),
                    retry_count: 0,
                };
            }
            Err(lance_memory::SummaryAdmission::Invalid) => {
                return BackfillResult::Fallback {
                    id,
                    reason: "empty_source".into(),
                    retry_count: 0,
                };
            }
            // `admit_summary_document` currently guarantees that Eligible
            // produces Ok, but keep the match exhaustive if that helper's
            // implementation changes independently of this worker.
            Err(lance_memory::SummaryAdmission::Eligible) => {
                return BackfillResult::Fatal {
                    reason: "candidate_admission_invalid".into(),
                    retry_count: 0,
                };
            }
        };
        if content.chars().count() > BACKFILL_MAX_SOURCE_CHARS {
            return BackfillResult::Fallback {
                id,
                reason: "source_too_long".into(),
                retry_count: 0,
            };
        }
        let decision = match self
            .summary_service
            .summarize_event_background(&row.memory_type, &row.source, &row.timestamp, &content)
            .await
        {
            Ok(decision) => decision,
            Err(error) => {
                if let Some(reason) = runtime_failure_reason(&error) {
                    return BackfillResult::Fatal {
                        reason: reason.into(),
                        retry_count: retry_count_for_reason(reason),
                    };
                }
                let reason = inference_failure_reason(&error);
                return BackfillResult::Fallback {
                    id,
                    reason: reason.into(),
                    retry_count: retry_count_for_reason(reason),
                };
            }
        };
        if !decision.should_store {
            return BackfillResult::Skipped {
                id,
                reason: "model_declined".into(),
                retry_count: 0,
            };
        }
        let Some(summary) = decision
            .summary
            .as_deref()
            .and_then(admit_redacted_memory_text)
        else {
            return BackfillResult::Fallback {
                id,
                reason: "invalid_model_output".into(),
                retry_count: 0,
            };
        };
        let (embedding, warning) = match self
            .asr_engine
            .ws_client
            .embed_texts(std::slice::from_ref(&summary))
            .await
        {
            Ok(v)
                if v.first()
                    .map(|vector| vector.len() == lance_memory::VECTOR_DIM as usize)
                    .unwrap_or(false) =>
            {
                (v.into_iter().next(), None)
            }
            _ => (None, Some("embedding_failed".into())),
        };
        BackfillResult::Completed {
            summary: SummaryBatchInput {
                entity_id: id,
                summary,
                embedding,
                attempt_id: Some(format!("attempt-{}", uuid::Uuid::new_v4().simple())),
                model_id: Some(MEMORY_SUMMARY_MODEL_ID.to_string()),
                prompt_version: Some(MEMORY_SUMMARY_PROMPT_VERSION.to_string()),
            },
            warning,
            retry_count: 0,
        }
    }

    pub fn get_effective_gemini_key(&self) -> String {
        let st = crate::settings::load_settings_file(&self.root_dir);
        let mut key = st
            .get("gemini_api_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if key.trim().is_empty() {
            key = std::env::var("GOOGLE_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .unwrap_or_default();
        }
        key.trim().to_string()
    }

    /// Persist a session event and, when an application handle is supplied,
    /// surface a newly accepted Fact to the live dashboard. The handle is
    /// optional so manual retry/backfill paths can reuse the same storage path
    /// without requiring a UI transport.
    pub async fn save_event_to_memory(&self, event: &SessionEvent) {
        let _ = self
            .save_event_to_memory_with_app_context(event, None, None)
            .await;
    }

    pub async fn save_event_to_memory_with_app(
        &self,
        event: &SessionEvent,
        app_handle: Option<AppHandle>,
    ) {
        let _ = self
            .save_event_to_memory_with_app_context(event, app_handle, None)
            .await;
    }

    /// Persist the authoritative raw event while retaining the session context
    /// of the callback that admitted it.  A callback may finish its already
    /// admitted raw write after Stop Session, so this method deliberately does
    /// not reject a stale context; callers gate all follow-up AI/TTS work with
    /// [`is_current_session`].  The boolean reports only the raw durability
    /// boundary, not detached projection, embedding, or summary work.
    async fn save_event_to_memory_with_app_context(
        &self,
        event: &SessionEvent,
        app_handle: Option<AppHandle>,
        context: Option<SessionContext>,
    ) -> bool {
        let _event_guard = self.begin_event_task();
        let Some(doc_text) = admit_redacted_memory_text(&event.content) else {
            return false;
        };

        let st = crate::settings::load_settings_file(&self.root_dir);
        let user_id_val = st
            .get("user_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "User".to_string());

        let mem_item = MemoryItem {
            id: event.id.clone(),
            document: doc_text.clone(),
            memory_type: event.r#type.clone(),
            source: event.author.clone(),
            timestamp: event.timestamp.clone(),
            user_id: Some(user_id_val),
        };
        // §3.4.1: write the redacted raw event immediately.  The nullable
        // vector is intentionally absent; embedding and curation are both
        // background work and cannot delay raw durability.
        match lance_memory::insert_memory_batch_nullable_authoritative(
            &self.root_dir,
            vec![mem_item],
            Some(vec![None]),
        )
        .await
        {
            Ok(_) => {
                self.log_mgr.info(
                    "Memory",
                    &memory_event_log_message(
                        &event.id,
                        &event.r#type,
                        &event.author,
                        &doc_text,
                        "raw_persisted",
                    ),
                );

                // The historical LanceDB row is a compatibility projection of
                // an already-durable raw event.  Run it detached so a live
                // event never waits behind a large Process-all/backfill
                // projection; a projection failure is logged and must never
                // roll back the journal write or the summary queue below.
                {
                    let this = self.clone();
                    let event_id = event.id.clone();
                    let event_type = event.r#type.clone();
                    let author = event.author.clone();
                    let event_timestamp = event.timestamp.clone();
                    let projection_text = doc_text.clone();
                    tokio::spawn(async move {
                        let item = MemoryItem {
                            id: event_id.clone(),
                            document: projection_text.clone(),
                            memory_type: event_type.clone(),
                            source: author.clone(),
                            timestamp: event_timestamp,
                            user_id: None,
                        };
                        if let Err(error) = lance_memory::project_compatibility_rows(
                            &this.root_dir,
                            vec![item],
                            Some(vec![None]),
                        )
                        .await
                        {
                            this.log_mgr.warn(
                                "Memory",
                                &format!(
                                    "{} projection_error={}",
                                    memory_event_log_message(
                                        &event_id,
                                        &event_type,
                                        &author,
                                        &projection_text,
                                        "projection_deferred"
                                    ),
                                    error
                                ),
                            );
                        }
                    });
                }

                // Embedding is deliberately detached from the raw write.  A
                // late result can only fill a non-summary vector.
                let this = self.clone();
                let event_id = event.id.clone();
                let event_type = event.r#type.clone();
                let source = event.author.clone();
                let embed_text = doc_text.clone();
                tokio::spawn(async move {
                    this.embed_and_update_document_vector(
                        &event_id,
                        &event_type,
                        &source,
                        &embed_text,
                    )
                    .await;
                });

                // §3.4.2: enqueue only the finalized ASR speech for live
                // curation without blocking the caller. Older Twitch/raw
                // rows remain pending for an explicit Memory Manager pass.
                if live_asr_summary_event(&event.r#type)
                    && matches!(
                        lance_memory::summary_admission(&event.r#type, &doc_text),
                        lance_memory::SummaryAdmission::Eligible
                    )
                {
                    self.log_mgr.info(
                        "Memory",
                        &memory_event_log_message(
                            &event.id,
                            &event.r#type,
                            &event.author,
                            &doc_text,
                            "summary_queued",
                        ),
                    );
                    let this = self.clone();
                    let app_for_summary = app_handle.clone();
                    let snapshot = SessionEvent {
                        content: doc_text.clone(),
                        ..event.clone()
                    };
                    let summary_context = context.clone();
                    tokio::spawn(async move {
                        this.process_summary_candidate(snapshot, app_for_summary, summary_context)
                            .await;
                    });
                }
                true
            }
            Err(e) => {
                self.log_mgr
                    .error("Memory", &format!("Failed to save event to LanceDB: {}", e));
                false
            }
        }
    }

    async fn embed_and_update_document_vector(
        &self,
        event_id: &str,
        event_type: &str,
        source: &str,
        content: &str,
    ) {
        let Some(content) = admit_redacted_memory_text(content) else {
            return;
        };
        let result = self
            .asr_engine
            .ws_client
            .embed_texts(std::slice::from_ref(&content))
            .await;
        let Some(vector) = result.ok().and_then(|vectors| vectors.into_iter().next()) else {
            self.log_mgr.info(
                "Memory",
                &memory_event_log_message(
                    event_id,
                    event_type,
                    source,
                    &content,
                    "document_embedding_unavailable",
                ),
            );
            return;
        };
        if vector.len() != lance_memory::VECTOR_DIM as usize
            || vector.iter().any(|value| !value.is_finite())
        {
            self.log_mgr.info(
                "Memory",
                &memory_event_log_message(
                    event_id,
                    event_type,
                    source,
                    &content,
                    "document_embedding_invalid",
                ),
            );
            return;
        }
        match lance_memory::update_document_vector(&self.root_dir, event_id, vector).await {
            Ok(true) => self.log_mgr.info(
                "Memory",
                &memory_event_log_message(
                    event_id,
                    event_type,
                    source,
                    &content,
                    "document_vector_updated",
                ),
            ),
            Ok(false) => self.log_mgr.info(
                "Memory",
                &memory_event_log_message(
                    event_id,
                    event_type,
                    source,
                    &content,
                    "document_vector_update_skipped",
                ),
            ),
            Err(error) => self.log_mgr.warn(
                "Memory",
                &format!(
                    "{} error={}",
                    memory_event_log_message(
                        event_id,
                        event_type,
                        source,
                        &content,
                        "document_vector_update_failed"
                    ),
                    error
                ),
            ),
        }
    }

    /// §3.4.4-3.4.6, §3.4.8-3.4.9: background curation for one candidate.
    /// Raw is already persisted; this only adds summary state beside it.
    /// `delete_memory` is never used here; `document` is never replaced.
    async fn process_summary_candidate(
        &self,
        event: SessionEvent,
        app_handle: Option<AppHandle>,
        context: Option<SessionContext>,
    ) {
        if !matches!(
            lance_memory::summary_admission(&event.r#type, &event.content),
            lance_memory::SummaryAdmission::Eligible
        ) {
            return;
        }
        self.process_summary_event(event, app_handle, context).await;
    }

    /// Process one admitted live summary event. Backfill uses the same public
    /// candidate admission and its chunk worker, so no all-types bypass exists.
    async fn process_summary_event(
        &self,
        mut event: SessionEvent,
        app_handle: Option<AppHandle>,
        context: Option<SessionContext>,
    ) {
        let Some(redacted_content) = admit_redacted_memory_text(&event.content) else {
            let _ = lance_memory::mark_summary_fallback(&self.root_dir, &event.id).await;
            return;
        };
        event.content = redacted_content;

        let decision = match self
            .summary_service
            .summarize_event(
                &event.r#type,
                &event.author,
                &event.timestamp,
                &event.content,
            )
            .await
        {
            Ok(decision) => decision,
            Err(error) => {
                match lance_memory::mark_summary_fallback(&self.root_dir, &event.id).await {
                    Ok(_) => self.log_mgr.warn(
                        "Memory",
                        &format!(
                            "{} error={}",
                            memory_event_log_message(
                                &event.id,
                                &event.r#type,
                                &event.author,
                                &event.content,
                                "summary_fallback"
                            ),
                            error
                        ),
                    ),
                    Err(mark_error) => self.log_mgr.warn(
                        "Memory",
                        &format!(
                            "{} error={} status_error={}",
                            memory_event_log_message(
                                &event.id,
                                &event.r#type,
                                &event.author,
                                &event.content,
                                "summary_fallback_failed"
                            ),
                            error,
                            mark_error
                        ),
                    ),
                }
                return;
            }
        };

        if !decision.should_store {
            match lance_memory::mark_summary_skipped(&self.root_dir, &event.id).await {
                Ok(true) => self.log_mgr.info(
                    "Memory",
                    &memory_event_log_message(
                        &event.id,
                        &event.r#type,
                        &event.author,
                        &event.content,
                        "summary_skipped",
                    ),
                ),
                Ok(false) => self.log_mgr.warn(
                    "Memory",
                    &memory_event_log_message(
                        &event.id,
                        &event.r#type,
                        &event.author,
                        &event.content,
                        "summary_skip_unchanged",
                    ),
                ),
                Err(error) => self.log_mgr.warn(
                    "Memory",
                    &format!(
                        "{} error={}",
                        memory_event_log_message(
                            &event.id,
                            &event.r#type,
                            &event.author,
                            &event.content,
                            "summary_skip_failed"
                        ),
                        error
                    ),
                ),
            }
            return;
        }

        let summary = match decision
            .summary
            .as_deref()
            .and_then(admit_redacted_memory_text)
        {
            Some(text) => text,
            _ => {
                match lance_memory::mark_summary_fallback(&self.root_dir, &event.id).await {
                    Ok(_) => self.log_mgr.warn(
                        "Memory",
                        &memory_event_log_message(
                            &event.id,
                            &event.r#type,
                            &event.author,
                            &event.content,
                            "summary_empty_fallback",
                        ),
                    ),
                    Err(error) => self.log_mgr.warn(
                        "Memory",
                        &format!(
                            "{} error={}",
                            memory_event_log_message(
                                &event.id,
                                &event.r#type,
                                &event.author,
                                &event.content,
                                "summary_empty_fallback_failed"
                            ),
                            error
                        ),
                    ),
                }
                return;
            }
        };

        // §3.4.6: embed the summary; on failure keep the document vector.
        let summary_vector: Option<Vec<f32>> = match self
            .asr_engine
            .ws_client
            .embed_texts(&[summary.clone()])
            .await
        {
            Ok(v)
                if v.first()
                    .map(|vec| vec.len() == (lance_memory::VECTOR_DIM as usize))
                    .unwrap_or(false) =>
            {
                v.into_iter().next()
            }
            _ => None,
        };
        match lance_memory::apply_summary(
            &self.root_dir,
            &event.id,
            &summary,
            MEMORY_SUMMARY_MODEL_ID,
            MEMORY_SUMMARY_PROMPT_VERSION,
            summary_vector,
        )
        .await
        {
            Ok(true) => {
                self.log_mgr.info(
                    "Memory",
                    &memory_event_log_message(
                        &event.id,
                        &event.r#type,
                        &event.author,
                        &event.content,
                        "summary_completed",
                    ),
                );

                // A completed local summary is represented by a durable
                // automatic Fact. Notify the live dashboard only after the
                // journal/projection path has accepted it, so the FACT badge
                // never claims persistence for a rejected row.
                let can_emit = context
                    .as_ref()
                    .map(|session| self.allows_fact_ui_emit(session))
                    .unwrap_or(true);
                if let Some(handle) = app_handle.filter(|_| can_emit) {
                    let canonical_event_id = MemoryRepository::canonical_event_id(&event.id);
                    let fact_id =
                        format!("fact:self:summary-{}", canonical_event_id.replace('-', ""));
                    let _ = handle.emit(
                        "memory-fact-created",
                        serde_json::json!({
                            "fact_id": fact_id,
                            "source_event_id": canonical_event_id,
                            "event_id": event.id,
                            "summary": summary,
                            "value": summary,
                            "timestamp": event.timestamp,
                            "source": event.author,
                            "event_type": event.r#type,
                        }),
                    );
                } else if context.is_some() {
                    self.log_mgr.info(
                        "Memory",
                        &format!(
                            "session_id={} generation={} event_id={} status=stale_fact_ui_dropped",
                            context
                                .as_ref()
                                .map(|session| session.session_id.as_str())
                                .unwrap_or_default(),
                            context
                                .as_ref()
                                .map(|session| session.generation)
                                .unwrap_or_default(),
                            event.id
                        ),
                    );
                }
            }
            Ok(false) => self.log_mgr.warn(
                "Memory",
                &memory_event_log_message(
                    &event.id,
                    &event.r#type,
                    &event.author,
                    &event.content,
                    "summary_completion_unchanged",
                ),
            ),
            Err(error) => self.log_mgr.warn(
                "Memory",
                &format!(
                    "{} error={}",
                    memory_event_log_message(
                        &event.id,
                        &event.r#type,
                        &event.author,
                        &event.content,
                        "summary_completion_failed"
                    ),
                    error
                ),
            ),
        }
    }

    /// ローカル GLuCoSE-base-ja 埋め込みモデルを用いたセマンティック記憶検索
    pub async fn get_relevant_memory_context(&self, query_text: &str) -> String {
        let Some(redacted_query) = admit_redacted_memory_text(query_text) else {
            return String::new();
        };
        let hits = if let Ok(q_vecs) = self
            .asr_engine
            .ws_client
            .embed_texts(&[redacted_query])
            .await
        {
            if let Some(first_vec) = q_vecs.first() {
                if first_vec.len() == (lance_memory::VECTOR_DIM as usize) {
                    lance_memory::search_similar_memories(&self.root_dir, first_vec, 5)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let mut mem_docs = Vec::new();
        if !hits.is_empty() {
            // §3.4.10: completed rows surface the summary; raw stays stored.
            for hit in &hits {
                let Some(text) = admit_redacted_memory_text(hit.searchable_text()) else {
                    continue;
                };
                if !text.is_empty() {
                    mem_docs.push(format!("- [{}] {}", hit.item.memory_type, text));
                }
            }
        } else {
            // ベクトル検索結果がない場合は直近の履歴を取得
            if let Ok(mem_res) = lance_memory::list_memories(&self.root_dir, Some(5), Some(0)).await
            {
                for m in &mem_res.memories {
                    // Best effort: prefer a completed summary when present.
                    let display = match lance_memory::get_memory_by_id(&self.root_dir, &m.id).await
                    {
                        Ok(Some(stored))
                            if stored.summary_status.as_deref()
                                == Some(lance_memory::SUMMARY_STATUS_COMPLETED) =>
                        {
                            stored
                                .summary
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .and_then(admit_redacted_memory_text)
                                .or_else(|| admit_redacted_memory_text(&m.document))
                        }
                        _ => admit_redacted_memory_text(&m.document),
                    };
                    if let Some(display) = display.filter(|text| !text.is_empty()) {
                        mem_docs.push(format!("- [{}] {}", m.memory_type, display));
                    }
                }
            }
        }

        if mem_docs.is_empty() {
            String::new()
        } else {
            self.log_mgr.info(
                "Memory",
                &format!(
                    "Retrieved {} relevant semantic memories from LanceDB (GLuCoSE-base-ja)",
                    mem_docs.len()
                ),
            );
            format!("\n\n### 過去の関連記憶:\n{}", mem_docs.join("\n"))
        }
    }

    pub fn start_session(&self) {
        if let Some(context) = self.begin_session() {
            let settings = crate::settings::load_settings_file(&self.root_dir);
            let (wake_word_config, wake_engine_supported) =
                wake_word_config_from_settings(&settings);
            self.asr_engine.configure_wake_word(wake_word_config);
            if !wake_engine_supported {
                self.log_mgr.warn(
                    "ASR",
                    "wake_word_engine is not implemented; using whisper_vad for this session",
                );
            }
            self.log_mgr.info(
                "Session",
                &format!(
                    "Game Assistant AI Session started session_id={} generation={} readiness_id=-",
                    context.session_id, context.generation
                ),
            );
        }
    }

    /// Ensure the persistent ASR worker and its WebSocket are ready before a
    /// microphone stream is attached.  Startup warmup normally makes this a
    /// fast no-op; the explicit wait keeps a user click during warmup from
    /// buffering audio behind a still-loading Whisper model.
    pub async fn ensure_asr_ready(&self) -> Result<(), String> {
        self.asr_engine.ws_client.warmup().await
    }

    fn asr_clock_ms(&self, context: &SessionContext) -> u64 {
        self.session_started_instant
            .lock()
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or_else(|| {
                Utc::now()
                    .signed_duration_since(context.started_at)
                    .num_milliseconds()
                    .max(0) as u64
            })
    }

    fn emit_asr_result(
        app_handle: Option<&AppHandle>,
        display_text: &str,
        stream: &str,
        is_final: bool,
        is_prompt: bool,
        latency_ms: Option<f64>,
        event_id: Option<&str>,
    ) {
        if let Some(handle) = app_handle {
            let _ = handle.emit(
                "asr_result",
                serde_json::json!({
                    "text": display_text,
                    "is_final": is_final,
                    "stream": stream,
                    "is_prompt": is_prompt,
                    "latency_ms": latency_ms,
                    "event_id": event_id,
                }),
            );
        }
    }

    fn handle_partial_asr_result(
        &self,
        context: &SessionContext,
        stream: &str,
        text: &str,
        latency_ms: Option<f64>,
        app_handle: Option<&AppHandle>,
    ) -> WakeWordDecision {
        let detector_is_current = self.is_current_session(context);
        let decision = if detector_is_current {
            self.asr_engine
                .handle_wake_word(stream, text, false, self.asr_clock_ms(context))
        } else {
            stale_wake_word_decision(stream, text, false, context.generation)
        };
        let collecting = prompt_collection_from_decision(&decision);
        if detector_is_current && stream == "mic" {
            self.is_collecting_prompt
                .store(collecting, Ordering::SeqCst);
        }
        let display_text = if stream == "discord" {
            format!("[Discord] {}", text)
        } else {
            text.to_string()
        };
        Self::emit_asr_result(
            app_handle,
            &display_text,
            stream,
            false,
            decision.is_prompt,
            latency_ms,
            None,
        );
        self.log_mgr.info(
            "ASR",
            &format!(
                "session_id={} generation={} {}",
                context.session_id,
                context.generation,
                wake_decision_log_message("-", stream, text, &decision, collecting)
            ),
        );
        decision
    }

    async fn persist_final_asr_result(
        &self,
        context: &SessionContext,
        stream: &str,
        text: &str,
        latency_ms: Option<f64>,
        app_handle: Option<&AppHandle>,
    ) -> Option<PersistedAsrEvent> {
        let event_id = uuid::Uuid::new_v4().to_string();
        let detector_is_current = self.is_current_session(context);
        let decision = if detector_is_current {
            self.asr_engine
                .handle_wake_word(stream, text, true, self.asr_clock_ms(context))
        } else {
            // Preserve the already-admitted raw event without letting a late
            // final callback mutate the detector belonging to a newer session.
            stale_wake_word_decision(stream, text, true, context.generation)
        };
        let collecting = prompt_collection_from_decision(&decision);
        if detector_is_current && stream == "mic" {
            self.is_collecting_prompt
                .store(collecting, Ordering::SeqCst);
        }
        let display_text = if stream == "discord" {
            format!("[Discord] {}", text)
        } else {
            text.to_string()
        };

        if self.is_current_session(context)
            && !self.first_asr_finalized_logged.swap(true, Ordering::SeqCst)
        {
            let wait_ms = self
                .session_started_instant
                .lock()
                .map(|started| started.elapsed().as_millis())
                .unwrap_or_default();
            let readiness_id = self
                .session_start_request_id
                .lock()
                .clone()
                .unwrap_or_else(|| "-".to_string());
            self.log_mgr.info(
                "ASR",
                &format!(
                    "session_id={} generation={} first_asr_finalized phase=session readiness_id={} event_id={} stream={} wait_ms={}",
                    context.session_id,
                    context.generation,
                    readiness_id,
                    event_id,
                    stream,
                    wait_ms
                ),
            );
        }
        self.log_mgr.info(
            "ASR",
            &format!(
                "session_id={} generation={} {} latency_ms={:?}",
                context.session_id,
                context.generation,
                memory_event_log_message(
                    &event_id,
                    &format!("{}_transcription", stream),
                    stream,
                    &display_text,
                    "finalized"
                ),
                latency_ms
            ),
        );
        self.log_mgr.info(
            "ASR",
            &format!(
                "session_id={} generation={} {}",
                context.session_id,
                context.generation,
                wake_decision_log_message(&event_id, stream, text, &decision, collecting)
            ),
        );
        if self.is_current_session(context) {
            Self::emit_asr_result(
                app_handle,
                &display_text,
                stream,
                true,
                decision.is_prompt,
                latency_ms,
                Some(event_id.as_str()),
            );
        } else {
            self.log_mgr.info(
                "Session",
                &format!(
                    "session_id={} generation={} event_id={} status=stale_final_ui_dropped",
                    context.session_id, context.generation, event_id
                ),
            );
        }

        let (event_type, author) = match stream {
            "mic" => ("user_speech", "User"),
            "discord" => ("discord_speech", "Discord"),
            _ => return None,
        };
        let content = admit_redacted_memory_text(text)?;
        let event = SessionEvent {
            id: event_id,
            r#type: event_type.to_string(),
            author: author.to_string(),
            content,
            timestamp: Local::now().to_rfc3339(),
        };

        self.append_event_to_session(event.clone(), Some(context));
        if !self
            .save_event_to_memory_with_app_context(
                &event,
                app_handle.cloned(),
                Some(context.clone()),
            )
            .await
        {
            self.log_mgr.warn(
                "Memory",
                &format!(
                    "session_id={} generation={} event_id={} status=raw_persist_failed",
                    context.session_id, context.generation, event.id
                ),
            );
            return None;
        }
        if self.is_current_session(context) {
            if let Some(handle) = app_handle {
                let _ = handle.emit("session-event", &event);
            }
        } else {
            self.log_mgr.info(
                "Session",
                &format!(
                    "session_id={} generation={} event_id={} status=stale_session_event_dropped",
                    context.session_id, context.generation, event.id
                ),
            );
        }
        Some(PersistedAsrEvent {
            stop_word_detected: stream == "mic" && contains_stop_word(text),
            event,
            decision,
        })
    }

    async fn process_asr_followup(
        &self,
        context: &SessionContext,
        result: PersistedAsrEvent,
        original_text: &str,
        app_handle: Option<&AppHandle>,
    ) {
        if !self.is_current_session(context) {
            self.log_mgr.info(
                "Session",
                &format!(
                    "session_id={} generation={} event_id={} status=stale_followup_dropped",
                    context.session_id, context.generation, result.event.id
                ),
            );
            return;
        }
        if result.stop_word_detected {
            self.tts_mgr.stop_playback();
            self.asr_engine.reset_wake_word_on_stop();
            self.is_collecting_prompt.store(false, Ordering::SeqCst);
            self.log_mgr.info(
                "ASR",
                &format!(
                    "session_id={} generation={} event_id={} status=stop_word_detected",
                    context.session_id, context.generation, result.event.id
                ),
            );
            return;
        }

        let prompt = match result.decision.action {
            WakeWordAction::PromptDetected | WakeWordAction::PromptReceived
                if result.decision.is_prompt =>
            {
                result.decision.clean_prompt.clone()
            }
            _ => String::new(),
        };
        if result.decision.should_acknowledge {
            let _ = self.tts_mgr.play_random_nod(&self.root_dir).await;
            if !self.is_current_session(context) {
                return;
            }
        }
        if result.decision.action == WakeWordAction::FinalWakeOnly {
            self.is_collecting_prompt.store(true, Ordering::SeqCst);
            return;
        }
        if prompt.chars().count() < 2 {
            if result.decision.action == WakeWordAction::PromptReceived {
                self.is_collecting_prompt.store(false, Ordering::SeqCst);
            }
            return;
        }

        let st_file = crate::settings::load_settings_file(&self.root_dir);
        let gemini_key = self.get_effective_gemini_key();
        let brave_key = st_file
            .get("brave_api_key")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| std::env::var("BRAVE_API_KEY").unwrap_or_default());
        let model = st_file
            .get("gemini_model")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".to_string())
            });
        let sys_prompt = crate::prompts::get_prompt(&self.root_dir, "system_instruction_character");
        let tts_cfg = extract_tts_settings(&st_file);
        self.log_mgr.info(
            "ASR",
            &format!(
                "session_id={} generation={} event_id={} status={} prompt_chars={}",
                context.session_id,
                context.generation,
                result.event.id,
                match result.decision.action {
                    WakeWordAction::PromptDetected => "prompt_detected",
                    _ => "prompt_received",
                },
                prompt.chars().count()
            ),
        );
        if let Err(error) = self
            .process_user_input_for_session(
                context,
                &result.event,
                &prompt,
                &gemini_key,
                &brave_key,
                &model,
                &sys_prompt,
                &tts_cfg,
                app_handle,
            )
            .await
        {
            self.log_mgr.error(
                "AI",
                &format!(
                    "session_id={} generation={} event_id={} Gemini process error: {}",
                    context.session_id, context.generation, result.event.id, error
                ),
            );
        }
        let _ = original_text;
    }

    pub fn start_session_with_services(
        self: &Arc<Self>,
        app_handle: Option<AppHandle>,
        twitch_service: Option<Arc<crate::twitch::TwitchService>>,
    ) {
        self.start_session_with_services_for_request(app_handle, twitch_service, None);
    }

    /// Start a session after a readiness-gated request.  The request identity
    /// is carried into the first finalized-ASR log so startup latency can be
    /// joined without relying on wall-clock ordering.
    pub fn start_session_with_request_id(
        self: &Arc<Self>,
        app_handle: Option<AppHandle>,
        twitch_service: Option<Arc<crate::twitch::TwitchService>>,
        request_id: String,
    ) {
        self.start_session_with_services_for_request(app_handle, twitch_service, Some(request_id));
    }

    fn start_session_with_services_for_request(
        self: &Arc<Self>,
        app_handle: Option<AppHandle>,
        twitch_service: Option<Arc<crate::twitch::TwitchService>>,
        request_id: Option<String>,
    ) {
        let Some(session_context) = self.begin_session() else {
            return;
        };
        *self.session_start_request_id.lock() = request_id;
        let readiness_id = self
            .session_start_request_id
            .lock()
            .clone()
            .unwrap_or_else(|| "-".to_string());
        self.log_mgr.info(
            "Session",
            &format!(
                "Game Assistant AI Session started session_id={} generation={} readiness_id={}",
                session_context.session_id, session_context.generation, readiness_id
            ),
        );

        let settings = crate::settings::load_settings_file(&self.root_dir);
        let (wake_word_config, wake_engine_supported) = wake_word_config_from_settings(&settings);
        self.asr_engine.configure_wake_word(wake_word_config);
        if !wake_engine_supported {
            self.log_mgr.warn(
                "ASR",
                "wake_word_engine is not implemented; using whisper_vad for this session",
            );
            if let Some(ref handle) = app_handle {
                let _ = handle.emit(
                    "toast_notice",
                    serde_json::json!({
                        "message": "⚠️ 選択されたWake Wordエンジンは未実装のため、Whisper VADを使用します。",
                        "type": "warning"
                    }),
                );
            }
        }

        // 1. Twitch サービス連携
        if let Some(twitch_svc) = twitch_service {
            let twitch_channel = settings
                .get("twitch_channel")
                .or_else(|| settings.get("twitch_bot_channel"))
                .or_else(|| settings.get("user_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            let twitch_bot_username = settings
                .get("twitch_bot_username")
                .and_then(|v| v.as_str())
                .unwrap_or("justinfan12345")
                .trim()
                .to_string();

            let twitch_bot_token = settings
                .get("twitch_access_token")
                .or_else(|| settings.get("twitch_bot_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            if !twitch_channel.is_empty() {
                let log_mgr_twitch = self.log_mgr.clone();
                let app_h = app_handle.clone();
                let session_self = self.clone();
                let twitch_context = session_context.clone();

                log_mgr_twitch.info(
                    "Twitch",
                    &format!(
                        "Logging in as '{}' to channel '{}'...",
                        twitch_bot_username, twitch_channel
                    ),
                );

                tauri::async_runtime::spawn(async move {
                    let bot_settings = crate::twitch::TwitchBotSettings {
                        channel: twitch_channel.clone(),
                        bot_nick: twitch_bot_username.clone(),
                        oauth_token: twitch_bot_token,
                    };

                    let session_for_msg = session_self.clone();
                    let app_for_msg = app_h.clone();
                    let context_for_msg = twitch_context.clone();
                    let on_msg = Arc::new(move |msg: crate::twitch::TwitchChatMessage| {
                        let sess = session_for_msg.clone();
                        let app_m = app_for_msg.clone();
                        let context = context_for_msg.clone();
                        if !sess.is_current_session(&context) {
                            sess.log_mgr.info(
                                "Session",
                                &format!(
                                    "session_id={} generation={} status=stale_twitch_callback_dropped",
                                    context.session_id, context.generation
                                ),
                            );
                            return;
                        }
                        let task_guard = sess.begin_event_task();
                        tauri::async_runtime::spawn(async move {
                            let _task_guard = task_guard;
                            // Twitch メッセージをイベント＆LanceDB に保存
                            let tw_event = SessionEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                r#type: "twitch_chat".to_string(),
                                author: msg.author.clone(),
                                content: msg.content.clone(),
                                timestamp: Local::now().to_rfc3339(),
                            };
                            sess.append_event_to_session(tw_event.clone(), Some(&context));
                            let persisted = sess
                                .save_event_to_memory_with_app_context(
                                    &tw_event,
                                    app_m.clone(),
                                    Some(context.clone()),
                                )
                                .await;
                            // The raw Twitch event is durable now; do not
                            // hold the stop-drain gate across Gemini/TTS work.
                            drop(_task_guard);
                            if !persisted || !sess.is_current_session(&context) {
                                return;
                            }

                            let st = crate::settings::load_settings_file(&sess.root_dir);
                            let gemini_key = sess.get_effective_gemini_key();
                            let brave_key = st
                                .get("brave_api_key")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let model = st
                                .get("gemini_model")
                                .and_then(|v| v.as_str())
                                .unwrap_or("gemini-2.0-flash")
                                .to_string();
                            let sys_prompt = crate::prompts::get_prompt(
                                &sess.root_dir,
                                "system_instruction_character",
                            );
                            let tts_cfg = extract_tts_settings(&st);

                            let _ = sess
                                .process_user_input_for_session(
                                    &context,
                                    &tw_event,
                                    &msg.content,
                                    &gemini_key,
                                    &brave_key,
                                    &model,
                                    &sys_prompt,
                                    &tts_cfg,
                                    app_m.as_ref(),
                                )
                                .await;
                        });
                    });

                    if let Err(e) = twitch_svc.connect(bot_settings, app_h, Some(on_msg)).await {
                        log_mgr_twitch.error("Twitch", &format!("Twitch connection error: {}", e));
                    } else {
                        log_mgr_twitch.info(
                            "Twitch",
                            &format!(
                                "Connected to Twitch channel '{}' successfully",
                                twitch_channel
                            ),
                        );
                    }
                });
            }
        }

        // 2. 音声入力 & Faster-Whisper GPU IPC ワーカーの起動 & コールバック登録
        let audio_device = settings
            .get("audio_device")
            .and_then(|v| v.as_str())
            .unwrap_or("Default")
            .to_string();
        let session_for_callback = self.clone();
        let app_for_callback = app_handle.clone();
        let log_mgr_callback = self.log_mgr.clone();
        let callback_context = session_context.clone();

        if let Err(e) = self.asr_engine.ws_client.start(
            move |stream: String, text: String, is_final: bool, latency_ms: Option<f64>| {
                let session_cl = session_for_callback.clone();
                let app_cl = app_for_callback.clone();
                let log_cl = log_mgr_callback.clone();
                let context = callback_context.clone();
                if !session_cl.is_current_session(&context) {
                    log_cl.info(
                        "Session",
                        &format!(
                            "session_id={} generation={} status=stale_asr_callback_dropped",
                            context.session_id, context.generation
                        ),
                    );
                    return;
                }
                let task_guard = session_cl.begin_event_task();

                tauri::async_runtime::spawn(async move {
                    let _task_guard = task_guard;
                    // Partials are provisional and must be discarded once the
                    // session boundary changes.  A finalized callback admitted
                    // while the session was active is different: its raw write
                    // must still complete so Stop Session can drain it.
                    if !is_final && !session_cl.is_current_session(&context) {
                        log_cl.info(
                            "Session",
                            &format!(
                                "session_id={} generation={} status=stale_asr_task_dropped",
                                context.session_id, context.generation
                            ),
                        );
                        return;
                    }

                    // The detector is the sole owner of partial/final wake state.
                    // A partial can request one acknowledgement, but only a
                    // final result is allowed to persist or reach Gemini.
                    if !is_final {
                        let decision = session_cl.handle_partial_asr_result(
                            &context,
                            &stream,
                            &text,
                            latency_ms,
                            app_cl.as_ref(),
                        );
                        drop(_task_guard);
                        if decision.should_acknowledge && session_cl.is_current_session(&context) {
                            let _ = session_cl
                                .tts_mgr
                                .play_random_nod(&session_cl.root_dir)
                                .await;
                        }
                        return;
                    }

                    let persisted = session_cl
                        .persist_final_asr_result(
                            &context,
                            &stream,
                            &text,
                            latency_ms,
                            app_cl.as_ref(),
                        )
                        .await;
                    // Stop Session waits only for the raw-save portion. Prompt
                    // handling, Gemini and TTS must not extend the drain window.
                    drop(_task_guard);
                    if let Some(result) = persisted {
                        session_cl
                            .process_asr_followup(&context, result, &text, app_cl.as_ref())
                            .await;
                    }
                    return;
                });
            },
        ) {
            self.log_mgr.error(
                "ASR",
                &format!("Failed to start Faster-Whisper GPU worker: {}", e),
            );
        }

        // マイク音声ストリーム開始 -> GPU WebSocket へサンプルを即座にパイプ
        let ws_for_mic = self.asr_engine.ws_client.clone();
        let _ = self.audio_input_mgr.start_mic_stream(
            Some(audio_device),
            app_handle.clone(),
            move |samples: Vec<f32>| {
                ws_for_mic.send_audio("mic", &samples);
            },
        );

        // 3. Discord 音声ループバックキャプチャ＆文字起こし開始 (オプション)
        let enable_discord = settings
            .get("enable_discord_capture")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enable_discord {
            let discord_device = settings
                .get("discord_audio_device")
                .and_then(|v| v.as_str())
                .unwrap_or("Default")
                .to_string();
            let ws_for_discord = self.asr_engine.ws_client.clone();

            let _ = self.audio_input_mgr.start_discord_stream(
                Some(discord_device),
                move |samples: Vec<f32>| {
                    ws_for_discord.send_audio("discord", &samples);
                },
            );
        }

        // 4. 自動ツッコミ・実況ループ (Auto Commentary Loop)
        self.auto_commentary_active.store(true, Ordering::SeqCst);
        let session_for_comm = self.clone();
        let app_for_comm = app_handle.clone();
        let log_mgr_comm = self.log_mgr.clone();
        let commentary_context = session_context.clone();

        tauri::async_runtime::spawn(async move {
            log_mgr_comm.info(
                "Commentary",
                "Autonomous Live Commentary & Visual Context engine activated",
            );

            while session_for_comm.is_current_session(&commentary_context)
                && session_for_comm
                    .auto_commentary_active
                    .load(Ordering::SeqCst)
            {
                let st = crate::settings::load_settings_file(&session_for_comm.root_dir);
                let enable_auto = st
                    .get("enable_auto_commentary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if !enable_auto {
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }

                let min_sec = st
                    .get("auto_commentary_min")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(200);
                let max_sec = st
                    .get("auto_commentary_max")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(400)
                    .max(min_sec);
                let avoid_dur = st
                    .get("auto_commentary_avoid_duration")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5);

                let cycle_sec = {
                    let mut rng = rand::thread_rng();
                    use rand::Rng;
                    rng.gen_range(min_sec..=max_sec)
                };

                log_mgr_comm.info(
                    "Commentary",
                    &format!(
                        "Next autonomous commentary scheduled in {} seconds",
                        cycle_sec
                    ),
                );

                let start_time = Instant::now();

                while start_time.elapsed().as_secs() < cycle_sec {
                    if !session_for_comm.is_current_session(&commentary_context)
                        || !session_for_comm
                            .auto_commentary_active
                            .load(Ordering::SeqCst)
                    {
                        return;
                    }
                    let elapsed = start_time.elapsed().as_secs();
                    let remaining = cycle_sec.saturating_sub(elapsed);

                    if let Some(ref handle) = app_for_comm {
                        let _ = handle.emit(
                            "auto_commentary_status",
                            serde_json::json!({
                                "is_running": true,
                                "remaining_sec": remaining,
                                "total_sec": cycle_sec
                            }),
                        );
                    }

                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }

                // 割り込み回避チェック（誰かが直前に発話中またはTTS再生中か）
                loop {
                    if !session_for_comm.is_current_session(&commentary_context)
                        || !session_for_comm
                            .auto_commentary_active
                            .load(Ordering::SeqCst)
                    {
                        return;
                    }

                    let elapsed_since_last_speak =
                        session_for_comm.last_speak_time.lock().elapsed().as_secs();
                    if elapsed_since_last_speak >= avoid_dur {
                        break;
                    }

                    log_mgr_comm.info(
                        "Commentary",
                        &format!(
                            "Speech activity detected, delaying commentary by {}s...",
                            avoid_dur
                        ),
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(avoid_dur)).await;
                }

                let gemini_key = session_for_comm.get_effective_gemini_key();
                let model = st
                    .get("gemini_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gemini-2.0-flash")
                    .to_string();
                let tts_cfg = extract_tts_settings(&st);

                if !session_for_comm.is_current_session(&commentary_context) {
                    return;
                }
                let _ = session_for_comm
                    .execute_auto_commentary_for_session(
                        &commentary_context,
                        &gemini_key,
                        &model,
                        &tts_cfg,
                        app_for_comm.as_ref(),
                    )
                    .await;
            }
        });
    }

    pub fn stop_session(&self) {
        let _ = self.stop_session_internal();
    }

    /// Final process teardown used by the Tauri exit hook. Normal session
    /// stops intentionally keep ASR warm for the next session.
    pub fn shutdown(&self) {
        self.stop_session();
        self.asr_engine.ws_client.stop();
    }

    pub fn stop_session_with_services(
        &self,
        twitch_service: Option<&crate::twitch::TwitchService>,
        app_handle: Option<AppHandle>,
    ) {
        let stopped_context = self.stop_session_internal();
        if let Some(twitch) = twitch_service {
            twitch.disconnect();
        }

        let st = crate::settings::load_settings_file(&self.root_dir);
        let create_blog = automatic_blog_post_enabled(&st);

        if create_blog {
            let Some(stopped_context) = stopped_context else {
                self.log_mgr.info(
                    "Blog",
                    "ブログ記事生成をスキップしました: activeなセッションがありません",
                );
                return;
            };
            let session_clone = self.clone();
            let app_h = app_handle;
            let log_mgr = self.log_mgr.clone();
            // Capture both boundaries before a subsequent Start Session can
            // overwrite shared state while this detached blog task runs.
            let session_id = Some(stopped_context.session_id.clone());
            let session_started_at = Some(stopped_context.started_at);

            tauri::async_runtime::spawn(async move {
                // ASR/Twitch callbacks are detached from the stop command.
                // Drain their event writes before taking the blog snapshot so
                // the final utterance cannot disappear from the article. The
                // wait is bounded: guards are RAII, so a timeout here means a
                // hung backend write, and generation still proceeds from the
                // already durable events.
                log_mgr.info(
                    "Blog",
                    "セッション停止後のイベント保存完了を待っています (最大10秒)...",
                );
                let outstanding = session_clone.wait_for_event_tasks_bounded().await;
                if outstanding > 0 {
                    log_mgr.warn(
                        "Blog",
                        &format!(
                            "イベント保存の待機がタイムアウトしました (未完了タスク {}件)。保存済みイベントのみでブログ生成を開始します。",
                            outstanding
                        ),
                    );
                    if let Some(ref h) = app_h {
                        let _ = h.emit(
                            "toast_notice",
                            serde_json::json!({
                                "message": "⚠️ 一部の発話の保存待ちがタイムアウトしました。保存済みの内容でブログを生成します。",
                                "type": "warning"
                            }),
                        );
                    }
                } else {
                    log_mgr.info(
                        "Blog",
                        "イベント保存が完了しました。ブログ生成を開始します。",
                    );
                }

                let gemini_key = session_clone.get_effective_gemini_key();
                let st_file = crate::settings::load_settings_file(&session_clone.root_dir);
                let model = st_file
                    .get("gemini_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gemini-2.0-flash")
                    .to_string();
                let blog_prompt = crate::prompts::get_prompt(
                    &session_clone.root_dir,
                    "blog_writer_system_prompt",
                );

                log_mgr.info(
                    "Blog",
                    "自動ブログ記事生成を開始します (create_blog_post: true)...",
                );
                if let Some(ref h) = app_h {
                    let _ = h.emit("toast_notice", serde_json::json!({
                        "message": "📝 セッション終了を検知しました。AIがnoteブログ記事を自動執筆中...",
                        "type": "info"
                    }));
                }

                if gemini_key.trim().is_empty() {
                    // No blog call will consume the stopped-session archive;
                    // raw events are already durable, so release this bounded
                    // in-memory copy on the key-missing path as well.
                    if let Some(ref id) = session_id {
                        session_clone.session_archives.lock().remove(id);
                    }
                    log_mgr.error(
                        "Blog",
                        "ブログ記事を生成できません: Gemini API キーが未設定です (設定または GEMINI_API_KEY を確認してください)",
                    );
                    if let Some(ref h) = app_h {
                        let _ = h.emit(
                            "toast_notice",
                            serde_json::json!({
                                "message": "⚠️ Gemini APIキーが未設定のため、ブログ記事を生成できませんでした。設定画面でAPIキーを保存してください。",
                                "type": "warning"
                            }),
                        );
                    }
                    return;
                }

                match session_clone
                    .generate_blog_article_for_session(
                        &gemini_key,
                        &model,
                        &blog_prompt,
                        session_started_at,
                        session_id,
                    )
                    .await
                {
                    Ok(article) => {
                        let blogs_dir = session_clone.root_dir.join("blogs");
                        let _ = std::fs::create_dir_all(&blogs_dir);
                        let stamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
                        let filepath = unique_blog_path(&blogs_dir, &stamp);
                        let filename = filepath
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| format!("{}.md", stamp));

                        if let Err(e) = std::fs::write(&filepath, &article) {
                            log_mgr.error(
                                "Blog",
                                &format!("ブログ記事のファイル書き込みに失敗しました: {}", e),
                            );
                            if let Some(ref h) = app_h {
                                let _ = h.emit(
                                    "toast_notice",
                                    serde_json::json!({
                                        "message": format!("⚠️ ブログ記事の保存に失敗しました: {}", e),
                                        "type": "warning"
                                    }),
                                );
                            }
                        } else {
                            log_mgr.info(
                                "Blog",
                                &format!("✅ ブログ記事を自動保存しました: {:?}", filepath),
                            );
                            if let Some(ref h) = app_h {
                                let _ = h.emit("toast_notice", serde_json::json!({
                                    "message": format!("✅ ブログ記事を自動保存しました！ (blogs/{})", filename),
                                    "type": "success"
                                }));
                            }
                        }
                    }
                    Err(e) => {
                        log_mgr.error("Blog", &format!("ブログ記事の自動生成エラー: {}", e));
                        if let Some(ref h) = app_h {
                            let _ = h.emit(
                                "toast_notice",
                                serde_json::json!({
                                    "message": format!("⚠️ ブログ記事の生成に失敗しました: {}", e),
                                    "type": "warning"
                                }),
                            );
                        }
                    }
                }
            });
        } else {
            // Stop still archives the in-memory ring before the bounded raw
            // drain.  When automatic blog generation is explicitly disabled
            // there is no later blog task that can consume that archive, so
            // release it immediately; authoritative raw rows remain durable
            // in LanceDB and are the source for any future manual export.
            if let Some(context) = stopped_context {
                self.session_archives.lock().remove(&context.session_id);
            }
            self.log_mgr.info(
                "Blog",
                "自動ブログ記事生成をスキップしました: create_blog_post=false (Settings > Blog & Skills で再度有効化できます)",
            );
        }
    }

    pub fn get_events(&self) -> Vec<SessionEvent> {
        self.events.lock().clone()
    }

    /// イベント追加
    pub fn add_event(&self, event: SessionEvent) {
        self.append_event_to_session(event, None);
    }

    /// 自立型ツッコミ・実況の実行 (指示文はUI/履歴に載せず、純粋なツッコミのみを生成・保存・発話)
    pub async fn execute_auto_commentary(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        self.execute_auto_commentary_inner(
            None,
            gemini_api_key,
            gemini_model,
            tts_settings,
            app_handle,
        )
        .await
    }

    async fn execute_auto_commentary_for_session(
        &self,
        context: &SessionContext,
        gemini_api_key: &str,
        gemini_model: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        self.execute_auto_commentary_inner(
            Some(context),
            gemini_api_key,
            gemini_model,
            tts_settings,
            app_handle,
        )
        .await
    }

    async fn execute_auto_commentary_inner(
        &self,
        context: Option<&SessionContext>,
        gemini_api_key: &str,
        gemini_model: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        self.log_mgr.info(
            "Commentary",
            "Generating autonomous live commentary on current gameplay...",
        );

        let st = crate::settings::load_settings_file(&self.root_dir);
        let sys_prompt = crate::prompts::get_prompt(&self.root_dir, "auto_commentary_prompt");

        // LanceDB 関連記憶をセマンティック検索 (直近の会話またはゲーム状況)
        let memory_context = self
            .get_relevant_memory_context("ゲームプレイ状況 実況 解説")
            .await;
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        // 直近の会話履歴（最大 10 件）
        let events = self.get_events();
        let mut session_history = String::new();
        for ev in events.iter().rev().take(10).rev() {
            session_history.push_str(&format!("{}: {}\n", ev.author, ev.content));
        }

        let history_context = if !session_history.is_empty() {
            format!("\n\n(直近の会話履歴):\n{}", session_history)
        } else {
            String::new()
        };

        // ゲーム画面のキャプチャ（選択中ウィンドウ優先）
        let use_image = st
            .get("use_image")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let screen_b64 = if use_image {
            let win_name = st.get("window").and_then(|v| v.as_str()).unwrap_or("");
            if !win_name.is_empty() {
                self.log_mgr.info(
                    "Visual",
                    &format!("Capturing target window for commentary: '{}'", win_name),
                );
                window_capture::capture_window_base64(win_name)
                    .or_else(window_capture::capture_primary_screen_base64)
            } else {
                window_capture::capture_primary_screen_base64()
            }
        } else {
            None
        };
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        let full_system_instruction =
            format!("{}{}{}", sys_prompt, memory_context, history_context);

        let chat_messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "状況を見て、テンポよく実況ツッコミやボヤキを1〜2文でお願いします。"
                .to_string(),
        }];

        let disable_thinking = st
            .get("disable_thinking_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let thinking_budget = if disable_thinking { Some(0) } else { None };

        let options = AiGenerateOptions {
            system_instruction: Some(full_system_instruction),
            temperature: Some(0.8),
            max_output_tokens: Some(200),
            image_base64: screen_b64,
            thinking_budget,
        };

        if let Some(handle) = app_handle {
            let _ = handle.emit(
                "gemini_status",
                serde_json::json!({ "is_generating": true }),
            );
        }

        let ai_res = match self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &chat_messages, &options)
            .await
        {
            Ok(res) => {
                if context
                    .map(|session| self.is_current_session(session))
                    .unwrap_or(true)
                {
                    if let Some(handle) = app_handle {
                        let _ = handle.emit(
                            "gemini_status",
                            serde_json::json!({ "is_generating": false }),
                        );
                    }
                }
                res
            }
            Err(e) => {
                if context
                    .map(|session| self.is_current_session(session))
                    .unwrap_or(true)
                {
                    if let Some(handle) = app_handle {
                        let _ = handle.emit(
                            "gemini_status",
                            serde_json::json!({ "is_generating": false }),
                        );
                    }
                }
                self.log_mgr.error(
                    "Commentary",
                    &format!("Auto Commentary generation error: {}", e),
                );
                return Err(e);
            }
        };

        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        let clean_ai_res = ai_res.trim().to_string();

        if !clean_ai_res.is_empty() {
            let ai_event = SessionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                r#type: "auto_commentary".to_string(),
                author: "AI_Auto".to_string(),
                content: clean_ai_res.clone(),
                timestamp: Local::now().to_rfc3339(),
            };
            self.log_mgr.info(
                "Commentary",
                &memory_event_log_message(
                    &ai_event.id,
                    &ai_event.r#type,
                    &ai_event.author,
                    &ai_event.content,
                    "generated",
                ),
            );
            self.append_event_to_session(ai_event.clone(), context);
            let _ = self
                .save_event_to_memory_with_app_context(
                    &ai_event,
                    app_handle.cloned(),
                    context.cloned(),
                )
                .await;

            if context
                .map(|session| self.is_current_session(session))
                .unwrap_or(true)
            {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("session-event", &ai_event);
                }
            }

            // 音声合成 & 発話再生
            if context
                .map(|session| !self.is_current_session(session))
                .unwrap_or(false)
            {
                return Ok(clean_ai_res);
            }
            if context
                .map(|session| self.is_current_session(session))
                .unwrap_or(true)
            {
                *self.last_speak_time.lock() = Instant::now();
            }
            if context
                .map(|session| self.is_current_session(session))
                .unwrap_or(true)
            {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("tts_status", serde_json::json!({ "is_playing": true }));
                }
            }
            let _ = self.tts_mgr.speak(&clean_ai_res, tts_settings).await;
            if context
                .map(|session| self.is_current_session(session))
                .unwrap_or(true)
            {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("tts_status", serde_json::json!({ "is_playing": false }));
                }
            }
            if context
                .map(|session| self.is_current_session(session))
                .unwrap_or(true)
            {
                *self.last_speak_time.lock() = Instant::now();
            }
        }

        Ok(clean_ai_res)
    }

    /// ユーザー発話または Twitch コメントへの応答処理
    pub async fn process_user_input(
        &self,
        author: &str,
        text: &str,
        input_type: &str,
        gemini_api_key: &str,
        brave_api_key: &str,
        gemini_model: &str,
        system_prompt: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        let clean_text = text.trim();
        if clean_text.is_empty() {
            return Ok(String::new());
        }

        // イベント記録
        let user_event = SessionEvent {
            id: uuid::Uuid::new_v4().to_string(),
            r#type: input_type.to_string(),
            author: author.to_string(),
            content: clean_text.to_string(),
            timestamp: Local::now().to_rfc3339(),
        };
        self.log_mgr.info(
            "Input",
            &memory_event_log_message(
                &user_event.id,
                &user_event.r#type,
                &user_event.author,
                &user_event.content,
                "received",
            ),
        );
        self.add_event(user_event.clone());
        if let Some(handle) = app_handle {
            let _ = handle.emit("session-event", &user_event);
        }
        // Persist the raw user/manual event before any retrieval, web search,
        // capture, or AI work. The persistence method detaches embedding and
        // summary work after the durable raw insert.
        self.save_event_to_memory_with_app(&user_event, app_handle.cloned())
            .await;

        self.process_user_input_for_event(
            None,
            &user_event,
            clean_text,
            gemini_api_key,
            brave_api_key,
            gemini_model,
            system_prompt,
            tts_settings,
            app_handle,
        )
        .await
    }

    /// Continue processing an ASR event that was already persisted by the
    /// finalized callback.  The prompt text may be the wake-word-cleaned
    /// candidate, while `event` remains the one authoritative raw event.
    async fn process_user_input_for_session(
        &self,
        context: &SessionContext,
        event: &SessionEvent,
        prompt_text: &str,
        gemini_api_key: &str,
        brave_api_key: &str,
        gemini_model: &str,
        system_prompt: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        if !self.is_current_session(context) {
            self.log_mgr.info(
                "Session",
                &format!(
                    "session_id={} generation={} event_id={} status=stale_input_processing_dropped",
                    context.session_id, context.generation, event.id
                ),
            );
            return Ok(String::new());
        }

        self.process_user_input_for_event(
            Some(context),
            event,
            prompt_text,
            gemini_api_key,
            brave_api_key,
            gemini_model,
            system_prompt,
            tts_settings,
            app_handle,
        )
        .await
    }

    /// Run retrieval, optional web search, Gemini, and TTS for one already
    /// admitted input event.  Keeping event creation outside this method is
    /// what prevents a wake-word-cleaned prompt from becoming a second raw
    /// memory row.
    async fn process_user_input_for_event(
        &self,
        context: Option<&SessionContext>,
        user_event: &SessionEvent,
        clean_text: &str,
        gemini_api_key: &str,
        brave_api_key: &str,
        gemini_model: &str,
        system_prompt: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        let clean_text = clean_text.trim();
        if clean_text.is_empty()
            || context
                .map(|session| !self.is_current_session(session))
                .unwrap_or(false)
        {
            return Ok(String::new());
        }

        // LanceDB 関連記憶をセマンティック検索 (GLuCoSE-base-ja 埋め込みモデル)
        let memory_context = self.get_relevant_memory_context(clean_text).await;
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        // Web 検索が必要か判定
        let is_search_needed = clean_text.contains("検索")
            || clean_text.contains("調べて")
            || clean_text.contains("最新情報");
        let search_context = if is_search_needed {
            self.log_mgr.info(
                "WebSearch",
                &memory_event_log_message(
                    &user_event.id,
                    &user_event.r#type,
                    &user_event.author,
                    clean_text,
                    "web_search_started",
                ),
            );
            let res = self
                .search_client
                .search_and_format(clean_text, brave_api_key)
                .await;
            format!("\n\n{}", res.summary_text)
        } else {
            String::new()
        };
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        let st = crate::settings::load_settings_file(&self.root_dir);
        let use_image = st
            .get("use_image")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // ゲーム画面のキャプチャ（選択中ウィンドウを優先、なければプライマリスクリーン）
        let screen_b64 = if use_image {
            let win_name = st.get("window").and_then(|v| v.as_str()).unwrap_or("");
            if !win_name.is_empty() {
                self.log_mgr.info(
                    "Visual",
                    &format!("Capturing target window: '{}'", win_name),
                );
                window_capture::capture_window_base64(win_name).or_else(|| {
                    self.log_mgr
                        .warn("Visual", "Window capture fallback to primary screen");
                    window_capture::capture_primary_screen_base64()
                })
            } else {
                self.log_mgr.info("Visual", "Capturing primary screen...");
                window_capture::capture_primary_screen_base64()
            }
        } else {
            None
        };
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        // プロンプト構築
        let full_system_instruction =
            format!("{}{}{}", system_prompt, memory_context, search_context);

        // 会話履歴
        let mut chat_messages = Vec::new();
        for ev in self.get_events() {
            let role = if ev.r#type == "ai_response" || ev.r#type == "auto_commentary" {
                "assistant"
            } else {
                "user"
            };
            chat_messages.push(ChatMessage {
                role: role.to_string(),
                content: format!("{}: {}", ev.author, ev.content),
            });
        }

        let st = crate::settings::load_settings_file(&self.root_dir);
        let disable_thinking = st
            .get("disable_thinking_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let thinking_budget = if disable_thinking { Some(0) } else { None };

        let options = AiGenerateOptions {
            system_instruction: Some(full_system_instruction),
            temperature: Some(0.7),
            max_output_tokens: Some(300),
            image_base64: screen_b64,
            thinking_budget,
        };

        self.log_mgr.info(
            "Gemini",
            &format!(
                "{} model={}",
                memory_event_log_message(
                    &user_event.id,
                    &user_event.r#type,
                    &user_event.author,
                    clean_text,
                    "generation_started"
                ),
                gemini_model
            ),
        );

        if let Some(handle) = app_handle {
            let _ = handle.emit(
                "gemini_status",
                serde_json::json!({ "is_generating": true }),
            );
        }

        // Gemini AI 推論
        let ai_res = match self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &chat_messages, &options)
            .await
        {
            Ok(res) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit(
                        "gemini_status",
                        serde_json::json!({ "is_generating": false }),
                    );
                }
                res
            }
            Err(e) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit(
                        "gemini_status",
                        serde_json::json!({ "is_generating": false }),
                    );
                }
                self.log_mgr
                    .error("Gemini", &format!("Gemini API failed: {}", e));
                return Err(e);
            }
        };

        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            return Ok(String::new());
        }

        let clean_ai_res = ai_res.trim().to_string();

        if !clean_ai_res.is_empty() {
            let ai_event = SessionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                r#type: "ai_response".to_string(),
                author: "Assistant".to_string(),
                content: clean_ai_res.clone(),
                timestamp: Local::now().to_rfc3339(),
            };
            self.log_mgr.info(
                "AI",
                &memory_event_log_message(
                    &ai_event.id,
                    &ai_event.r#type,
                    &ai_event.author,
                    &ai_event.content,
                    "generated",
                ),
            );
            self.append_event_to_session(ai_event.clone(), context);
            if let Some(handle) = app_handle {
                let _ = handle.emit("session-event", &ai_event);
            }

            let _ = self
                .save_event_to_memory_with_app_context(
                    &ai_event,
                    app_handle.cloned(),
                    context.cloned(),
                )
                .await;

            // 音声合成 & 発話再生
            self.log_mgr
                .info("TTS", "Synthesizing and playing speech...");
            if context
                .map(|session| !self.is_current_session(session))
                .unwrap_or(false)
            {
                return Ok(clean_ai_res);
            }
            *self.last_speak_time.lock() = Instant::now();
            if let Some(handle) = app_handle {
                let _ = handle.emit("tts_status", serde_json::json!({ "is_playing": true }));
            }
            let _ = self.tts_mgr.speak(&clean_ai_res, tts_settings).await;
            if let Some(handle) = app_handle {
                let _ = handle.emit("tts_status", serde_json::json!({ "is_playing": false }));
            }
            *self.last_speak_time.lock() = Instant::now();
        }

        Ok(clean_ai_res)
    }

    /// note ブログ記事の自動執筆 (5,000文字規模 & スキル注入)
    pub async fn generate_blog_article(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        blog_system_prompt: &str,
    ) -> Result<String, String> {
        // Clone the boundary before entering the async call.  Holding a
        // parking_lot MutexGuard across `.await` makes the Tauri command
        // future !Send and prevents the library from compiling.
        let session_started_at = self.session_started_at.lock().clone();
        let session_id = {
            let id = self.session_id.lock().clone();
            (!id.is_empty()).then_some(id)
        };
        self.generate_blog_article_for_session(
            gemini_api_key,
            gemini_model,
            blog_system_prompt,
            session_started_at,
            session_id,
        )
        .await
    }

    /// Generate a blog article using a caller-supplied session boundary for
    /// persisted fallback rows.  Stop-time generation passes the boundary it
    /// captured before spawning its detached task; the public command keeps
    /// the existing API and uses the current manager boundary when available.
    async fn generate_blog_article_for_session(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        blog_system_prompt: &str,
        session_started_at: Option<DateTime<Utc>>,
        session_id: Option<String>,
    ) -> Result<String, String> {
        // The public/manual command can race a detached ASR raw-save task just
        // like Stop Session can.  Apply the same bounded drain at this
        // boundary so a direct blog request cannot silently omit its final
        // utterance; a timeout remains recoverable because raw persistence is
        // authoritative and the snapshot below is still bounded.
        let outstanding = self.wait_for_event_tasks_bounded().await;
        if outstanding > 0 {
            self.log_mgr.warn(
                "Blog",
                &format!(
                    "手動ブログ生成の保存待機がタイムアウトしました (未完了タスク {}件)。保存済みイベントで続行します。",
                    outstanding
                ),
            );
        }
        let mut events = session_id
            .as_ref()
            .and_then(|id| self.session_archives.lock().get(id).cloned())
            .unwrap_or_else(|| self.get_events());
        if events.is_empty() {
            // A portable app can be restarted between session stop and blog
            // generation.  Recover the persisted raw history when the
            // in-memory ring has no entries; this also makes a delayed stop
            // callback resilient to a UI remount.
            if let Ok(memories) = lance_memory::list_stored_memories(&self.root_dir).await {
                events.extend(persisted_blog_fallback_events(
                    memories,
                    session_started_at.as_ref(),
                ));
            }
        }
        if let Some(id) = session_id {
            self.session_archives.lock().remove(&id);
        }
        if events.is_empty() {
            return Err("会話履歴がありません。".to_string());
        }

        self.log_mgr.info(
            "Blog",
            "Generating note blog article from session history...",
        );

        let mut logs = String::new();
        for ev in &events {
            let line = format!("[{}] {}: {}\n", ev.timestamp, ev.author, ev.content);
            if logs.len().saturating_add(line.len()) > BLOG_MAX_SOURCE_BYTES {
                break;
            }
            logs.push_str(&line);
        }

        let st = crate::settings::load_settings_file(&self.root_dir);

        // 1. スキル適用の判定と読み込み (enable_blog_skills & enabled_blog_skills)
        let enable_skills = st
            .get("enable_blog_skills")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut skill_instructions = String::new();

        if enable_skills {
            let enabled_skills: Vec<String> = st
                .get("enabled_blog_skills")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| vec!["k0ta-writing-style".to_string()]);

            if !enabled_skills.is_empty() {
                let skills_text =
                    crate::settings::load_enabled_skills_text(&self.root_dir, &enabled_skills);
                if !skills_text.is_empty() {
                    self.log_mgr.info(
                        "Blog",
                        &format!("ブログ記事生成にスキルを適用します: {:?}", enabled_skills),
                    );
                    skill_instructions = format!(
                        "\n\n# 適用スキル・執筆ガイドライン\n以下のスキルの指示・文体・トーン＆マナー・構成パターンを最優先で適用して記事を作成してください。\n\n{}",
                        skills_text
                    );
                }
            }
        }

        let base_blog_prompt = if blog_system_prompt.trim().is_empty() {
            crate::prompts::get_prompt(&self.root_dir, "blog_writer_system_prompt")
        } else {
            blog_system_prompt.to_string()
        };

        let full_blog_prompt = format!("{}{}", base_blog_prompt, skill_instructions);

        let prompt_text = format!(
            "# 会話履歴・配信ログ\n{}\n\n上記の会話履歴を元に、指示に従ってnote用の魅力的なプレイ日誌ブログ記事を作成してください。",
            logs
        );

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt_text,
        }];

        // 2. ブログ Thinking モードの制御 (blog_use_thinking: true/false)
        let blog_use_thinking = st
            .get("blog_use_thinking")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let thinking_budget = if blog_use_thinking {
            Some(2048)
        } else {
            Some(0)
        };

        self.log_mgr.info(
            "Blog",
            &format!(
                "ブログ記事生成パラメータ (model: {}, thinking: {})",
                gemini_model, blog_use_thinking
            ),
        );

        let options = AiGenerateOptions {
            system_instruction: Some(full_blog_prompt),
            temperature: Some(0.7),
            max_output_tokens: Some(4000),
            image_base64: None,
            thinking_budget,
        };

        let blog_article = self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &messages, &options)
            .await?;

        self.log_mgr.info(
            "Blog",
            &format!(
                "✅ note ブログ記事の生成に成功しました (文字数: {})",
                blog_article.chars().count()
            ),
        );
        Ok(blog_article)
    }
}

/// settings.json から最新の TTS 設定を抽出
pub fn extract_tts_settings(st: &serde_json::Value) -> TtsSettings {
    let tts_engine = st
        .get("tts_engine")
        .and_then(|v| v.as_str())
        .unwrap_or("voicevox")
        .to_string();
    let speaker_id = st.get("speaker_id").and_then(|v| v.as_i64()).unwrap_or(46) as i32;
    let vits2_speaker_id = st
        .get("vits2_speaker_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let voicevox_url = st
        .get("voicevox_url")
        .and_then(|v| v.as_str())
        .unwrap_or("http://127.0.0.1:50021")
        .to_string();
    TtsSettings {
        tts_engine,
        speaker_id,
        vits2_speaker_id,
        voicevox_url,
    }
}

/// かなのゆらぎ・英数字・大文字小文字の正規化
pub fn normalize_kana(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            'ァ'..='ン' => {
                if let Some(hira) = char::from_u32(c as u32 - 0x60) {
                    result.push(hira);
                } else {
                    result.push(c);
                }
            }
            'A'..='Z' => {
                result.push(c.to_ascii_lowercase());
            }
            'ぁ' => result.push('あ'),
            'ぃ' => result.push('い'),
            'ぅ' => result.push('う'),
            'ぇ' => result.push('え'),
            'ぉ' => result.push('お'),
            'っ' => result.push('つ'),
            'ゃ' => result.push('や'),
            'ゅ' => result.push('ゆ'),
            'ょ' => result.push('よ'),
            '〜' | '～' | 'ー' | '-' => result.push('ー'),
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        admit_redacted_memory_text, automatic_blog_post_enabled, backfill_log_message,
        backfill_log_message_with_counters, backfill_log_severity, contains_stop_word,
        finalize_backfill_progress, increment_reason, inference_failure_reason,
        live_asr_summary_event, partition_backfill_rows, persisted_blog_fallback_events,
        record_backfill_commit, runtime_failure_reason, set_backfill_fatal, should_backfill_row,
        unique_blog_path, BackfillLogSeverity, MemoryBackfillProgress, SessionManager,
        StoredMemory, SummaryBackfillApplyResult,
    };
    use crate::lance_memory::{self, SummaryExclusionDetail};
    use crate::logger::LogManager;
    use crate::memory_v2::repository::MemoryRepository;
    use crate::tts::TtsManager;
    use std::sync::Arc;

    fn unique_session_test_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ga-session-{}-{}-{}",
            name,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create session test root");
        dir
    }

    fn test_session_manager(name: &str) -> SessionManager {
        let root = unique_session_test_root(name);
        SessionManager::new(
            root,
            Arc::new(TtsManager::new()),
            Arc::new(LogManager::new(unique_session_test_root(&format!(
                "{}-logs",
                name
            )))),
        )
    }

    #[tokio::test]
    async fn session_callback_arms_partial_once_and_promotes_final_once() {
        let session = test_session_manager("callback-wake-red");
        session.start_session();
        session
            .asr_engine
            .configure_wake_word(crate::asr::WakeWordConfig::new(
                vec!["ねえぐり".to_string()],
            ));
        session.asr_engine.begin_wake_word_session(1);
        let context = session
            .current_session_context()
            .expect("test session must be active");

        let first_partial =
            session.handle_partial_asr_result(&context, "mic", "ねえぐり", None, None);
        assert!(first_partial.should_acknowledge);
        assert!(
            session
                .is_collecting_prompt
                .load(std::sync::atomic::Ordering::SeqCst),
            "the session callback must mirror partial wake arming into prompt collection"
        );

        let repeated_partial =
            session.handle_partial_asr_result(&context, "mic", "ねえぐり", None, None);
        assert!(!repeated_partial.should_acknowledge);

        let final_prompt = session
            .persist_final_asr_result(&context, "mic", "ねえぐり 今日の配信をまとめて", None, None)
            .await
            .expect("final callback must persist one raw event");
        assert_eq!(
            final_prompt.decision.action,
            crate::asr::WakeWordAction::PromptDetected
        );
        assert!(final_prompt.decision.is_prompt);
        assert!(
            !session
                .is_collecting_prompt
                .load(std::sync::atomic::Ordering::SeqCst),
            "final prompt handling must clear the session collection state"
        );

        let duplicate_final =
            session
                .asr_engine
                .handle_wake_word("mic", "ねえぐり 今日の配信をまとめて", true, 900);
        assert_eq!(
            duplicate_final.action,
            crate::asr::WakeWordAction::DuplicateSuppressed
        );
    }

    #[tokio::test]
    async fn stop_word_final_still_persists_raw_event() {
        let session = test_session_manager("stop-word-raw-red");
        session.start_session();
        let event = super::SessionEvent {
            id: "stop-word-raw-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "ストップ、直前の発話も保存して".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        // This is the finalized callback's durable boundary: stop-word
        // handling must not replace or discard the finalized raw payload.
        session.add_event(event.clone());
        session.save_event_to_memory(&event).await;

        let persisted = crate::lance_memory::get_memory_by_event_id(&session.root_dir, &event.id)
            .await
            .expect("raw event lookup must succeed")
            .expect("stop-word final must remain durable");
        assert_eq!(persisted.document, event.content);
        assert_eq!(persisted.id, event.id);
    }

    #[tokio::test]
    async fn stop_resets_wake_state_before_blog_drain_boundary() {
        let session = test_session_manager("stop-blog-boundary-red");
        session.start_session();
        session.asr_engine.begin_wake_word_session(1);
        let partial = session
            .asr_engine
            .handle_wake_word("mic", "ねえぐり", false, 0);
        assert!(partial.should_acknowledge);

        let guard = session.begin_event_task();
        session.stop_session();
        assert!(!session.is_active());
        let next_session_partial =
            session
                .asr_engine
                .handle_wake_word("mic", "ねえぐり", false, 100);
        assert_eq!(
            next_session_partial.action,
            crate::asr::WakeWordAction::PartialWakeDetected,
        );
        assert!(
            next_session_partial.should_acknowledge,
            "Stop Session must release the old cooldown before its bounded drain"
        );
        drop(guard);
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );
    }

    #[test]
    fn stopped_session_archive_is_reclaimed_when_blog_is_disabled() {
        let session = test_session_manager("archive-reclaim");
        std::fs::write(
            session.root_dir.join("settings.json"),
            r#"{"create_blog_post":false}"#,
        )
        .unwrap();
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.add_event(super::SessionEvent {
            id: "archive-reclaim-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "archive remains durable".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });

        session.stop_session_with_services(None, None);

        assert!(
            !session
                .session_archives
                .lock()
                .contains_key(&context.session_id),
            "disabled blog generation must not retain an unbounded session archive"
        );
    }

    #[test]
    fn stale_summary_can_emit_after_stop_but_not_after_replacement_start() {
        let session = test_session_manager("fact-generation-boundary");
        session.start_session();
        let first = session
            .current_session_context()
            .expect("first session must be active");
        assert!(session.allows_fact_ui_emit(&first));

        session.stop_session();
        assert!(
            session.allows_fact_ui_emit(&first),
            "a completed summary may still update the dashboard before replacement"
        );

        session.start_session();
        let second = session
            .current_session_context()
            .expect("replacement session must be active");
        assert!(!session.allows_fact_ui_emit(&first));
        assert!(session.allows_fact_ui_emit(&second));
    }

    #[test]
    fn stop_word_detection_covers_each_configured_fragment() {
        assert!(contains_stop_word("ストップってば！"));
        assert!(contains_stop_word("ちょっとだまってて"));
        assert!(contains_stop_word("静かにして"));
        assert!(!contains_stop_word("今日はゲームをしよう"));
        assert!(!contains_stop_word(""));
    }

    #[test]
    fn blog_article_paths_never_collide_within_one_second() {
        let dir = unique_session_test_root("blog-names");
        let first = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(first, dir.join("2026-09-07_12-00-00.md"));

        std::fs::write(&first, "first").unwrap();
        let second = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(second, dir.join("2026-09-07_12-00-00_2.md"));

        std::fs::write(&second, "second").unwrap();
        let third = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(third, dir.join("2026-09-07_12-00-00_3.md"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stop_drain_reports_outstanding_raw_save_tasks_after_bounded_wait() {
        let session = test_session_manager("stop-drain");

        // No in-flight raw-save tasks: the drain completes immediately.
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );

        // A held guard is reported as outstanding once the bound elapses
        // instead of blocking blog generation forever.
        let guard = session.begin_event_task();
        let outstanding = session
            .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
            .await;
        assert_eq!(
            outstanding, 1,
            "a held raw-save guard must be reported, never silently ignored"
        );

        // Guards are RAII: releasing it lets the next drain finish cleanly.
        drop(guard);
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );
    }

    #[tokio::test]
    async fn stop_drain_wakes_as_soon_as_the_last_guard_drops() {
        let session = test_session_manager("stop-drain-wake");
        let guard = session.begin_event_task();
        let waiter = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .wait_for_event_tasks_with_timeout(std::time::Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        drop(guard);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("drain must wake on guard release")
                .unwrap(),
            0
        );
    }

    #[test]
    fn memory_admission_redacts_secrets_before_persistence() {
        let admitted = admit_redacted_memory_text("  token=super-secret-value  ");

        assert_eq!(admitted.as_deref(), Some("token=[REDACTED:credentials]"));
        assert!(!admitted
            .as_deref()
            .unwrap_or_default()
            .contains("super-secret-value"));
    }

    #[test]
    fn memory_admission_is_idempotent_and_rejects_empty_text() {
        let first = admit_redacted_memory_text("email alice@example.com").unwrap();
        let second = admit_redacted_memory_text(&first).unwrap();

        assert_eq!(first, second);
        assert_eq!(admit_redacted_memory_text(" \n\t "), None);
    }

    #[test]
    fn blog_generation_defaults_on_for_portable_installs() {
        assert!(automatic_blog_post_enabled(&serde_json::json!({})));
        assert!(automatic_blog_post_enabled(
            &serde_json::json!({"create_blog_post": true})
        ));
        assert!(!automatic_blog_post_enabled(
            &serde_json::json!({"create_blog_post": false})
        ));
    }

    #[test]
    fn blog_fallback_is_session_scoped_and_bounded() {
        let started_at = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let row = |id: &str, timestamp: &str| StoredMemory {
            id: id.into(),
            document: format!("raw-{id}"),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: timestamp.into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: None,
        };

        let rows = vec![
            row("old", "2026-09-07T11:59:59Z"),
            row("new", "2026-09-07T12:00:01+00:00"),
            row("malformed", "not-a-timestamp"),
        ];
        let events = persisted_blog_fallback_events(rows, Some(&started_at));
        assert_eq!(
            events
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["new"]
        );

        let many = (0..250)
            .map(|index| row(&format!("event-{index}"), "2026-09-07T12:00:01Z"))
            .collect();
        assert_eq!(
            persisted_blog_fallback_events(many, Some(&started_at)).len(),
            200
        );
    }

    #[test]
    fn live_summary_processing_accepts_only_asr_stream_events() {
        assert!(live_asr_summary_event("user_speech"));
        assert!(live_asr_summary_event("discord_speech"));
        assert!(!live_asr_summary_event("twitch_chat"));
        assert!(!live_asr_summary_event("ai_response"));
        assert!(!live_asr_summary_event("auto_commentary"));
    }

    #[test]
    fn completed_projection_without_durable_summary_fact_is_reprocessed() {
        let row = StoredMemory {
            id: "legacy-row".into(),
            document: "raw event".into(),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: Some("legacy summary".into()),
            summary_status: Some(lance_memory::SUMMARY_STATUS_COMPLETED.into()),
            summary_model: Some("old-model".into()),
            summary_prompt_version: Some("old".into()),
            vector_source: Some(lance_memory::VECTOR_SOURCE_SUMMARY.into()),
        };
        assert!(should_backfill_row(&row, &std::collections::HashSet::new()));
    }

    #[test]
    fn backfill_partition_aggregates_already_processed_rows() {
        let row = |id: &str| StoredMemory {
            id: id.into(),
            document: "raw event".into(),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: Some(lance_memory::SUMMARY_STATUS_COMPLETED.into()),
            summary_model: Some("model".into()),
            summary_prompt_version: Some(lance_memory::SUMMARY_PROMPT_VERSION.into()),
            vector_source: Some(lance_memory::VECTOR_SOURCE_SUMMARY.into()),
        };
        let rows = vec![row("done-1"), row("done-2"), row("new-1")];
        let durable = rows[..2]
            .iter()
            .map(|item| MemoryRepository::canonical_event_id(&item.id))
            .collect();
        let durable_statuses = rows[..2]
            .iter()
            .map(|item| {
                (
                    MemoryRepository::canonical_event_id(&item.id),
                    super::SummaryStatusRecord {
                        entity_id: item.id.clone(),
                        status: "completed".into(),
                        prompt_version: Some(lance_memory::SUMMARY_PROMPT_VERSION.into()),
                        ..Default::default()
                    },
                )
            })
            .collect();

        let partition = partition_backfill_rows(
            &rows,
            &durable,
            &std::collections::HashSet::new(),
            &durable_statuses,
        );

        assert_eq!(
            partition
                .candidates
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new-1"]
        );
        assert_eq!(partition.skipped_reasons.len(), 1);
        assert_eq!(partition.skipped_reasons.values().next().copied(), Some(2));
    }

    #[test]
    fn backfill_partition_admits_only_shared_summary_candidates() {
        let row = |id: &str, memory_type: &str, document: &str| StoredMemory {
            id: id.into(),
            document: document.into(),
            memory_type: memory_type.into(),
            source: "source".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: Some(lance_memory::VECTOR_SOURCE_DOCUMENT.into()),
        };
        let rows = vec![
            row("human", "user_speech", "永久に保持する設定"),
            row("ai", "ai_response", "assistant output"),
            row("manual", "manual", "manual note"),
            row("empty", "user_speech", "  \n\t"),
            row("unknown", "unknown", "unknown event"),
        ];

        let partition = partition_backfill_rows(
            &rows,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
        );

        assert_eq!(
            partition
                .candidates
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["human"]
        );
        assert!(
            partition.skipped_reasons.is_empty(),
            "non-candidates are progress-only"
        );
        assert_eq!(partition.non_candidates, 4);
    }

    #[test]
    fn row_warnings_do_not_set_fatal_state_but_later_fatal_is_preserved() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 1,
            total: 3,
            queued: 2,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 1,
            attempted: 1,
            retry_count: 0,
            remaining: 2,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };

        increment_reason(&mut progress, "invalid_model_output");
        assert_eq!(progress.state, "running");
        assert_eq!(progress.error, None);
        assert_eq!(progress.fatal_error, None);

        set_backfill_fatal(&mut progress, "journal_commit_failed");
        assert_eq!(progress.state, "error");
        assert_eq!(
            progress.fatal_error.as_deref(),
            Some("journal_commit_failed")
        );
        assert_eq!(progress.error.as_deref(), Some("journal_commit_failed"));
        assert_eq!(progress.remaining, 2);
        assert_eq!(progress.reason_counts["invalid_model_output"], 1);
        assert!(!progress.reason_counts.contains_key("journal_commit_failed"));
    }

    #[test]
    fn durable_chunk_commit_preserves_terminal_counter_invariant() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 1,
            total: 4,
            queued: 3,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 1,
            attempted: 3,
            retry_count: 0,
            remaining: 3,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        let result = SummaryBackfillApplyResult {
            persisted: 1,
            policy_excluded: 1,
            exclusions: vec![SummaryExclusionDetail {
                entity_id: "excluded".into(),
                reason: "policy_excluded".into(),
            }],
            terminal_failed: 1,
            terminal_skipped: 0,
            projection_error: Some("projection unavailable".into()),
        };

        record_backfill_commit(&mut progress, 3, 1, 0, &result);

        assert_eq!(progress.processed, 4);
        assert_eq!(progress.persisted, 1);
        assert_eq!(progress.skipped, 1);
        assert_eq!(progress.failed, 1);
        assert_eq!(progress.remaining, 0);
        assert_eq!(
            progress.processed - progress.excluded,
            progress.persisted + progress.skipped + progress.failed
        );
        assert_eq!(progress.fatal_error, None);
        assert_eq!(progress.reason_counts["policy_excluded"], 1);
    }

    #[test]
    fn backfill_logs_are_structured_redacted_and_severity_is_explicit() {
        let message = backfill_log_message(
            "run-1",
            "inference",
            "event-1",
            2,
            4,
            3,
            "fallback",
            "invalid_model_output",
        );

        assert!(message.contains("run_id=run-1"));
        assert!(message.contains("phase=inference"));
        assert!(message.contains("event_id=event-1"));
        assert!(message.contains("chunk=2"));
        assert!(message.contains("reason=invalid_model_output"));
        assert!(!message.contains("secret source text"));
        assert_eq!(
            backfill_log_severity("fallback"),
            BackfillLogSeverity::Warning
        );
        assert_eq!(
            backfill_log_severity("projection_warning"),
            BackfillLogSeverity::Warning
        );
        assert_eq!(backfill_log_severity("fatal"), BackfillLogSeverity::Error);
        assert_eq!(
            backfill_log_severity("completed"),
            BackfillLogSeverity::Info
        );
        assert_eq!(
            runtime_failure_reason("summary queue full"),
            Some("summary_queue_failed")
        );
        assert_eq!(
            runtime_failure_reason("contract violation: invalid_model_output"),
            None
        );
    }

    #[test]
    fn stable_classifier_preserves_contract_reason_codes() {
        assert_eq!(
            inference_failure_reason("contract violation: metadata_echo"),
            "metadata_echo"
        );
        assert_eq!(
            inference_failure_reason("contract violation: ungrounded_summary"),
            "ungrounded_summary"
        );
        assert_eq!(
            inference_failure_reason("summary response is not JSON"),
            "invalid_model_output"
        );
    }

    #[test]
    fn stable_classifier_keeps_runtime_timeout_out_of_generic_inference_failure() {
        assert_eq!(
            inference_failure_reason("summary_runtime_timeout: request deadline exceeded"),
            "summary_runtime_timeout"
        );
        assert_eq!(
            runtime_failure_reason("summary_runtime_timeout: request deadline exceeded"),
            Some("summary_runtime_timeout")
        );

        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 0,
            total: 1,
            queued: 1,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 0,
            attempted: 0,
            retry_count: 1,
            remaining: 1,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        set_backfill_fatal(
            &mut progress,
            runtime_failure_reason("summary_runtime_timeout: request deadline exceeded")
                .expect("timeout is a fatal runtime reason"),
        );
        assert_eq!(progress.state, "error");
        assert_eq!(
            progress.fatal_error.as_deref(),
            Some("summary_runtime_timeout")
        );
        assert_eq!(progress.failed, 0);
        assert_eq!(progress.reason_counts.len(), 0);
    }

    #[test]
    fn retrying_the_same_failed_row_does_not_double_count_final_failure() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 0,
            total: 1,
            queued: 1,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 0,
            attempted: 1,
            retry_count: 1,
            remaining: 1,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        let result = SummaryBackfillApplyResult::default();

        // The first attempt and its corrective retry represent one final row.
        record_backfill_commit(&mut progress, 1, 0, 1, &result);
        record_backfill_commit(&mut progress, 1, 0, 1, &result);

        assert_eq!(progress.failed, 1);
        assert_eq!(progress.retry_count, 1);
        assert_eq!(progress.processed, 1);
    }

    #[test]
    fn completed_with_row_warnings_is_not_a_run_fatal_error() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 2,
            total: 2,
            queued: 2,
            skipped: 0,
            failed: 1,
            persisted: 1,
            excluded: 0,
            attempted: 2,
            retry_count: 1,
            remaining: 0,
            reason_counts: [("metadata_echo".to_string(), 1)].into_iter().collect(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };

        finalize_backfill_progress(&mut progress, 2, false);

        assert_eq!(progress.state, "completed");
        assert_eq!(progress.fatal_error, None);
        assert_eq!(progress.error, None);
        assert_eq!(progress.remaining, 0);
        assert!(progress.message.contains("warnings"));
    }

    #[test]
    fn backfill_log_contains_only_safe_identity_and_counter_fields() {
        let message = backfill_log_message_with_counters(
            "run-7",
            "inference",
            "event-7",
            3,
            2,
            4,
            1,
            2,
            0,
            1,
            1,
            "fallback",
            "metadata_echo: private source text",
        );

        assert!(message.contains("run_id=run-7"));
        assert!(message.contains("phase=inference"));
        assert!(message.contains("event_id=event-7"));
        assert!(message.contains("chunk=3"));
        assert!(message.contains("attempt=2"));
        assert!(message.contains("status=fallback"));
        assert!(message.contains("reason=metadata_echo"));
        assert!(message.contains("counters="));
        assert!(!message.contains("private source text"));
        assert!(!message.contains("model response"));
    }
}
