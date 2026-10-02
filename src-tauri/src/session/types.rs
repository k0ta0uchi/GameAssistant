use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::asr::WakeWordDecision;
use crate::lance_memory::StoredMemory;
use crate::memory_v2::repository::SummaryBatchInput;
use crate::tts::TtsSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDropReason {
    EmptyPrompt,
    EmptyPromptAfterWakeRemoval,
    WakeWordRequired,
    WakeOnly,
    DuplicateSuppressed,
    StaleAtEntry,
    StaleAfterMemorySearch,
    StaleAfterWebSearch,
    StaleAfterScreenCapture,
    StaleBeforeGeneration,
    StaleAfterGeneration,
    StaleBeforeTts,
    StaleAfterTts,
    EmptyResponse,
    StopWord,
    UnsupportedAction,
}

impl InputDropReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyPrompt => "empty_prompt",
            Self::EmptyPromptAfterWakeRemoval => "empty_prompt_after_wake_removal",
            Self::WakeWordRequired => "wake_word_required",
            Self::WakeOnly => "wake_only_waiting_for_prompt",
            Self::DuplicateSuppressed => "duplicate_suppressed",
            Self::StaleAtEntry => "stale_at_entry",
            Self::StaleAfterMemorySearch => "stale_after_memory_search",
            Self::StaleAfterWebSearch => "stale_after_web_search",
            Self::StaleAfterScreenCapture => "stale_after_screen_capture",
            Self::StaleBeforeGeneration => "stale_before_generation",
            Self::StaleAfterGeneration => "stale_after_generation",
            Self::StaleBeforeTts => "stale_before_tts",
            Self::StaleAfterTts => "stale_after_tts",
            Self::EmptyResponse => "empty_response",
            Self::StopWord => "stop_word",
            Self::UnsupportedAction => "unsupported_action",
        }
    }
}

pub fn should_emit_input_drop_toast(reason: InputDropReason) -> bool {
    // Ordinary speech without a wake word is expected background input, not a
    // user-visible error.  Keep the structured drop log for diagnostics while
    // reserving toasts for accepted wake transitions and actionable failures.
    !matches!(reason, InputDropReason::WakeWordRequired)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtsOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputProcessingOutcome {
    Generated { response: String, tts: TtsOutcome },
    Dropped { reason: InputDropReason },
}

pub type InputProcessingResult = Result<InputProcessingOutcome, String>;

/// Normalize only transport-level whitespace. The raw event is retained as
/// captured; this value is used for prompt admission so full-width spaces and
/// line breaks cannot make a valid prompt look empty.
pub fn normalize_prompt_text(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character == '\u{3000}' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn prompt_is_sendable(prompt: &str) -> bool {
    !prompt.is_empty()
}

pub fn asr_prompt_is_sendable(prompt: &str) -> bool {
    prompt.chars().count() >= MIN_ASR_PROMPT_CHARS
}

pub fn stop_word_boundary_after(text: &str, word_end: usize, word: &str) -> bool {
    let suffix = &text[word_end..];
    if suffix.is_empty() {
        return true;
    }
    if suffix
        .chars()
        .next()
        .is_some_and(|character| character.is_whitespace() || "、。！？!?.,".contains(character))
    {
        return true;
    }

    // Japanese stop commands commonly take a short polite/imperative ending;
    // only those endings are accepted after the exact configured phrase.
    match word {
        "ストップ" => ["って", "して", "よ", "ね", "ください"]
            .iter()
            .any(|ending| suffix.starts_with(ending)),
        "だまって" => ["て", "よ", "ね", "ください"]
            .iter()
            .any(|ending| suffix.starts_with(ending)),
        "静かに" => ["して", "しろ", "よ", "ね", "ください"]
            .iter()
            .any(|ending| suffix.starts_with(ending)),
        _ => false,
    }
}

/// Utterances containing any of these fragments stop TTS playback and cancel
/// prompt collection. The finalized utterance itself is still persisted; this
/// predicate only decides the extra stop action.
pub fn contains_stop_word(text: &str) -> bool {
    let normalized = normalize_prompt_text(text);
    ["ストップ", "だまって", "静かに"].iter().any(|stop_word| {
        let mut search_from = 0;
        while let Some(relative_start) = normalized[search_from..].find(stop_word) {
            let start = search_from + relative_start;
            let end = start + stop_word.len();
            if stop_word_boundary_after(&normalized, end, stop_word) {
                return true;
            }
            search_from = end;
            if search_from >= normalized.len() {
                break;
            }
        }
        false
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAdmissionDropReason {
    EmptyTranscript,
    PolicyDenied,
    RedactionNotStable,
    UnknownStream,
}

impl MemoryAdmissionDropReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyTranscript => "empty_transcript",
            Self::PolicyDenied => "policy_denied",
            Self::RedactionNotStable => "redaction_not_stable",
            Self::UnknownStream => "unknown_stream",
        }
    }
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
pub struct SessionContext {
    pub session_id: String,
    pub generation: u64,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PersistedAsrEvent {
    pub event: SessionEvent,
    pub decision: WakeWordDecision,
    pub stop_word_detected: bool,
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
    pub reason_counts: BTreeMap<String, usize>,
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

pub enum BackfillResult {
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

pub const SESSION_EVENT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const BLOG_FALLBACK_MAX_EVENTS: usize = 200;
pub const BLOG_MAX_SOURCE_BYTES: usize = 64 * 1024;
pub const MIN_ASR_PROMPT_CHARS: usize = 2;
pub const MAX_CHAT_HISTORY_MESSAGES: usize = 40;
pub const MAX_CHAT_HISTORY_CHARS: usize = 16 * 1024;
pub const INPUT_WEB_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const INPUT_SCREEN_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const BACKFILL_MAX_SOURCE_CHARS: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillLogSeverity {
    Info,
    Warning,
    Error,
}

pub struct BackfillPartition {
    pub candidates: Vec<StoredMemory>,
    pub skipped_reasons: BTreeMap<String, usize>,
    pub non_candidates: usize,
}

/// Guard used for every asynchronous event callback that can still append a
/// session event after the stop button is pressed.  Blog generation waits for
/// these guards so its snapshot cannot race the final ASR/Twitch write.
pub struct EventTaskGuard {
    pub pending: Arc<AtomicUsize>,
    pub idle: Arc<Notify>,
}

impl Drop for EventTaskGuard {
    fn drop(&mut self) {
        if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
}

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
