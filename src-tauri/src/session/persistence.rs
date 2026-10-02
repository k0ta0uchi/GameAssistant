use std::sync::atomic::Ordering;
use chrono::Utc;
use tauri::{AppHandle, Emitter};

use crate::lance_memory::{self, MemoryItem, StoredMemory, SummaryBackfillApplyResult};
use crate::memory_v2::error::MemoryError;
use crate::memory_v2::policy::{Policy, PrivacyAdmission};
use crate::memory_v2::repository::{
    MemoryRepository, SummaryBatchInput, SummaryProcessDecision, SummaryProcessMode,
    SummaryStatusRecord, SUMMARY_MODEL_ID as MEMORY_SUMMARY_MODEL_ID,
    SUMMARY_PROMPT_VERSION as MEMORY_SUMMARY_PROMPT_VERSION,
};
use crate::summary_failure::{reason_from_error, SummaryFailureReason};

use super::lifecycle::session_context_fields;
use super::types::*;
use super::SessionManager;

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

pub(crate) fn live_asr_summary_event(event_type: &str) -> bool {
    matches!(event_type, "user_speech" | "discord_speech")
}

pub(crate) fn admit_redacted_memory_text_with_reason(
    input: &str,
) -> Result<String, MemoryAdmissionDropReason> {
    let input = input.trim();
    if input.is_empty() {
        return Err(MemoryAdmissionDropReason::EmptyTranscript);
    }
    let redacted = Policy::default()
        .admit_raw_text(input, PrivacyAdmission::public())
        .map_err(|error| match error {
            // Keep policy failures machine-readable and intentionally omit
            // the category text: policy diagnostics must never echo input.
            MemoryError::ProhibitedCategory(_) => MemoryAdmissionDropReason::PolicyDenied,
            MemoryError::InvalidContent => MemoryAdmissionDropReason::RedactionNotStable,
            _ => MemoryAdmissionDropReason::PolicyDenied,
        })?;
    let redacted = redacted.as_str().trim();
    if redacted.is_empty() {
        return Err(MemoryAdmissionDropReason::RedactionNotStable);
    }
    Ok(redacted.to_string())
}

/// Compatibility wrapper for non-diagnostic memory callers.
pub(crate) fn admit_redacted_memory_text(input: &str) -> Option<String> {
    admit_redacted_memory_text_with_reason(input).ok()
}

pub(crate) fn raw_admission_drop_message(
    manager: &SessionManager,
    event_id: &str,
    context: Option<&SessionContext>,
    status: &str,
    reason: MemoryAdmissionDropReason,
) -> String {
    let (session_id, generation) = session_context_fields(manager, context);
    format!(
        "session_id={} generation={} event_id={} status={} reason={}",
        session_id,
        generation,
        event_id,
        status,
        reason.code()
    )
}

pub(crate) fn raw_durability_gap_message(
    manager: &SessionManager,
    event_id: &str,
    context: Option<&SessionContext>,
    status: &str,
) -> String {
    let (session_id, generation) = session_context_fields(manager, context);
    format!(
        "session_id={} generation={} event_id={} status={}",
        session_id, generation, event_id, status
    )
}

pub(crate) fn backfill_log_severity(status: &str) -> BackfillLogSeverity {
    match status {
        "fatal" | "error" => BackfillLogSeverity::Error,
        "fallback" | "skipped" | "warning" | "projection_warning" => BackfillLogSeverity::Warning,
        _ => BackfillLogSeverity::Info,
    }
}

/// Structured backfill diagnostics intentionally contain identifiers and
/// reason codes only. In particular, neither source documents nor model
/// requests/responses are accepted by this formatter.
pub(crate) fn backfill_log_message(
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

pub(crate) fn backfill_log_message_with_counters(
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

pub(crate) fn increment_reason(progress: &mut MemoryBackfillProgress, reason: &str) {
    let code = backfill_reason_code(reason);
    if !code.is_empty() {
        *progress.reason_counts.entry(code).or_insert(0) += 1;
    }
}

pub(crate) fn record_final_reason(
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

pub(crate) fn refresh_backfill_remaining(progress: &mut MemoryBackfillProgress) {
    progress.remaining = progress.total.saturating_sub(progress.processed);
}

pub(crate) fn set_backfill_fatal(progress: &mut MemoryBackfillProgress, reason: &str) {
    let safe_reason = backfill_reason_code(reason);
    progress.state = "error".into();
    progress.fatal_error = Some(safe_reason.clone());
    // `error` is retained for older clients; it now mirrors only a fatal
    // reason and never receives row-local warnings.
    progress.error = Some(safe_reason);
    refresh_backfill_remaining(progress);
}

pub(crate) fn backfill_reason_code(reason: &str) -> String {
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

pub(crate) fn inference_failure_reason(error: &str) -> &'static str {
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

pub(crate) fn retry_count_for_reason(reason: &str) -> usize {
    match backfill_reason_code(reason).as_str() {
        "invalid_model_output"
        | "metadata_echo"
        | "ungrounded_summary"
        | "summary_runtime_timeout"
        | "summary_runtime_failed" => 1,
        _ => 0,
    }
}

pub(crate) fn runtime_failure_reason(error: &str) -> Option<&'static str> {
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

pub(crate) fn record_backfill_commit(
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

pub(crate) fn finalize_backfill_progress(
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
pub(crate) fn is_backfill_candidate(row: &StoredMemory) -> bool {
    matches!(
        lance_memory::summary_admission(&row.memory_type, &row.document),
        lance_memory::SummaryAdmission::Eligible
    )
}

pub(crate) fn should_backfill_row(
    row: &StoredMemory,
    durable_summary_event_ids: &std::collections::HashSet<String>,
) -> bool {
    !durable_summary_event_ids.contains(&MemoryRepository::canonical_event_id(&row.id))
}

pub(crate) fn backfill_skip_reason(
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
pub(crate) fn partition_backfill_rows(
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

pub(crate) fn partition_backfill_rows_with_repairable(
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

pub(crate) async fn persist_backfill_chunk(
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


impl SessionManager {
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

    pub(crate) async fn summarize_backfill_event(&self, row: StoredMemory) -> BackfillResult {
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
    pub(crate) async fn save_event_to_memory_with_app_context(
        &self,
        event: &SessionEvent,
        app_handle: Option<AppHandle>,
        context: Option<SessionContext>,
    ) -> bool {
        let _event_guard = self.begin_event_task();
        let doc_text = match admit_redacted_memory_text_with_reason(&event.content) {
            Ok(text) => text,
            Err(reason) => {
                self.log_mgr.warn(
                    "Memory",
                    &raw_admission_drop_message(
                        self,
                        &event.id,
                        context.as_ref(),
                        "raw_admission_dropped",
                        reason,
                    ),
                );
                return false;
            }
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
                                    "{} reason={}",
                                    memory_event_log_message(
                                        &event_id,
                                        &event_type,
                                        &author,
                                        &projection_text,
                                        "projection_deferred"
                                    ),
                                    lance_memory::raw_persistence_failure_reason(&error)
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
                let summary_is_eligible = matches!(
                    lance_memory::summary_admission(&event.r#type, &doc_text),
                    lance_memory::SummaryAdmission::Eligible
                );
                let summary_context_is_current = context
                    .as_ref()
                    .map(|value| self.is_current_session(value))
                    .unwrap_or(true);
                if live_asr_summary_event(&event.r#type)
                    && summary_is_eligible
                    && summary_context_is_current
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
                } else if live_asr_summary_event(&event.r#type)
                    && summary_is_eligible
                    && !summary_context_is_current
                {
                    // A final ASR frame may arrive after Stop and still needs
                    // its raw event persisted.  Its live summary, however,
                    // belongs to the old session and must remain pending for
                    // an explicit Memory Manager pass instead of mutating a
                    // replacement session's Fact stream.
                    self.log_mgr.info(
                        "Session",
                        &raw_durability_gap_message(
                            self,
                            &event.id,
                            context.as_ref(),
                            "stale_summary_dropped",
                        ),
                    );
                }
                true
            }
            Err(e) => {
                let reason = lance_memory::raw_persistence_failure_reason(&e);
                let status = if e.starts_with("raw durable;") {
                    "raw_durable_followup_failed"
                } else {
                    "raw_persist_failed"
                };
                self.log_mgr.error(
                    "Memory",
                    &format!(
                        "session_id={} generation={} event_id={} status={} reason={}",
                        context
                            .as_ref()
                            .map(|value| value.session_id.as_str())
                            .unwrap_or("-"),
                        context
                            .as_ref()
                            .map(|value| value.generation)
                            .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                        event.id,
                        status,
                        reason,
                    ),
                );
                false
            }
        }
    }

    pub(crate) async fn embed_and_update_document_vector(
        &self,
        event_id: &str,
        event_type: &str,
        source: &str,
        content: &str,
    ) {
        let content = match admit_redacted_memory_text_with_reason(content) {
            Ok(content) => content,
            Err(reason) => {
                self.log_mgr.warn(
                    "Memory",
                    &format!(
                        "event_id={} status=document_embedding_dropped reason={}",
                        event_id,
                        reason.code()
                    ),
                );
                return;
            }
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

    pub(crate) async fn process_summary_candidate(
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
    pub(crate) async fn process_summary_event(
        &self,
        mut event: SessionEvent,
        app_handle: Option<AppHandle>,
        context: Option<SessionContext>,
    ) {
        let Some(redacted_content) = admit_redacted_memory_text(&event.content) else {
            let _ = lance_memory::mark_summary_fallback_with_reason(
                &self.root_dir,
                &event.id,
                "invalid_model_output",
            )
            .await;
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
                let reason = runtime_failure_reason(&error)
                    .unwrap_or_else(|| inference_failure_reason(&error));
                match lance_memory::mark_summary_fallback_with_reason(
                    &self.root_dir,
                    &event.id,
                    reason,
                )
                .await
                {
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
            match lance_memory::mark_summary_skipped_with_reason(
                &self.root_dir,
                &event.id,
                "model_declined",
            )
            .await
            {
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
                match lance_memory::mark_summary_fallback_with_reason(
                    &self.root_dir,
                    &event.id,
                    "empty_summary",
                )
                .await
                {
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
            Err(error) => {
                let _ = lance_memory::mark_summary_fallback_with_reason(
                    &self.root_dir,
                    &event.id,
                    "journal_commit_failed",
                )
                .await;
                self.log_mgr.warn(
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
                );
            }
        }
    }

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
}
