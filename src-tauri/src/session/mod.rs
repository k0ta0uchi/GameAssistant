//! # Session Management Architecture
//!
//! This module coordinates the runtime lifecycle of an active assistant session,
//! spanning real-time speech recognition (ASR), wake word detection, prompt admission,
//! multimodal Gemini reasoning, text-to-speech (TTS), autonomous live commentary,
//! long-term semantic memory persistence (LanceDB / memory-v2), and automatic blog generation.
//!
//! ## Invariant 1: Lock Ordering (Hierarchy)
//! To prevent deadlocks across concurrent callbacks and async tasks, strict lock ordering
//! is enforced:
//!
//! 1. `session_lifecycle` (`Arc<Mutex<()>>`):
//!    - **Level 1 (Highest)**. Acquired first during session state transitions (`begin_session`,
//!      `stop_session_internal`), event appends (`append_event_to_session`), and fact emit gates.
//! 2. Subordinate Mutexes:
//!    - **Level 2 (Lower)**: `session_id`, `events`, `session_archives`, `session_started_instant`,
//!      `session_start_request_id`, `session_started_at`, `last_speak_time`.
//!    - Subordinate locks may be acquired while holding `session_lifecycle`.
//!    - **NEVER** acquire `session_lifecycle` while holding any Level 2 lock.
//!    - **NEVER** hold any `parking_lot::MutexGuard` across an `.await` point (causes `!Send` compile failure).
//!
//! ## Invariant 2: Session Generation & Stale Callbacks
//! - `session_generation` (`Arc<AtomicU64>`): Monotonically increasing epoch counter.
//! - Incremented on **BOTH** `begin_session` (Start) and `stop_session_internal` (Stop).
//! - Every registered ASR / Twitch callback captures an immutable `SessionContext` (`session_id`, `generation`, `started_at`).
//! - **Admission Gate**: When an async callback fires:
//!   - If `session_generation` has advanced, the callback is classified as **stale**.
//!   - Stale ASR final transcripts and Twitch messages are **still persisted to raw storage**
//!     (raw durability invariant: never lose user speech).
//!   - However, follow-up prompt processing, Gemini inference, TTS synthesis, and UI event emissions
//!     are strictly blocked (`is_current_session` / `context_allows_ui`).
//!
//! ## Invariant 3: Raw Durability vs Live UI/AI Gate
//! - Raw events are persisted immediately to LanceDB upon arrival.
//! - Non-ASR streams or unadmitted wake-word fragments are filtered before prompt generation,
//!   but raw transcripts are safely journaled with redaction.
//!
//! ## Invariant 4: Session Archives & Blog Snapshot Isolation
//! - When a session stops, current ring buffer events are drained into `session_archives[session_id]`.
//! - If the user rapidly restarts a session (Stop -> Start), the new session receives a new generation
//!   and fresh event buffer.
//! - The detached blog generation task reads from `session_archives[stopped_session_id]`, preventing
//!   it from reading events belonging to the new session.
//! - `EventTaskGuard` tracks in-flight raw saving tasks. `wait_for_event_tasks_bounded` drains them
//!   before blog generation begins (up to 10 seconds timeout).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Notify;

use crate::ai_client::AiClient;
use crate::asr::AsrEngine;
use crate::audio_input::AudioInputManager;
use crate::local_summary::LocalSummaryService;
use crate::logger::LogManager;
use crate::tts::TtsManager;
use crate::web_search::WebSearchClient;

pub(crate) mod ai_pipeline;
pub(crate) mod blog;
pub(crate) mod commentary;
pub(crate) mod input_pipeline;
pub(crate) mod lifecycle;
pub(crate) mod persistence;
pub mod types;

#[cfg(test)]
mod tests;

pub(crate) use lifecycle::resolve_effective_twitch_channel;
#[allow(unused_imports)]
pub(crate) use persistence::memory_event_log_message;
pub use types::*;

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
    /// current-session ring after a rapid Stop->Start transition.
    session_archives: Arc<Mutex<HashMap<String, Vec<SessionEvent>>>>,
    /// Monotonic Start Session boundary used for first-final latency metrics.
    session_started_instant: Arc<Mutex<Option<Instant>>>,
    /// Correlates the readiness-gated Start Session request with its first
    /// finalized ASR latency record. Direct/native callers leave this empty.
    session_start_request_id: Arc<Mutex<Option<String>>>,
    first_asr_finalized_logged: Arc<AtomicBool>,
    /// UTC boundary captured when Start Session is accepted. The stop-time
    /// blog fallback uses this to recover only raw rows from the session being
    /// closed, even when the in-memory ring has been cleared or remounted.
    session_started_at: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
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
            session_archives: Arc::new(Mutex::new(HashMap::new())),
            session_started_instant: Arc::new(Mutex::new(None)),
            session_start_request_id: Arc::new(Mutex::new(None)),
            first_asr_finalized_logged: Arc::new(AtomicBool::new(false)),
            session_started_at: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn begin_event_task(&self) -> EventTaskGuard {
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
    pub(crate) async fn wait_for_event_tasks_bounded(&self) -> usize {
        self.wait_for_event_tasks_with_timeout(SESSION_EVENT_DRAIN_TIMEOUT)
            .await
    }

    pub(crate) async fn wait_for_event_tasks_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> usize {
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

    /// Final process teardown used by the Tauri exit hook. Normal session
    /// stops intentionally keep ASR warm for the next session.
    pub fn shutdown(&self) {
        self.stop_session();
        self.asr_engine.ws_client.stop();
    }

    pub fn get_events(&self) -> Vec<SessionEvent> {
        self.events.lock().clone()
    }

    /// イベント追加
    pub fn add_event(&self, event: SessionEvent) {
        self.append_event_to_session(event, None);
    }
}
