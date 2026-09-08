use crate::session::normalize_kana;
use candle_core::{Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{audio, model::Whisper, Config};
use futures::{SinkExt, StreamExt};
use hound::{WavSpec, WavWriter};
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Cursor};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use tokenizers::Tokenizer;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::logger::LogManager;

const ASR_WS_URL: &str = "ws://127.0.0.1:18088/asr";
const ASR_WS_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
// A cold CUDA model load can take around a minute on this machine, and the
// server only starts listening after that load completes.  Keep a generous
// bounded window so a healthy process is not killed at the exact load
// boundary; child-exit detection still fails fast for genuine startup errors.
const ASR_WS_MAX_ATTEMPTS: usize = 1_800;
const ASR_WS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
const ASR_WS_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
pub const DEFAULT_WAKE_WORD_COOLDOWN_MS: u64 = 1_800;
pub const WAKE_WORD_ENGINE: &str = "whisper_vad";

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct AsrJobHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
// SAFETY: a job object handle is process-wide and is only closed while the
// owning process store is locked; moving the wrapper between runtime threads
// does not transfer ownership to an arbitrary thread.
unsafe impl Send for AsrJobHandle {}

#[cfg(windows)]
// SAFETY: access is serialized by the Arc<Mutex<Option<AsrJobHandle>>> that
// owns the handle. The wrapper itself is Copy only so it can be moved into the
// close helper after the mutex has yielded ownership.
unsafe impl Sync for AsrJobHandle {}

#[cfg(test)]
async fn probe_ws(url: &str, connect_timeout: std::time::Duration) -> bool {
    tokio::time::timeout(connect_timeout, connect_async(url))
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false)
}

/// Forward a child-process line to the application log stream. Keeping this
/// at the process boundary makes ASR startup failures visible in the same
/// Console Logs panel as Rust-side diagnostics while preserving stdout/stderr
/// fallback behavior for tests and pre-initialization failures.
fn record_child_line(log_mgr: &Option<Arc<LogManager>>, stream: &str, line: &str) {
    let message = line.trim();
    if message.is_empty() {
        return;
    }

    if let Some(log_mgr) = log_mgr {
        let lower = message.to_ascii_lowercase();
        if stream == "stderr"
            && ["error", "exception", "traceback", "fatal"]
                .iter()
                .any(|marker| lower.contains(marker))
        {
            log_mgr.error("ASR-Server", message);
        } else if stream == "stderr" {
            log_mgr.warn("ASR-Server", message);
        } else {
            log_mgr.info("ASR-Server", message);
        }
    } else if stream == "stderr" {
        eprintln!("[STDERR] [ASR-Server] {}", message);
    } else {
        println!("[INFO] [ASR-Server] {}", message);
    }
}

#[cfg(windows)]
fn taskkill_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;

    if pid == 0 || pid == std::process::id() {
        return;
    }
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(0x08000000)
        .output();
}

#[cfg(not(windows))]
fn taskkill_process_tree(_pid: u32) {}

/// Terminate an ASR child and any descendants it created.  The explicit
/// `taskkill /T` is kept as a compatibility fallback for older Windows
/// installations; the Job Object held by the client is the crash-safe
/// mechanism and is closed by the caller after this best-effort termination.
fn terminate_child_tree(mut child: Child) {
    let pid = child.id();
    // Kill the tree before the parent handle is waited. The Job Object closes
    // any descendants that were not visible to taskkill, while avoiding a
    // second PID lookup after the parent has exited and its PID can be reused.
    taskkill_process_tree(pid);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn close_asr_job(job: AsrJobHandle) {
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(job.0);
    }
}

/// Take ownership of the tracked process/job exactly once.  This function is
/// intentionally callable from both the synchronous stop path and the async
/// WebSocket worker's terminal-error path, so every failure route has the same
/// cleanup semantics and cannot leave Python loading in the background.
fn terminate_owned_process(
    child_store: &Arc<Mutex<Option<Child>>>,
    process_lock: &Arc<Mutex<()>>,
    #[cfg(windows)] job_store: &Arc<Mutex<Option<AsrJobHandle>>>,
    expected_generation: Option<(&Arc<AtomicU64>, u64)>,
) {
    let _process_guard = process_lock.lock();
    if let Some((generation, expected)) = expected_generation {
        if generation.load(Ordering::SeqCst) != expected {
            return;
        }
    }
    if let Some(child) = child_store.lock().take() {
        terminate_child_tree(child);
    }
    #[cfg(windows)]
    if let Some(job) = job_store.lock().take() {
        // Closing a KILL_ON_JOB_CLOSE job is the final descendant cleanup
        // guarantee, including grandchildren that outlived the Python parent.
        close_asr_job(job);
    }
    // Keep the compatibility sweep under the same process lock. Otherwise a
    // replacement worker could bind 18088 between the tracked-child cleanup
    // and this sweep, and a stale failure task could kill the new worker.
    kill_process_on_port(18088);
}

/// Invalidate one WebSocket generation and tear down the process that belongs
/// to it.  A compare-and-exchange makes this safe against a user starting a
/// replacement worker while an older connection task is still unwinding.
fn cleanup_failed_worker(
    is_started: &Arc<AtomicBool>,
    is_ready: &Arc<AtomicBool>,
    connection_active: &Arc<AtomicBool>,
    readiness_notify: &Arc<Notify>,
    generation: &Arc<AtomicU64>,
    expected_generation: u64,
    child_store: &Arc<Mutex<Option<Child>>>,
    process_lock: &Arc<Mutex<()>>,
    #[cfg(windows)] job_store: &Arc<Mutex<Option<AsrJobHandle>>>,
) -> bool {
    let next_generation = expected_generation.saturating_add(1);
    if generation
        .compare_exchange(
            expected_generation,
            next_generation,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        // A newer worker (or an explicit stop) owns the stores now. Never let
        // a stale task kill that replacement process.
        return false;
    }

    is_started.store(false, Ordering::SeqCst);
    is_ready.store(false, Ordering::SeqCst);
    connection_active.store(false, Ordering::SeqCst);
    readiness_notify.notify_waiters();
    terminate_owned_process(
        child_store,
        process_lock,
        #[cfg(windows)]
        job_store,
        Some((generation, next_generation)),
    );
    true
}

#[cfg(windows)]
fn attach_asr_job(child: &Child) -> Result<AsrJobHandle, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    let raw_handle = child.as_raw_handle();
    if raw_handle.is_null() {
        return Err("ASR server process handle unavailable".to_string());
    }
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|error| format!("create ASR kill-on-close job: {}", error))?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if let Err(error) = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } {
        unsafe {
            let _ = CloseHandle(job);
        }
        return Err(format!("configure ASR kill-on-close job: {}", error));
    }
    if let Err(error) = unsafe { AssignProcessToJobObject(job, HANDLE(raw_handle)) } {
        unsafe {
            let _ = CloseHandle(job);
        }
        return Err(format!("assign ASR server to kill-on-close job: {}", error));
    }
    Ok(AsrJobHandle(job))
}

use std::collections::HashMap;
use tokio::sync::oneshot;

type AsrCallback = Arc<dyn Fn(String, String, bool, Option<f64>) + Send + Sync>;

struct AudioPacket {
    stream: String,
    data: Vec<u8>,
}

/// The wake-word path is intentionally explicit about its policy.  The
/// default product mode requires one of the configured/default wake words;
/// callers may opt into forwarding every final utterance or disabling the
/// path entirely without relying on an implicit empty-list branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeWordMode {
    RequireWakeWord,
    AllFinalSpeech,
    Disabled,
}

impl Default for WakeWordMode {
    fn default() -> Self {
        Self::RequireWakeWord
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeWordPhase {
    Idle,
    Armed,
    AwaitingPrompt,
}

impl Default for WakeWordPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeWordAction {
    Ignored,
    PartialWakeDetected,
    PartialPromptCandidate,
    FinalWakeOnly,
    PromptDetected,
    PromptReceived,
    FinalSpeech,
    DuplicateSuppressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeWordConfig {
    pub custom_wake_words: Vec<String>,
    pub mode: WakeWordMode,
    pub cooldown_ms: u64,
}

impl Default for WakeWordConfig {
    fn default() -> Self {
        Self {
            custom_wake_words: Vec::new(),
            mode: WakeWordMode::RequireWakeWord,
            cooldown_ms: DEFAULT_WAKE_WORD_COOLDOWN_MS,
        }
    }
}

impl WakeWordConfig {
    pub fn new(custom_wake_words: Vec<String>) -> Self {
        Self {
            custom_wake_words,
            ..Self::default()
        }
    }

    pub fn with_mode(mut self, mode: WakeWordMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_cooldown_ms(mut self, cooldown_ms: u64) -> Self {
        self.cooldown_ms = cooldown_ms;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeWordDecision {
    pub stream: String,
    pub engine: &'static str,
    pub session_generation: u64,
    pub is_final: bool,
    pub phase: WakeWordPhase,
    pub wake_word_checked: bool,
    pub wake_word_detected: bool,
    pub clean_prompt: String,
    pub action: WakeWordAction,
    pub should_acknowledge: bool,
    pub is_prompt: bool,
    pub cooldown_active: bool,
    pub duplicate_suppressed: bool,
}

impl WakeWordDecision {
    fn ignored(
        stream: &str,
        is_final: bool,
        phase: WakeWordPhase,
        session_generation: u64,
        clean_prompt: String,
        cooldown_active: bool,
    ) -> Self {
        Self {
            stream: stream.to_string(),
            engine: WAKE_WORD_ENGINE,
            session_generation,
            is_final,
            phase,
            wake_word_checked: false,
            wake_word_detected: false,
            clean_prompt,
            action: WakeWordAction::Ignored,
            should_acknowledge: false,
            is_prompt: false,
            cooldown_active,
            duplicate_suppressed: false,
        }
    }
}

/// Deterministic partial/final wake-word state machine.
///
/// `handle_asr` is safe to call from the ASR callback for every WebSocket
/// message.  Partial messages may arm an utterance and request one
/// acknowledgement, but only final messages produce `PromptDetected` or
/// `PromptReceived`.  The caller supplies a monotonic millisecond timestamp
/// so tests and the native session callback share exactly the same cooldown
/// semantics without sleeping.
pub struct WakeWordStateMachine {
    config: WakeWordConfig,
    session_generation: u64,
    phase: WakeWordPhase,
    provisional_clean_prompt: String,
    acknowledgement_sent: bool,
    cooldown_until_ms: Option<u64>,
    last_final_key: Option<String>,
    last_final_ms: Option<u64>,
    last_final_wake_related: bool,
}

impl Default for WakeWordStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeWordStateMachine {
    pub fn new() -> Self {
        Self::with_config(WakeWordConfig::default())
    }

    pub fn with_config(config: WakeWordConfig) -> Self {
        Self {
            config,
            session_generation: 0,
            phase: WakeWordPhase::Idle,
            provisional_clean_prompt: String::new(),
            acknowledgement_sent: false,
            cooldown_until_ms: None,
            last_final_key: None,
            last_final_ms: None,
            last_final_wake_related: false,
        }
    }

    pub fn begin_session(&mut self, generation: u64) {
        self.session_generation = generation;
        self.reset_all();
    }

    pub fn on_generation_changed(&mut self, generation: u64) -> bool {
        if self.session_generation == generation {
            return false;
        }
        self.session_generation = generation;
        self.reset_all();
        true
    }

    pub fn set_config(&mut self, config: WakeWordConfig) {
        self.config = config;
        self.reset_all();
    }

    pub fn config(&self) -> &WakeWordConfig {
        &self.config
    }

    pub fn session_generation(&self) -> u64 {
        self.session_generation
    }

    pub fn phase(&self) -> WakeWordPhase {
        self.phase
    }

    pub fn is_armed(&self) -> bool {
        self.phase == WakeWordPhase::Armed
    }

    pub fn is_collecting_prompt(&self) -> bool {
        self.phase == WakeWordPhase::AwaitingPrompt
            || (self.phase == WakeWordPhase::Armed && self.provisional_clean_prompt.is_empty())
    }

    pub fn in_cooldown(&self, now_ms: u64) -> bool {
        self.cooldown_until_ms
            .map(|until| now_ms < until)
            .unwrap_or(false)
    }

    pub fn reset_on_silence(&mut self) {
        if self.phase == WakeWordPhase::Armed {
            self.phase = WakeWordPhase::Idle;
        }
        self.provisional_clean_prompt.clear();
        self.acknowledgement_sent = false;
    }

    pub fn reset_on_stop(&mut self) {
        self.reset_all();
    }

    fn reset_all(&mut self) {
        self.phase = WakeWordPhase::Idle;
        self.provisional_clean_prompt.clear();
        self.acknowledgement_sent = false;
        self.cooldown_until_ms = None;
        self.last_final_key = None;
        self.last_final_ms = None;
        self.last_final_wake_related = false;
    }

    /// Process one ASR result.
    pub fn handle_asr(
        &mut self,
        stream: &str,
        text: &str,
        is_final: bool,
        now_ms: u64,
    ) -> WakeWordDecision {
        let cooldown_active = self.in_cooldown(now_ms);

        // The ASR callback is shared by microphone and Discord streams.  A
        // non-mic result must never inspect or mutate the mic wake state.
        if stream != "mic" || self.config.mode == WakeWordMode::Disabled {
            return WakeWordDecision::ignored(
                stream,
                is_final,
                self.phase,
                self.session_generation,
                text.to_string(),
                cooldown_active,
            );
        }

        // Empty partial/final messages are valid transport messages but are
        // not wake-word candidates.  A final empty message still closes a
        // provisional armed utterance, while an already committed prompt
        // collection remains available for the next sufficient utterance.
        if text.trim().is_empty() {
            if is_final {
                self.finish_provisional();
            }
            return WakeWordDecision::ignored(
                stream,
                is_final,
                self.phase,
                self.session_generation,
                text.to_string(),
                self.in_cooldown(now_ms),
            );
        }

        // This explicit mode is useful for deployments that intentionally do
        // not require a wake word.  It still keeps partials provisional and
        // only promotes final text to a prompt.
        if self.config.mode == WakeWordMode::AllFinalSpeech {
            if !is_final {
                if self.phase == WakeWordPhase::AwaitingPrompt {
                    return self.decision(
                        stream,
                        false,
                        true,
                        false,
                        text.to_string(),
                        WakeWordAction::PartialPromptCandidate,
                        false,
                        true,
                        cooldown_active,
                        false,
                    );
                }
                return self.decision(
                    stream,
                    false,
                    false,
                    false,
                    text.to_string(),
                    WakeWordAction::Ignored,
                    false,
                    false,
                    cooldown_active,
                    false,
                );
            }

            self.finish_provisional();
            self.phase = WakeWordPhase::Idle;
            return self.decision(
                stream,
                true,
                false,
                false,
                text.to_string(),
                WakeWordAction::PromptReceived,
                false,
                true,
                self.in_cooldown(now_ms),
                false,
            );
        }

        let (matched, clean_prompt) =
            AsrEngine::check_wake_word(text, &self.config.custom_wake_words);

        if !is_final {
            if matched {
                if self.phase == WakeWordPhase::Armed {
                    // Whisper partials are revised frequently.  Keep the
                    // newest useful candidate, but never acknowledge twice.
                    if !clean_prompt.is_empty() {
                        self.provisional_clean_prompt = clean_prompt.clone();
                    }
                    return self.decision(
                        stream,
                        false,
                        true,
                        true,
                        clean_prompt,
                        WakeWordAction::PartialWakeDetected,
                        false,
                        false,
                        self.in_cooldown(now_ms),
                        true,
                    );
                }

                if cooldown_active {
                    // A residual/echo wake during the cooldown is observable
                    // but cannot arm another acknowledgement.
                    return self.decision(
                        stream,
                        false,
                        true,
                        true,
                        clean_prompt,
                        WakeWordAction::DuplicateSuppressed,
                        false,
                        false,
                        true,
                        true,
                    );
                }

                self.phase = WakeWordPhase::Armed;
                self.provisional_clean_prompt = clean_prompt.clone();
                // A partial that already contains trailing words is only a
                // candidate.  Acknowledge immediately only for a standalone
                // wake word; the final result owns prompt confirmation.
                let should_acknowledge = clean_prompt.is_empty();
                self.acknowledgement_sent = should_acknowledge;
                self.set_cooldown(now_ms);
                return self.decision(
                    stream,
                    false,
                    true,
                    true,
                    clean_prompt,
                    WakeWordAction::PartialWakeDetected,
                    should_acknowledge,
                    false,
                    true,
                    false,
                );
            }

            if self.phase == WakeWordPhase::AwaitingPrompt {
                return self.decision(
                    stream,
                    false,
                    true,
                    false,
                    text.to_string(),
                    WakeWordAction::PartialPromptCandidate,
                    false,
                    true,
                    cooldown_active,
                    false,
                );
            }

            return self.decision(
                stream,
                false,
                true,
                false,
                text.to_string(),
                WakeWordAction::Ignored,
                false,
                false,
                cooldown_active,
                false,
            );
        }

        let final_key = normalize_wake_word_key(text);
        let duplicate_final = !final_key.is_empty()
            && self.last_final_wake_related
            && self.last_final_key.as_deref() == Some(final_key.as_str())
            && self
                .last_final_ms
                .map(|last| now_ms.saturating_sub(last) <= self.config.cooldown_ms)
                .unwrap_or(false);
        if duplicate_final {
            self.finish_provisional();
            return self.decision(
                stream,
                true,
                true,
                matched,
                clean_prompt,
                WakeWordAction::DuplicateSuppressed,
                false,
                false,
                self.in_cooldown(now_ms),
                true,
            );
        }

        let was_armed = self.phase == WakeWordPhase::Armed;
        let was_waiting = self.phase == WakeWordPhase::AwaitingPrompt;
        let acknowledgement_was_sent = self.acknowledgement_sent;

        if matched {
            // A final result is the only point at which a prompt can be
            // promoted.  A partial arm therefore suppresses the final ack,
            // but never suppresses final prompt processing.
            if !was_armed && !was_waiting && cooldown_active {
                self.finish_provisional();
                return self.decision(
                    stream,
                    true,
                    true,
                    true,
                    clean_prompt,
                    WakeWordAction::DuplicateSuppressed,
                    false,
                    false,
                    true,
                    true,
                );
            }

            self.finish_provisional();
            self.set_cooldown(now_ms);
            self.remember_final(&final_key, now_ms, true);

            if clean_prompt.is_empty() {
                self.phase = WakeWordPhase::AwaitingPrompt;
                self.acknowledgement_sent = true;
                return self.decision(
                    stream,
                    true,
                    true,
                    true,
                    String::new(),
                    WakeWordAction::FinalWakeOnly,
                    !acknowledgement_was_sent,
                    false,
                    true,
                    false,
                );
            }

            self.phase = WakeWordPhase::Idle;
            return self.decision(
                stream,
                true,
                true,
                true,
                clean_prompt,
                WakeWordAction::PromptDetected,
                !acknowledgement_was_sent,
                true,
                true,
                false,
            );
        }

        self.finish_provisional();
        if was_waiting {
            self.phase = WakeWordPhase::Idle;
            self.remember_final(&final_key, now_ms, true);
            return self.decision(
                stream,
                true,
                true,
                false,
                text.to_string(),
                WakeWordAction::PromptReceived,
                false,
                true,
                self.in_cooldown(now_ms),
                false,
            );
        }

        self.phase = WakeWordPhase::Idle;
        self.decision(
            stream,
            true,
            true,
            false,
            text.to_string(),
            WakeWordAction::FinalSpeech,
            false,
            false,
            self.in_cooldown(now_ms),
            false,
        )
    }

    fn decision(
        &self,
        stream: &str,
        is_final: bool,
        wake_word_checked: bool,
        wake_word_detected: bool,
        clean_prompt: String,
        action: WakeWordAction,
        should_acknowledge: bool,
        is_prompt: bool,
        cooldown_active: bool,
        duplicate_suppressed: bool,
    ) -> WakeWordDecision {
        WakeWordDecision {
            stream: stream.to_string(),
            engine: WAKE_WORD_ENGINE,
            session_generation: self.session_generation,
            is_final,
            phase: self.phase,
            wake_word_checked,
            wake_word_detected,
            clean_prompt,
            action,
            should_acknowledge,
            is_prompt,
            cooldown_active,
            duplicate_suppressed,
        }
    }

    fn finish_provisional(&mut self) {
        if self.phase == WakeWordPhase::Armed {
            self.phase = WakeWordPhase::Idle;
        }
        self.provisional_clean_prompt.clear();
        self.acknowledgement_sent = false;
    }

    fn set_cooldown(&mut self, now_ms: u64) {
        self.cooldown_until_ms = Some(now_ms.saturating_add(self.config.cooldown_ms));
    }

    fn remember_final(&mut self, final_key: &str, now_ms: u64, wake_related: bool) {
        if final_key.is_empty() {
            self.last_final_key = None;
            self.last_final_ms = None;
            self.last_final_wake_related = false;
        } else {
            self.last_final_key = Some(final_key.to_string());
            self.last_final_ms = Some(now_ms);
            self.last_final_wake_related = wake_related;
        }
    }
}

pub struct WhisperWsClient {
    audio_tx: Mutex<Option<mpsc::UnboundedSender<AudioPacket>>>,
    cmd_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    child: Arc<Mutex<Option<Child>>>,
    lifecycle_lock: Arc<Mutex<()>>,
    process_lock: Arc<Mutex<()>>,
    #[cfg(windows)]
    job: Arc<Mutex<Option<AsrJobHandle>>>,
    callback: Arc<Mutex<Option<AsrCallback>>>,
    pending_embeds: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<Vec<f32>>>>>>,
    log_mgr: Arc<Mutex<Option<Arc<LogManager>>>>,
    is_started: Arc<AtomicBool>,
    is_ready: Arc<AtomicBool>,
    connection_active: Arc<AtomicBool>,
    readiness_notify: Arc<Notify>,
    connection_generation: Arc<AtomicU64>,
}

impl Default for WhisperWsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WhisperWsClient {
    pub fn new() -> Self {
        Self {
            audio_tx: Mutex::new(None),
            cmd_tx: Mutex::new(None),
            child: Arc::new(Mutex::new(None)),
            lifecycle_lock: Arc::new(Mutex::new(())),
            process_lock: Arc::new(Mutex::new(())),
            #[cfg(windows)]
            job: Arc::new(Mutex::new(None)),
            callback: Arc::new(Mutex::new(None)),
            pending_embeds: Arc::new(Mutex::new(HashMap::new())),
            log_mgr: Arc::new(Mutex::new(None)),
            is_started: Arc::new(AtomicBool::new(false)),
            is_ready: Arc::new(AtomicBool::new(false)),
            connection_active: Arc::new(AtomicBool::new(false)),
            readiness_notify: Arc::new(Notify::new()),
            connection_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_callback<F>(&self, on_result: F)
    where
        F: Fn(String, String, bool, Option<f64>) + Send + Sync + 'static,
    {
        *self.callback.lock() = Some(Arc::new(on_result));
    }

    /// Attach the application's logger so child stdout/stderr reaches the
    /// same in-memory/event stream as native diagnostics.
    pub fn set_log_manager(&self, log_mgr: Arc<LogManager>) {
        *self.log_mgr.lock() = Some(log_mgr);
    }

    /// True only while the exact WebSocket carrying this client's audio and
    /// command channels has completed its handshake.
    pub fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst)
    }

    /// テキスト一覧をローカル GLuCoSE-base-ja モデルで 768 次元ベクトル化
    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let req_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending_embeds.lock().insert(req_id.clone(), tx);

        let msg = serde_json::json!({
            "cmd": "embed",
            "id": req_id,
            "texts": texts,
        })
        .to_string();

        if let Some(ref sender) = *self.cmd_tx.lock() {
            sender.send(msg).map_err(|e| e.to_string())?;
        } else {
            return Err("WebSocket connection not active".to_string());
        }

        tokio::time::timeout(tokio::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| "Embedding timeout".to_string())?
            .map_err(|_| "Embedding channel dropped".to_string())
    }

    /// VRAM 事前確保 (1GB) の動的切り替え
    pub fn set_preallocate_vram(&self, enable: bool) -> Result<(), String> {
        let msg = serde_json::json!({
            "cmd": "preallocate_vram",
            "enable": enable,
        })
        .to_string();

        if let Some(ref sender) = *self.cmd_tx.lock() {
            sender.send(msg).map_err(|e| e.to_string())?;
            Ok(())
        } else {
            Err("WebSocket connection not active".to_string())
        }
    }

    /// CUDA INT8 Faster-Whisper WebSocket サーバーを起動し、ws://127.0.0.1:18088/asr に接続
    pub fn start<F>(&self, on_result: F) -> Result<(), String>
    where
        F: Fn(String, String, bool, Option<f64>) + Send + Sync + 'static,
    {
        // Startup warmup and session-start can arrive concurrently. Serialize
        // lifecycle transitions so a second caller cannot observe the short
        // window after is_started=true but before the child/channel stores are
        // installed and tear down the first worker.
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.set_callback(on_result);

        if self.is_started.load(Ordering::SeqCst) {
            // 既に起動済みの場合はコールバックの更新のみで即時有効化。
            // 子プロセスが先に終了していた場合だけ stale state を破棄し、
            // 次のブロックで新しい worker を起動する。
            let child_present = self.child.lock().is_some();
            if child_present && self.child_exit_reason().is_none() {
                return Ok(());
            }
            self.stop_inner();
        }

        self.stop_process_only_inner();
        self.is_ready.store(false, Ordering::SeqCst);

        let root_dir = crate::resolve_project_root();

        // Portable releases must never fall back to a checkout path or a
        // system-wide Python.  The bootstrap manager extracts this script and
        // creates the venv beside the EXE before ASR is started.
        let script_path = root_dir.join("scripts").join("asr_server.py");
        let python_path = root_dir.join("venv").join("Scripts").join("python.exe");

        if !script_path.exists() {
            eprintln!(
                "[ERROR] [ASR] ASR server script not found at: {:?}",
                script_path
            );
            return Err(format!("ASR server script not found at: {:?}", script_path));
        }
        if !python_path.exists() {
            eprintln!(
                "[ERROR] [ASR] Portable Python environment is not ready at: {:?}",
                python_path
            );
            return Err(format!(
                "Portable Python environment is not ready at: {:?}; run first-launch setup",
                python_path
            ));
        }

        if self.is_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // A stopped/failed worker may still have an old WebSocket task
        // winding down. The generation token prevents that task from
        // changing readiness after this new worker has connected.
        let connection_generation = self
            .connection_generation
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);

        let models_dir =
            crate::model_manager::ModelManager::get_effective_models_dir(&root_dir, None);
        let cache_dir = root_dir.join(".hf-cache");

        let mut cmd = Command::new(&python_path);
        cmd.arg(&script_path)
            .current_dir(&root_dir)
            .env("RUNTIME_ROOT", root_dir.to_string_lossy().to_string())
            .env(
                "SETTINGS_PATH",
                root_dir.join("settings.json").to_string_lossy().to_string(),
            )
            .env("MODELS_DIR", models_dir.to_string_lossy().to_string())
            .env("CACHE_DIR", cache_dir.to_string_lossy().to_string())
            .env("HF_HOME", cache_dir.to_string_lossy().to_string())
            .env(
                "TRANSFORMERS_CACHE",
                cache_dir.to_string_lossy().to_string(),
            )
            .env("HF_HUB_CACHE", cache_dir.to_string_lossy().to_string())
            .env(
                "HUGGINGFACE_HUB_CACHE",
                cache_dir.to_string_lossy().to_string(),
            )
            .env(
                "SENTENCE_TRANSFORMERS_HOME",
                cache_dir.to_string_lossy().to_string(),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.is_started.store(false, Ordering::SeqCst);
                return Err(format!("Failed to spawn ASR server: {}", error));
            }
        };

        // Put the Python parent in a kill-on-close Job Object before exposing
        // it to the async worker.  This covers grandchildren created by ML
        // libraries and also handles an abrupt GameAssistant process exit.
        #[cfg(windows)]
        let child_job = match attach_asr_job(&child) {
            Ok(job) => job,
            Err(error) => {
                terminate_child_tree(child);
                self.is_started.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };

        let child_log_mgr = self.log_mgr.lock().clone();
        record_child_line(
            &child_log_mgr,
            "stdout",
            &format!("Spawned ASR server child process (pid={})", child.id()),
        );

        if let Some(stdout) = child.stdout.take() {
            let log_mgr = child_log_mgr.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().filter_map(|l| l.ok()) {
                    record_child_line(&log_mgr, "stdout", &line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let log_mgr = child_log_mgr.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().filter_map(|l| l.ok()) {
                    record_child_line(&log_mgr, "stderr", &line);
                }
            });
        }

        {
            let _process_guard = self.process_lock.lock();
            *self.child.lock() = Some(child);
            #[cfg(windows)]
            {
                *self.job.lock() = Some(child_job);
            }
        }

        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<AudioPacket>();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();
        *self.audio_tx.lock() = Some(audio_tx);
        *self.cmd_tx.lock() = Some(cmd_tx);

        let callback_arc = self.callback.clone();
        let pending_embeds_arc = self.pending_embeds.clone();
        let ws_log_mgr = child_log_mgr.clone();
        let is_ready = self.is_ready.clone();
        let connection_active = self.connection_active.clone();
        let readiness_notify = self.readiness_notify.clone();
        let generation = self.connection_generation.clone();
        let is_started = self.is_started.clone();
        let child_store = self.child.clone();
        let process_lock = self.process_lock.clone();
        #[cfg(windows)]
        let job_store = self.job.clone();
        connection_active.store(true, Ordering::SeqCst);

        tauri::async_runtime::spawn(async move {
            let ws_url = ASR_WS_URL;
            let mut ws_stream = None;
            let startup_deadline = tokio::time::Instant::now() + ASR_WS_STARTUP_TIMEOUT;

            for attempt in 1..=ASR_WS_MAX_ATTEMPTS {
                let now = tokio::time::Instant::now();
                if now >= startup_deadline {
                    break;
                }
                tokio::time::sleep(ASR_WS_RETRY_DELAY.min(startup_deadline - now)).await;

                let remaining =
                    startup_deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let connect_timeout = ASR_WS_CONNECT_TIMEOUT.min(remaining);
                match tokio::time::timeout(connect_timeout, connect_async(ws_url)).await {
                    Ok(Ok((stream, _))) => {
                        record_child_line(
                            &ws_log_mgr,
                            "stdout",
                            "Successfully connected to Faster-Whisper CUDA INT8 WebSocket server",
                        );
                        ws_stream = Some(stream);
                        break;
                    }
                    Ok(Err(_)) | Err(_) => {
                        if attempt % 8 == 0 {
                            record_child_line(
                                &ws_log_mgr,
                                "stderr",
                                &format!(
                                    "Waiting for ASR WebSocket server (attempt {}/{}, connection timeout {:?})",
                                    attempt, ASR_WS_MAX_ATTEMPTS, connect_timeout
                                ),
                            );
                        }
                    }
                }
            }

            let ws_stream = match ws_stream {
                Some(s) => s,
                None => {
                    record_child_line(
                        &ws_log_mgr,
                        "stderr",
                        &format!(
                            "Failed to connect to Faster-Whisper ASR WebSocket server within {:?}",
                            ASR_WS_STARTUP_TIMEOUT
                        ),
                    );
                    if cleanup_failed_worker(
                        &is_started,
                        &is_ready,
                        &connection_active,
                        &readiness_notify,
                        &generation,
                        connection_generation,
                        &child_store,
                        &process_lock,
                        #[cfg(windows)]
                        &job_store,
                    ) {
                        record_child_line(
                            &ws_log_mgr,
                            "stderr",
                            "ASR server process cleanup completed after startup failure",
                        );
                    }
                    return;
                }
            };

            if generation.load(Ordering::SeqCst) != connection_generation {
                return;
            }
            // Readiness means the exact WebSocket connection used for audio
            // and commands is established, not merely that a second probe
            // connection happened to accept a handshake.
            is_ready.store(true, Ordering::SeqCst);
            readiness_notify.notify_waiters();

            let (mut ws_write, mut ws_read) = ws_stream.split();

            let mut sent_stream: Option<String> = None;
            let mut send_task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(audio_packet) = audio_rx.recv() => {
                            if sent_stream.as_deref() != Some(audio_packet.stream.as_str()) {
                                let stream_tag = serde_json::json!({
                                    "cmd": "audio_stream",
                                    "stream": &audio_packet.stream,
                                }).to_string();
                                if ws_write.send(Message::Text(stream_tag)).await.is_err() {
                                    break;
                                }
                                sent_stream = Some(audio_packet.stream);
                            }
                            if ws_write.send(Message::Binary(audio_packet.data)).await.is_err() {
                                break;
                            }
                        }
                        Some(cmd_str) = cmd_rx.recv() => {
                            let resets_stream_state = serde_json::from_str::<serde_json::Value>(&cmd_str)
                                .ok()
                                .and_then(|value| value.get("cmd").and_then(|cmd| cmd.as_str()).map(|cmd| cmd == "reset"))
                                .unwrap_or(false);
                            if ws_write.send(Message::Text(cmd_str)).await.is_err() {
                                break;
                            }
                            if resets_stream_state {
                                // The server resets its implicit stream to
                                // mic. Force the next packet to carry a fresh
                                // tag even if it uses the same stream as the
                                // previous packet before reset.
                                sent_stream = None;
                            }
                        }
                        else => break,
                    }
                }
            });

            tokio::select! {
                _ = &mut send_task => {
                    record_child_line(
                        &ws_log_mgr,
                        "stderr",
                        "ASR WebSocket send task stopped; terminating the server process",
                    );
                }
                _ = async {
                    while let Some(msg) = ws_read.next().await {
                        match msg {
                            Ok(Message::Text(txt)) => {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) {
                                    if let Some(msg_type) = val.get("type").and_then(|v| v.as_str()) {
                                        if msg_type == "embed_res" {
                                            if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                                                if let Some(resp_tx) = pending_embeds_arc.lock().remove(id)
                                                {
                                                    let mut vecs = Vec::new();
                                                    if let Some(raw_arr) =
                                                        val.get("vectors").and_then(|v| v.as_array())
                                                    {
                                                        for row in raw_arr {
                                                            if let Some(arr) = row.as_array() {
                                                                let f_row: Vec<f32> = arr
                                                                    .iter()
                                                                    .filter_map(|x| {
                                                                        x.as_f64().map(|f| f as f32)
                                                                    })
                                                                    .collect();
                                                                vecs.push(f_row);
                                                            }
                                                        }
                                                    }
                                                    let _ = resp_tx.send(vecs);
                                                }
                                            }
                                            continue;
                                        }
                                    }

                                    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
                                        let stream_name = val
                                            .get("stream")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("mic")
                                            .to_string();
                                        let is_final = val
                                            .get("is_final")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                        let latency_ms = val.get("latency_ms").and_then(|v| v.as_f64());
                                        if let Some(ref cb) = *callback_arc.lock() {
                                            cb(stream_name, text.to_string(), is_final, latency_ms);
                                        }
                                    }
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Err(e) => {
                                record_child_line(&ws_log_mgr, "stderr", &format!("ASR WebSocket read error: {}", e));
                                break;
                            }
                            _ => {}
                        }
                    }
                } => {}
            }

            send_task.abort();
            if cleanup_failed_worker(
                &is_started,
                &is_ready,
                &connection_active,
                &readiness_notify,
                &generation,
                connection_generation,
                &child_store,
                &process_lock,
                #[cfg(windows)]
                &job_store,
            ) {
                record_child_line(
                    &ws_log_mgr,
                    "stderr",
                    "ASR server process cleanup completed after WebSocket failure",
                );
            }
        });

        Ok(())
    }

    /// WebSocket 接続が成功するまで非同期で待機して完了を返す
    pub async fn warmup(&self) -> Result<(), String> {
        let started = self.is_started.load(Ordering::SeqCst);
        let child_present = self.child.lock().is_some();
        let connection_active = self.connection_active.load(Ordering::SeqCst);
        if !started
            || !child_present
            || (!self.is_ready.load(Ordering::SeqCst) && !connection_active)
        {
            if started {
                self.stop();
            }
            self.start(|_, _, _, _| {})?;
        }

        let startup_deadline = tokio::time::Instant::now() + ASR_WS_STARTUP_TIMEOUT;
        for _ in 0..ASR_WS_MAX_ATTEMPTS {
            if self.is_ready.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Some(reason) = self.child_exit_reason() {
                let log_mgr = self.log_mgr.lock().clone();
                record_child_line(&log_mgr, "stderr", &reason);
                // The Python process failed before the WebSocket worker could
                // finish its retry loop. Tear down the tracked handle/job now
                // instead of waiting for the configured cold-start deadline.
                self.stop();
                return Err(reason);
            }

            let now = tokio::time::Instant::now();
            if now >= startup_deadline {
                break;
            }
            let remaining = startup_deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                break;
            }
            let notified = self.readiness_notify.notified();
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep(ASR_WS_RETRY_DELAY.min(remaining)) => {},
            }
        }
        Err(format!(
            "ASR WebSocket server warmup timed out after {:?}",
            ASR_WS_STARTUP_TIMEOUT
        ))
    }

    fn child_exit_reason(&self) -> Option<String> {
        let mut child_guard = self.child.lock();
        let child = child_guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(format!(
                "ASR server child exited before WebSocket readiness (status {})",
                status
            )),
            Ok(None) => None,
            Err(error) => Some(format!(
                "Unable to inspect ASR server child process: {}",
                error
            )),
        }
    }

    /// Whisper GPU ワーカーを完全に停止して再起動
    pub async fn restart(&self) -> Result<(), String> {
        let cb_opt = self.callback.lock().clone();
        self.stop();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if let Some(cb) = cb_opt {
            self.start(move |s, t, f, lat| cb(s, t, f, lat))?;
        } else {
            self.start(|_, _, _, _| {})?;
        }

        self.warmup().await
    }

    /// f32 PCM サンプルをバイナリ（リトルエンディアン）に変換して WebSocket サーバーへ送信
    pub fn send_audio(&self, stream: &str, samples: &[f32]) {
        // Never let a disconnected/not-yet-ready worker accumulate an
        // unbounded audio backlog. Session start waits for readiness, and a
        // transient disconnect should drop audio until the worker is healthy.
        if !self.is_ready.load(Ordering::SeqCst) {
            return;
        }
        if let Some(ref tx) = *self.audio_tx.lock() {
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let _ = tx.send(AudioPacket {
                stream: stream.to_string(),
                data: bytes,
            });
        }
    }

    /// Clear the server-side rolling ASR buffer without tearing down the
    /// already warmed Whisper process. This keeps the next session isolated
    /// while preserving instant startup for repeated sessions.
    pub fn reset_audio(&self) {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            let _ = tx.send(serde_json::json!({ "cmd": "reset" }).to_string());
        }
    }

    /// Ask the VAD server to promote the current stream buffer to one final
    /// result.  This is the fallback for a short utterance that ended before
    /// the polling loop emitted a partial; the server owns the transcription
    /// and always preserves the `{ text, is_final, stream }` contract.
    pub fn flush_audio(&self, stream: &str) -> Result<(), String> {
        let stream = if stream.trim().is_empty() {
            "mic"
        } else {
            stream.trim()
        };
        let message = serde_json::json!({
            "cmd": "flush",
            "stream": stream,
        })
        .to_string();

        if let Some(ref sender) = *self.cmd_tx.lock() {
            sender.send(message).map_err(|error| error.to_string())
        } else {
            Err("WebSocket connection not active".to_string())
        }
    }

    pub fn stop_process_only(&self) {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.stop_process_only_inner();
    }

    fn stop_process_only_inner(&self) {
        self.is_ready.store(false, Ordering::SeqCst);
        self.connection_active.store(false, Ordering::SeqCst);
        self.readiness_notify.notify_waiters();
        *self.audio_tx.lock() = None;
        *self.cmd_tx.lock() = None;
        terminate_owned_process(
            &self.child,
            &self.process_lock,
            #[cfg(windows)]
            &self.job,
            None,
        );
    }

    pub fn stop(&self) {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.stop_inner();
    }

    fn stop_inner(&self) {
        self.is_started.store(false, Ordering::SeqCst);
        self.connection_generation.fetch_add(1, Ordering::SeqCst);
        self.stop_process_only_inner();
    }
}

/// 指定ポートをリッスンしている外部プロセス（ゾンビ Python 等）を強制終了
pub fn kill_process_on_port(port: u16) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(out) = Command::new("cmd")
            .args(["/C", &format!("netstat -ano -p tcp | findstr :{}", port)])
            .creation_flags(0x08000000)
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 && parts[1].ends_with(&format!(":{}", port)) {
                    if let Some(pid_str) = parts.last() {
                        if let Ok(pid) = pid_str.parse::<u32>() {
                            if pid > 0 && pid != std::process::id() {
                                taskkill_process_tree(pid);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for WhisperWsClient {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct CandleWhisperModel {
    pub model: Whisper,
    pub tokenizer: Tokenizer,
    pub config: Config,
    pub mel_filters: Vec<f32>,
    pub device: Device,
}

pub struct AsrEngine {
    cached_model: Arc<Mutex<Option<CandleWhisperModel>>>,
    pub ws_client: Arc<WhisperWsClient>,
    wake_word_state: Arc<Mutex<WakeWordStateMachine>>,
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrEngine {
    pub fn new() -> Self {
        Self {
            cached_model: Arc::new(Mutex::new(None)),
            ws_client: Arc::new(WhisperWsClient::new()),
            wake_word_state: Arc::new(Mutex::new(WakeWordStateMachine::new())),
        }
    }

    /// Configure the shared wake-word state used by the native session
    /// callback.  Configuration changes intentionally start a fresh state so
    /// an old session cannot leak its cooldown or prompt-collection mode.
    pub fn configure_wake_word(&self, config: WakeWordConfig) {
        self.wake_word_state.lock().set_config(config);
    }

    /// Start a new session generation and clear all provisional wake state.
    pub fn begin_wake_word_session(&self, generation: u64) {
        self.wake_word_state.lock().begin_session(generation);
    }

    /// Reset the detector when a newer session generation is observed.
    pub fn on_wake_word_generation_changed(&self, generation: u64) -> bool {
        self.wake_word_state
            .lock()
            .on_generation_changed(generation)
    }

    /// Release an incomplete utterance after VAD silence without cancelling a
    /// prompt collection that was already committed by a final wake-only
    /// result.
    pub fn reset_wake_word_on_silence(&self) {
        self.wake_word_state.lock().reset_on_silence();
    }

    /// Stop-session hook.  This clears the generation's cooldown, duplicate
    /// fingerprint, and prompt-collection state.
    pub fn reset_wake_word_on_stop(&self) {
        self.wake_word_state.lock().reset_on_stop();
    }

    /// Process one ASR partial/final result through the shared detector.
    pub fn handle_wake_word(
        &self,
        stream: &str,
        text: &str,
        is_final: bool,
        now_ms: u64,
    ) -> WakeWordDecision {
        self.wake_word_state
            .lock()
            .handle_asr(stream, text, is_final, now_ms)
    }

    /// f32 PCM サンプル列を 16kHz モノラル 16bit PCM WAV バイト列に変換
    pub fn pcm_to_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec)
                .map_err(|e| format!("WavWriter init error: {}", e))?;

            for &sample in samples {
                let clamped = sample.clamp(-1.0, 1.0);
                let sample_i16 = (clamped * 32767.0) as i16;
                writer
                    .write_sample(sample_i16)
                    .map_err(|e| format!("WavWriter write error: {}", e))?;
            }
            writer
                .finalize()
                .map_err(|e| format!("WavWriter finalize error: {}", e))?;
        }

        Ok(cursor.into_inner())
    }

    /// Pure Rust Native Whisper モデルのロード（Kotoba-Whisper-v2.0-faster）
    fn load_native_model() -> Result<CandleWhisperModel, String> {
        let device = Device::Cpu;
        let root_dir = crate::resolve_project_root();
        let models_dir =
            crate::model_manager::ModelManager::get_effective_models_dir(&root_dir, None);
        let local_dir = models_dir.join("kotoba-whisper-v2.0-faster");
        let config_path = local_dir.join("config.json");
        let tokenizer_path = local_dir.join("tokenizer.json");
        let weights_path = local_dir.join("model.safetensors");
        if !weights_path.exists() {
            return Err(format!(
                "Kotoba-Whisper model is not installed at {:?}; complete first-launch setup",
                local_dir
            ));
        }
        eprintln!(
            "[ASR-Native] Found local Kotoba-Whisper-v2.0-faster model directory at: {:?}",
            local_dir
        );

        let config_str = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config: {}", e))?;
        let config: Config = serde_json::from_str(&config_str)
            .map_err(|e| format!("Failed to parse config: {}", e))?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        eprintln!(
            "[ASR-Native] Loading weights from '{:?}' into memory...",
            weights_path
        );
        let vb = unsafe {
            candle_nn::VarBuilder::from_mmaped_safetensors(
                &[weights_path],
                candle_core::DType::F32,
                &device,
            )
            .map_err(|e| format!("Failed to load safetensors: {}", e))?
        };

        let model = Whisper::load(&vb, config.clone())
            .map_err(|e| format!("Failed to build whisper model: {}", e))?;

        let mel_bytes: &[u8] = if config.num_mel_bins == 128 {
            include_bytes!("../resources/melfilters128.bytes")
        } else {
            include_bytes!("../resources/melfilters.bytes")
        };

        let mut mel_filters = vec![0f32; mel_bytes.len() / 4];
        <byteorder::LittleEndian as byteorder::ByteOrder>::read_f32_into(
            mel_bytes,
            &mut mel_filters,
        );

        eprintln!(
            "[ASR-Native] Mel filters loaded from embedded binary ({} bins, {} floats)",
            config.num_mel_bins,
            mel_filters.len()
        );
        eprintln!("[ASR-Native] Kotoba-Whisper-v2.0-faster successfully loaded and cached on CPU!");

        Ok(CandleWhisperModel {
            model,
            tokenizer,
            config,
            mel_filters,
            device,
        })
    }

    /// モデルが未ロードならメモリに読み込み（ダミー推論は行わない）
    pub fn ensure_model_loaded(&self) -> Result<(), String> {
        let mut lock = self.cached_model.lock();
        if lock.is_none() {
            let loaded = Self::load_native_model()?;
            *lock = Some(loaded);
        }
        Ok(())
    }

    /// Pure Rust Native ASR 文字起こし（16kHz PCM サンプル配列から直接推論）
    pub fn transcribe_pcm_native(&self, samples: &[f32]) -> Result<String, String> {
        // 1. サンプル長 & 無音エネルギーチェック (無音時は Whisper を回さず即座に空文字を返す)
        if samples.len() < 3200 {
            // 200ms 未満の音声はスキップ
            return Ok(String::new());
        }

        let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();
        if rms < 0.003f32 {
            // 無音・極小ノイズは Whisper の幻覚（'ご' など）防止のためスキップ
            return Ok(String::new());
        }

        let mut lock = self.cached_model.lock();
        if lock.is_none() {
            let loaded = Self::load_native_model()?;
            *lock = Some(loaded);
        }

        let m_ref = lock.as_mut().unwrap();

        // 実際の発話長に合わせた最小限のパディング（160サンプル単位 & 偶数フレーム & 最大30秒）
        let hop_size = 160;
        let mut padded = samples.to_vec();
        if padded.len() > 480_000 {
            padded.truncate(480_000);
        }
        let remainder = padded.len() % hop_size;
        if remainder != 0 {
            padded.resize(padded.len() + (hop_size - remainder), 0.0f32);
        }
        let n_frames = padded.len() / hop_size;
        if !n_frames.is_multiple_of(2) {
            padded.resize(padded.len() + hop_size, 0.0f32);
        }

        let mel = audio::pcm_to_mel(&m_ref.config, &padded, &m_ref.mel_filters);
        let mel_len = mel.len();
        let mel_frames = mel_len / m_ref.config.num_mel_bins;
        let mel_tensor = Tensor::from_vec(
            mel,
            (1, m_ref.config.num_mel_bins, mel_frames),
            &m_ref.device,
        )
        .map_err(|e| format!("Tensor conversion error: {}", e))?;

        let mel_segment = if mel_frames > 3000 {
            mel_tensor
                .narrow(2, 0, 3000)
                .map_err(|e| format!("Mel narrow error: {}", e))?
        } else {
            mel_tensor
        };

        let enc = m_ref
            .model
            .encoder
            .forward(&mel_segment, true)
            .map_err(|e| format!("Encoder forward error: {}", e))?;

        m_ref.model.reset_kv_cache();

        // 日本語言語指定トークン (<|ja|>: 50266) を確実に含める
        let sot_token = m_ref
            .tokenizer
            .token_to_id("<|startoftranscript|>")
            .unwrap_or(50258);
        let ja_token = m_ref.tokenizer.token_to_id("<|ja|>").unwrap_or(50266);
        let transcribe_token = m_ref
            .tokenizer
            .token_to_id("<|transcribe|>")
            .unwrap_or(50360);
        let notimestamps_token = m_ref
            .tokenizer
            .token_to_id("<|notimestamps|>")
            .unwrap_or(50364);
        let eot_token = m_ref
            .tokenizer
            .token_to_id("<|endoftext|>")
            .unwrap_or(50257);

        let initial_tokens = vec![sot_token, ja_token, transcribe_token, notimestamps_token];
        let mut current_tokens = initial_tokens.clone();
        let mut generated_tokens = Vec::new();
        let mut repeat_count = 0;
        let mut last_tok = 0u32;

        for _step in 0..448 {
            let token_tensor = Tensor::new(current_tokens.as_slice(), &m_ref.device)
                .map_err(|e| format!("Token tensor error: {}", e))?
                .unsqueeze(0)
                .map_err(|e| format!("Unsqueeze error: {}", e))?;

            let ys = m_ref
                .model
                .decoder
                .forward(&token_tensor, &enc, true)
                .map_err(|e| format!("Decoder forward error: {}", e))?;
            let logits = m_ref
                .model
                .decoder
                .final_linear(&ys)
                .map_err(|e| format!("Decoder final linear error: {}", e))?;

            let (_, seq_len, _) = logits
                .dims3()
                .map_err(|e| format!("Logits shape error: {}", e))?;
            let next_token_logits = logits
                .i((0, seq_len - 1, ..))
                .map_err(|e| format!("Logits index error: {}", e))?;
            let next_token = next_token_logits
                .argmax(0)
                .map_err(|e| format!("Argmax error: {}", e))?
                .to_scalar::<u32>()
                .map_err(|e| format!("Scalar read error: {}", e))?;

            if next_token == eot_token {
                break;
            }

            if next_token == last_tok {
                repeat_count += 1;
                if repeat_count >= 5 {
                    break;
                }
            } else {
                repeat_count = 1;
                last_tok = next_token;
            }

            generated_tokens.push(next_token);
            current_tokens.push(next_token);
        }

        let decoded_text = if !generated_tokens.is_empty() {
            m_ref
                .tokenizer
                .decode(&generated_tokens, true)
                .map_err(|e| format!("Tokenizer decode error: {}", e))?
        } else {
            String::new()
        };

        // 連続するカンマやピリオド等の記号ノイズを除去
        let clean = decoded_text
            .replace(",,,,,", "")
            .replace(",,,,", "")
            .replace(",,,", "")
            .replace(",,", "")
            .trim()
            .to_string();

        // 1文字の記号ノイズ（'ご' や '.' などのみ）は除外
        let clean = if clean == "ご" || clean == "." || clean == "、" || clean == "。" {
            String::new()
        } else {
            clean
        };

        if !clean.is_empty() {
            eprintln!(
                "[ASR-Native] Transcribed ({} tokens): '{}'",
                generated_tokens.len(),
                clean
            );
        }

        Ok(clean)
    }

    /// ウェイクワードが含まれているか判定
    pub fn check_wake_word(text: &str, custom_wake_words: &[String]) -> (bool, String) {
        for wake_word in custom_wake_words {
            if let Some(clean_prompt) = remove_wake_word_source(text, wake_word) {
                return (true, clean_prompt);
            }
        }

        // Keep aliases in one list so callers using the legacy matcher and
        // the state machine share the exact same built-in vocabulary.
        let default_wake_words = [
            "ねえぐり",
            "ねぐり",
            "ネグリ",
            "ねーぐり",
            "ねぇぐり",
            "ね〜ぐり",
            "ね～ぐり",
            "ね~ぐり",
            "neguri",
            "アシスタント",
            "ヘイぐり",
        ];
        for wake_word in default_wake_words {
            if let Some(clean_prompt) = remove_wake_word_source(text, wake_word) {
                return (true, clean_prompt);
            }
        }

        (false, text.to_string())
    }
}

/// Normalize the characters used for wake-word matching while retaining the
/// byte range of every source character. Whisper may insert spaces between
/// kana, so whitespace is ignored for matching but remains part of the range
/// removed from the original transcription.
fn normalized_wake_word_chars(text: &str) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut normalized = Vec::new();
    let mut source_ranges = Vec::new();
    let mut previous = None;

    let mut source_chars = text.char_indices().peekable();
    while let Some((start, source_char)) = source_chars.next() {
        let end = start + source_char.len_utf8();
        let (source_end, normalized_piece) =
            if let Some(halfwidth_base) = normalize_halfwidth_kana(source_char) {
                let mut source_end = end;
                let mut mark = None;
                if let Some(&(mark_start, mark_char)) = source_chars.peek() {
                    if matches!(mark_char, 'ﾞ' | 'ﾟ') {
                        source_chars.next();
                        source_end = mark_start + mark_char.len_utf8();
                        mark = Some(mark_char);
                    }
                }
                let voiced = apply_halfwidth_diacritic(halfwidth_base, mark);
                (source_end, voiced.to_string())
            } else {
                let source_char = normalize_fullwidth_ascii(source_char);
                // Katakana conversion can produce a small hiragana character (for
                // example ェ -> ぇ); normalize once more so it follows the same
                // small-kana folding rule as a directly written hiragana source.
                let normalized_piece = normalize_kana(&normalize_kana(&source_char.to_string()));
                (end, normalized_piece)
            };

        for normalized_char in normalized_piece.chars() {
            if normalized_char.is_whitespace() {
                continue;
            }

            let normalized_char = if is_wake_long_mark(normalized_char) {
                long_vowel_for(previous)
            } else {
                normalized_char
            };
            normalized.push(normalized_char);
            source_ranges.push((start, source_end));
            previous = Some(normalized_char);
        }
    }

    canonicalize_wake_aliases(normalized, source_ranges)
}

fn normalize_fullwidth_ascii(source_char: char) -> char {
    match source_char {
        '\u{ff01}'..='\u{ff5e}' => {
            char::from_u32(source_char as u32 - 0xfee0).unwrap_or(source_char)
        }
        '\u{3000}' => ' ',
        _ => source_char,
    }
}

/// Convert half-width katakana to hiragana before applying the normal kana
/// folding rules.  The optional dakuten/handakuten is consumed by the caller
/// so a pair such as ｸﾞ maps to one source-aware character (ぐ).
fn normalize_halfwidth_kana(source_char: char) -> Option<char> {
    Some(match source_char {
        'ｦ' => 'を',
        'ｧ' => 'あ',
        'ｨ' => 'い',
        'ｩ' => 'う',
        'ｪ' => 'え',
        'ｫ' => 'お',
        'ｬ' => 'や',
        'ｭ' => 'ゆ',
        'ｮ' => 'よ',
        'ｯ' => 'つ',
        'ｰ' => 'ー',
        'ｱ' => 'あ',
        'ｲ' => 'い',
        'ｳ' => 'う',
        'ｴ' => 'え',
        'ｵ' => 'お',
        'ｶ' => 'か',
        'ｷ' => 'き',
        'ｸ' => 'く',
        'ｹ' => 'け',
        'ｺ' => 'こ',
        'ｻ' => 'さ',
        'ｼ' => 'し',
        'ｽ' => 'す',
        'ｾ' => 'せ',
        'ｿ' => 'そ',
        'ﾀ' => 'た',
        'ﾁ' => 'ち',
        'ﾂ' => 'つ',
        'ﾃ' => 'て',
        'ﾄ' => 'と',
        'ﾅ' => 'な',
        'ﾆ' => 'に',
        'ﾇ' => 'ぬ',
        'ﾈ' => 'ね',
        'ﾉ' => 'の',
        'ﾊ' => 'は',
        'ﾋ' => 'ひ',
        'ﾌ' => 'ふ',
        'ﾍ' => 'へ',
        'ﾎ' => 'ほ',
        'ﾏ' => 'ま',
        'ﾐ' => 'み',
        'ﾑ' => 'む',
        'ﾒ' => 'め',
        'ﾓ' => 'も',
        'ﾔ' => 'や',
        'ﾕ' => 'ゆ',
        'ﾖ' => 'よ',
        'ﾗ' => 'ら',
        'ﾘ' => 'り',
        'ﾙ' => 'る',
        'ﾚ' => 'れ',
        'ﾛ' => 'ろ',
        'ﾜ' => 'わ',
        'ﾝ' => 'ん',
        _ => return None,
    })
}

fn apply_halfwidth_diacritic(base: char, mark: Option<char>) -> char {
    match (base, mark) {
        ('う', Some('ﾞ')) => 'ゔ',
        ('か', Some('ﾞ')) => 'が',
        ('き', Some('ﾞ')) => 'ぎ',
        ('く', Some('ﾞ')) => 'ぐ',
        ('け', Some('ﾞ')) => 'げ',
        ('こ', Some('ﾞ')) => 'ご',
        ('さ', Some('ﾞ')) => 'ざ',
        ('し', Some('ﾞ')) => 'じ',
        ('す', Some('ﾞ')) => 'ず',
        ('せ', Some('ﾞ')) => 'ぜ',
        ('そ', Some('ﾞ')) => 'ぞ',
        ('た', Some('ﾞ')) => 'だ',
        ('ち', Some('ﾞ')) => 'ぢ',
        ('つ', Some('ﾞ')) => 'づ',
        ('て', Some('ﾞ')) => 'で',
        ('と', Some('ﾞ')) => 'ど',
        ('は', Some('ﾞ')) => 'ば',
        ('ひ', Some('ﾞ')) => 'び',
        ('ふ', Some('ﾞ')) => 'ぶ',
        ('へ', Some('ﾞ')) => 'べ',
        ('ほ', Some('ﾞ')) => 'ぼ',
        ('は', Some('ﾟ')) => 'ぱ',
        ('ひ', Some('ﾟ')) => 'ぴ',
        ('ふ', Some('ﾟ')) => 'ぷ',
        ('へ', Some('ﾟ')) => 'ぺ',
        ('ほ', Some('ﾟ')) => 'ぽ',
        _ => base,
    }
}

fn is_wake_long_mark(source_char: char) -> bool {
    matches!(source_char, 'ー' | '〜' | '～' | '-' | '~')
}

/// Expand a Japanese long-vowel mark according to the preceding kana. This
/// makes ねーぐり / ね〜ぐり equivalent to ねえぐり while keeping the rule
/// useful for custom kana wake words as well.
fn long_vowel_for(previous: Option<char>) -> char {
    match previous {
        Some(
            'あ' | 'か' | 'が' | 'さ' | 'ざ' | 'た' | 'だ' | 'な' | 'は' | 'ば' | 'ぱ' | 'ま'
            | 'や' | 'ら' | 'わ' | 'ぁ' | 'ゃ',
        ) => 'あ',
        Some(
            'い' | 'き' | 'ぎ' | 'し' | 'じ' | 'ち' | 'ぢ' | 'に' | 'ひ' | 'び' | 'ぴ' | 'み'
            | 'り' | 'ゐ' | 'ぃ' | 'ゅ',
        ) => 'い',
        Some(
            'う' | 'く' | 'ぐ' | 'す' | 'ず' | 'つ' | 'づ' | 'ぬ' | 'ふ' | 'ぶ' | 'ぷ' | 'む'
            | 'ゆ' | 'る' | 'ゔ' | 'ぅ',
        ) => 'う',
        Some(
            'え' | 'け' | 'げ' | 'せ' | 'ぜ' | 'て' | 'で' | 'ね' | 'へ' | 'べ' | 'ぺ' | 'め'
            | 'れ' | 'ゑ' | 'ぇ',
        ) => 'え',
        Some(
            'お' | 'こ' | 'ご' | 'そ' | 'ぞ' | 'と' | 'ど' | 'の' | 'ほ' | 'ぼ' | 'ぽ' | 'も'
            | 'よ' | 'ろ' | 'を' | 'ぉ',
        ) => 'お',
        _ => 'ー',
    }
}

/// Collapse the known Japanese/Latin aliases to one matching key. The source
/// range is carried over the whole alias so cleanup still removes exactly the
/// characters Whisper returned, including any spaces between kana.
fn canonicalize_wake_aliases(
    normalized: Vec<char>,
    source_ranges: Vec<(usize, usize)>,
) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut canonical = Vec::with_capacity(normalized.len());
    let mut canonical_ranges = Vec::with_capacity(source_ranges.len());
    let mut index = 0;

    while index < normalized.len() {
        let alias_len = if normalized[index..].starts_with(&['ね', 'え', 'ぐ', 'り']) {
            Some(4)
        } else if normalized[index..].starts_with(&['n', 'e', 'g', 'u', 'r', 'i']) {
            Some(6)
        } else {
            None
        };

        if let Some(alias_len) = alias_len {
            let range = (
                source_ranges[index].0,
                source_ranges[index + alias_len - 1].1,
            );
            canonical.extend(['ね', 'ぐ', 'り']);
            canonical_ranges.extend([range; 3]);
            index += alias_len;
        } else {
            canonical.push(normalized[index]);
            canonical_ranges.push(source_ranges[index]);
            index += 1;
        }
    }

    (canonical, canonical_ranges)
}

fn normalize_wake_word_key(text: &str) -> String {
    normalized_wake_word_chars(text).0.into_iter().collect()
}

/// Return the source byte range corresponding to a normalized wake word.
/// Matching is kana/case normalized and whitespace-insensitive, while source
/// offsets make cleanup work for the exact phrase Whisper returned.
fn wake_word_source_range(text: &str, wake_word: &str) -> Option<(usize, usize)> {
    let (normalized_text, source_ranges) = normalized_wake_word_chars(text);
    let (normalized_wake_word, _) = normalized_wake_word_chars(wake_word);

    if normalized_wake_word.is_empty() || normalized_wake_word.len() > normalized_text.len() {
        return None;
    }

    normalized_text
        .windows(normalized_wake_word.len())
        .position(|window| window == normalized_wake_word.as_slice())
        .map(|start| {
            let end = start + normalized_wake_word.len() - 1;
            (source_ranges[start].0, source_ranges[end].1)
        })
}

/// Remove the source phrase that matched a wake word. Using the matched byte
/// range (instead of `str::replace`) keeps cleanup consistent when the
/// transcription uses a different kana form or inserts spaces.
fn remove_wake_word_source(text: &str, wake_word: &str) -> Option<String> {
    let (start, end) = wake_word_source_range(text, wake_word)?;
    let mut clean_prompt = String::with_capacity(text.len() - (end - start));
    clean_prompt.push_str(&text[..start]);
    clean_prompt.push_str(&text[end..]);
    Some(clean_prompt.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_wake_word_removes_a_spaced_kana_source_phrase() {
        let custom = vec!["ねえぐり".to_string()];

        let result = AsrEngine::check_wake_word("ネ エ グ リ 今日はどう?", &custom);

        assert_eq!(result, (true, "今日はどう?".to_string()));
    }

    #[test]
    fn default_wake_word_matches_kana_and_space_variants() {
        let result = AsrEngine::check_wake_word("ネグ リ 明日の天気は?", &[]);

        assert_eq!(result, (true, "明日の天気は?".to_string()));
    }

    #[test]
    fn wake_word_only_returns_an_empty_prompt() {
        let custom = vec!["ねえぐり".to_string()];

        let result = AsrEngine::check_wake_word("ねえ ぐり", &custom);

        assert_eq!(result, (true, String::new()));
    }

    #[test]
    fn one_breath_wake_word_prompt_is_left_after_source_phrase_removal() {
        let custom = vec!["ねえぐり".to_string()];

        let (matched, clean_prompt) =
            AsrEngine::check_wake_word("ネェグリ ゲームを起動して", &custom);

        assert!(matched);
        assert_eq!(clean_prompt, "ゲームを起動して");
        assert!(clean_prompt.chars().count() >= 2);
    }

    #[test]
    fn partial_short_wake_word_arms_once_before_final() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(7);

        let empty = detector.handle_asr("mic", "   ", false, 0);
        assert_eq!(empty.action, WakeWordAction::Ignored);
        assert!(!empty.wake_word_checked);
        assert!(!empty.wake_word_detected);

        let first = detector.handle_asr("mic", "ねえぐり", false, 1);
        assert_eq!(first.session_generation, 7);
        assert!(first.wake_word_detected);
        assert!(first.should_acknowledge);
        assert!(detector.is_armed());

        let repeated = detector.handle_asr("mic", "ねえぐり", false, 100);
        assert!(repeated.wake_word_detected);
        assert!(!repeated.should_acknowledge);
        assert!(repeated.duplicate_suppressed);
        assert!(detector.is_armed());
    }

    #[test]
    fn final_without_partial_still_detects_wake_and_prompt() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(8);

        let result = detector.handle_asr("mic", "ねえぐり 明日の天気は?", true, 0);
        assert_eq!(result.action, WakeWordAction::PromptDetected);
        assert!(result.wake_word_checked);
        assert!(result.wake_word_detected);
        assert!(result.is_prompt);
        assert!(result.should_acknowledge);
        assert_eq!(result.clean_prompt, "明日の天気は?");

        let duplicate = detector.handle_asr("mic", "ねえぐり 明日の天気は?", true, 100);
        assert_eq!(duplicate.action, WakeWordAction::DuplicateSuppressed);
        assert!(duplicate.duplicate_suppressed);
        assert!(!duplicate.is_prompt);
    }

    #[test]
    fn partial_and_final_one_breath_prompt_produce_one_final_action() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(9);

        let partial_wake = detector.handle_asr("mic", "ねえぐり", false, 0);
        assert_eq!(partial_wake.action, WakeWordAction::PartialWakeDetected);
        assert!(partial_wake.should_acknowledge);

        let partial_prompt = detector.handle_asr("mic", "ねえぐり ゲームを起動して", false, 100);
        assert_eq!(partial_prompt.action, WakeWordAction::PartialWakeDetected);
        assert!(!partial_prompt.should_acknowledge);
        assert!(partial_prompt.duplicate_suppressed);

        let final_prompt = detector.handle_asr("mic", "ねえぐり ゲームを起動して", true, 800);
        assert_eq!(final_prompt.action, WakeWordAction::PromptDetected);
        assert!(!final_prompt.should_acknowledge);
        assert_eq!(final_prompt.clean_prompt, "ゲームを起動して");

        let duplicate_final = detector.handle_asr("mic", "ねえぐり ゲームを起動して", true, 900);
        assert_eq!(duplicate_final.action, WakeWordAction::DuplicateSuppressed);
        assert!(duplicate_final.duplicate_suppressed);
    }

    #[test]
    fn partial_wake_with_trailing_prompt_is_only_a_candidate_until_final() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(90);

        let partial = detector.handle_asr("mic", "ねえぐり ゲームを起動して", false, 0);
        assert_eq!(partial.action, WakeWordAction::PartialWakeDetected);
        assert!(partial.wake_word_detected);
        assert!(!partial.is_prompt);
        assert!(!partial.should_acknowledge);
        assert!(detector.is_armed());

        let final_result = detector.handle_asr("mic", "ねえぐり ゲームを起動して", true, 700);
        assert_eq!(final_result.action, WakeWordAction::PromptDetected);
        assert!(final_result.is_prompt);
        assert!(final_result.should_acknowledge);
        assert_eq!(final_result.clean_prompt, "ゲームを起動して");
    }

    #[test]
    fn wake_only_commits_prompt_collection_and_next_final_is_prompt() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(10);

        let partial = detector.handle_asr("mic", "ねえぐり", false, 0);
        assert!(partial.should_acknowledge);
        assert!(detector.is_armed());
        assert!(detector.is_collecting_prompt());

        let wake_only = detector.handle_asr("mic", "ねえぐり", true, 700);
        assert_eq!(wake_only.action, WakeWordAction::FinalWakeOnly);
        assert!(!wake_only.should_acknowledge);
        assert!(detector.is_collecting_prompt());

        let prompt = detector.handle_asr("mic", "続きの指示", true, 1_000);
        assert_eq!(prompt.action, WakeWordAction::PromptReceived);
        assert!(prompt.is_prompt);
        assert_eq!(prompt.clean_prompt, "続きの指示");
        assert!(!detector.is_collecting_prompt());
    }

    #[test]
    fn silence_stop_and_generation_change_release_provisional_state() {
        let mut detector = WakeWordStateMachine::new();
        detector.begin_session(11);
        let _ = detector.handle_asr("mic", "ねえぐり", false, 0);
        detector.reset_on_silence();
        assert!(!detector.is_armed());
        assert_eq!(detector.phase(), WakeWordPhase::Idle);

        let still_cooling = detector.handle_asr("mic", "ねえぐり", false, 100);
        assert_eq!(still_cooling.action, WakeWordAction::DuplicateSuppressed);
        let rearmed = detector.handle_asr("mic", "ねえぐり", false, 1_800);
        assert_eq!(rearmed.action, WakeWordAction::PartialWakeDetected);
        assert!(rearmed.should_acknowledge);

        detector.reset_on_stop();
        assert_eq!(detector.phase(), WakeWordPhase::Idle);
        assert!(!detector.in_cooldown(1_800));

        let _ = detector.handle_asr("mic", "ねえぐり", false, 2_000);
        assert!(detector.is_armed());
        assert!(detector.on_generation_changed(12));
        assert_eq!(detector.phase(), WakeWordPhase::Idle);
        assert!(!detector.is_collecting_prompt());
        assert!(!detector.in_cooldown(2_001));
    }

    #[test]
    fn discord_wake_word_is_ignored_without_mutating_mic_state() {
        let mut detector = WakeWordStateMachine::new();

        let discord = detector.handle_asr("discord", "ねえぐり", false, 0);
        assert_eq!(discord.action, WakeWordAction::Ignored);
        assert!(!discord.wake_word_checked);
        assert!(!discord.wake_word_detected);
        assert!(!detector.is_armed());

        let mic = detector.handle_asr("mic", "ねえぐり", false, 0);
        assert_eq!(mic.action, WakeWordAction::PartialWakeDetected);
        assert!(mic.should_acknowledge);
    }

    #[test]
    fn default_and_custom_wake_words_share_kana_long_space_width_and_latin_rules() {
        let variants = [
            "ねえぐり",
            "ねぐり",
            "ネグリ",
            "ねーぐり",
            "ねぇぐり",
            "ね〜ぐり",
            "neguri",
            "ＮＥＧＵＲＩ",
            "ﾈｸﾞﾘ",
            "ﾈｰｸﾞﾘ",
        ];
        let custom = vec![" ねえぐり ".to_string()];

        for variant in variants {
            let source = format!("  {variant}  ゲームを起動して");
            let result = AsrEngine::check_wake_word(&source, &custom);
            assert_eq!(result, (true, "ゲームを起動して".to_string()), "{variant}");
        }

        let custom_latin = vec!["ＮＥＧＵＲＩ".to_string()];
        let result = AsrEngine::check_wake_word("ネ エ グ リ 今日は?", &custom_latin);
        assert_eq!(result, (true, "今日は?".to_string()));
    }

    #[test]
    fn asr_engine_wrapper_exposes_state_machine_for_session_callbacks() {
        let engine = AsrEngine::new();
        engine.configure_wake_word(WakeWordConfig::new(vec!["ねえぐり".to_string()]));
        engine.begin_wake_word_session(42);

        let partial = engine.handle_wake_word("mic", "ねえぐり", false, 0);
        assert!(partial.wake_word_detected);
        assert!(partial.should_acknowledge);

        let final_result = engine.handle_wake_word("mic", "ねえぐり 起動", true, 700);
        assert_eq!(final_result.action, WakeWordAction::PromptDetected);
        assert_eq!(final_result.clean_prompt, "起動");
    }

    #[test]
    fn flush_audio_exposes_the_final_only_vad_fallback_command() {
        let client = WhisperWsClient::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        *client.cmd_tx.lock() = Some(tx);

        client
            .flush_audio(" discord ")
            .expect("flush command must enqueue");

        let command = rx.try_recv().expect("flush command missing");
        let value: serde_json::Value = serde_json::from_str(&command).expect("valid JSON");
        assert_eq!(value["cmd"], "flush");
        assert_eq!(value["stream"], "discord");
    }

    #[test]
    fn empty_wake_word_policy_is_explicitly_selectable() {
        let config = WakeWordConfig::default()
            .with_mode(WakeWordMode::AllFinalSpeech)
            .with_cooldown_ms(0);
        let mut detector = WakeWordStateMachine::with_config(config);

        let partial = detector.handle_asr("mic", "通常の暫定文", false, 0);
        assert_eq!(partial.action, WakeWordAction::Ignored);
        assert!(!partial.wake_word_checked);

        let final_result = detector.handle_asr("mic", "通常の確定文", true, 1);
        assert_eq!(final_result.action, WakeWordAction::PromptReceived);
        assert!(final_result.is_prompt);

        detector.set_config(WakeWordConfig::default().with_mode(WakeWordMode::Disabled));
        let disabled = detector.handle_asr("mic", "ねえぐり 起動", true, 2);
        assert_eq!(disabled.action, WakeWordAction::Ignored);
        assert!(!disabled.wake_word_checked);
    }

    #[test]
    fn test_transcribe_dummy_audio() {
        let engine = AsrEngine::new();
        let dummy = vec![0.0f32; 16000]; // 1秒の無音
        let result = engine.transcribe_pcm_native(&dummy);
        assert!(result.is_ok(), "Transcription failed: {:?}", result);
    }

    #[test]
    fn test_transcribe_wav_file() {
        let wav_path = "J:\\Train\\wav\\Kota\\0001.wav";
        if !std::path::Path::new(wav_path).exists() {
            println!("WAV file not found at: {}", wav_path);
            return;
        }

        let mut reader = hound::WavReader::open(wav_path).expect("Failed to open WAV file");
        let spec = reader.spec();
        println!(
            "WAV spec: sample_rate={}, channels={}, bits_per_sample={}",
            spec.sample_rate, spec.channels, spec.bits_per_sample
        );

        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.unwrap() as f32 / max_val)
                    .collect()
            }
            hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        };

        // モノラル 16kHz にリサンプリング
        let mono_samples = if spec.channels > 1 {
            samples
                .chunks(spec.channels as usize)
                .map(|ch| ch[0])
                .collect()
        } else {
            samples
        };

        let resampled = if spec.sample_rate != 16000 {
            crate::audio_input::resample_linear(&mono_samples, spec.sample_rate, 16000, 1)
        } else {
            mono_samples
        };

        let start = std::time::Instant::now();
        let engine = AsrEngine::new();
        let result = engine.transcribe_pcm_native(&resampled);
        let elapsed = start.elapsed();

        println!("==========================================");
        println!("WAV Transcription Test for: {}", wav_path);
        println!("Audio duration: {:.2}s", resampled.len() as f32 / 16000.0);
        println!("Inference time: {:?}", elapsed);
        println!("Result: {:?}", result);
        println!("==========================================");

        assert!(result.is_ok());
    }

    #[test]
    fn test_whisper_ws_client() {
        let client = WhisperWsClient::new();
        let (tx, rx) = std::sync::mpsc::channel();

        let res = client.start(move |stream, text, is_final, _latency| {
            println!(
                "[TEST-CALLBACK] Stream: {}, Text: '{}', Final: {}",
                stream, text, is_final
            );
            let _ = tx.send((stream, text, is_final));
        });

        assert!(res.is_ok(), "Failed to start WS client: {:?}", res);

        // 0001.wav を読み込んで投入
        let wav_path = "J:\\Train\\wav\\Kota\\0001.wav";
        if std::path::Path::new(wav_path).exists() {
            let mut reader = hound::WavReader::open(wav_path).unwrap();
            let spec = reader.spec();
            let samples: Vec<f32> = match spec.sample_format {
                hound::SampleFormat::Int => {
                    let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
                    reader
                        .samples::<i32>()
                        .map(|s| s.unwrap() as f32 / max_val)
                        .collect()
                }
                hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
            };
            let mono_samples = if spec.channels > 1 {
                samples
                    .chunks(spec.channels as usize)
                    .map(|ch| ch[0])
                    .collect()
            } else {
                samples
            };
            let resampled = if spec.sample_rate != 16000 {
                crate::audio_input::resample_linear(&mono_samples, spec.sample_rate, 16000, 1)
            } else {
                mono_samples
            };

            // サーバー起動・CUDA ロード・WebSocket 接続完了を確実に待機
            let rt = tokio::runtime::Runtime::new().unwrap();
            let warmup_res = rt.block_on(client.warmup());
            println!("[TEST-WARMUP] Warmup result: {:?}", warmup_res);
            assert!(warmup_res.is_ok());

            // 0.2 秒チャンク（3200サンプル）ずつ順次送信
            for chunk in resampled.chunks(3200) {
                client.send_audio("mic", chunk);
                std::thread::sleep(std::time::Duration::from_millis(40));
            }

            // 結果を待機
            let mut got_any = false;
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(10) {
                if let Ok((_stream, text, is_final)) =
                    rx.recv_timeout(std::time::Duration::from_millis(500))
                {
                    println!("[TEST-RECV] Text: '{}', is_final: {}", text, is_final);
                    got_any = true;
                    if is_final {
                        break;
                    }
                }
            }
            client.stop();
            assert!(got_any, "Expected transcription result within timeout");
        }
    }

    #[tokio::test]
    async fn ws_probe_is_bounded_when_server_accepts_without_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accept_task = tokio::spawn(async move {
            if let Ok((_socket, _)) = listener.accept().await {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        let url = format!("ws://{address}/asr");
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            probe_ws(&url, std::time::Duration::from_millis(50)),
        )
        .await
        .expect("WebSocket probe must not hang on a stalled handshake");

        assert!(
            !result,
            "a stalled handshake must not be reported as connected"
        );
        accept_task.abort();
    }

    #[test]
    fn startup_timeout_has_margin_for_cold_cuda_model_load() {
        // A cold Faster-Whisper CUDA load has already been observed to finish
        // at roughly the old 60-second deadline.  Keep a margin so the retry
        // loop cannot kill a healthy server just as it begins listening.
        assert!(
            ASR_WS_STARTUP_TIMEOUT >= std::time::Duration::from_secs(120),
            "ASR startup timeout must leave margin beyond a 60-second cold model load"
        );
        assert!(
            ASR_WS_MAX_ATTEMPTS as u128 * ASR_WS_RETRY_DELAY.as_millis() as u128
                >= ASR_WS_STARTUP_TIMEOUT.as_millis() as u128,
            "ASR retry budget must cover the complete startup timeout"
        );
    }

    #[test]
    fn child_output_is_forwarded_to_the_application_console_log() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-asr-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let log_mgr = Arc::new(LogManager::new(root.clone()));
        let sink = Some(log_mgr.clone());

        record_child_line(&sink, "stderr", "ModuleNotFoundError: pkg_resources");
        record_child_line(&sink, "stdout", "ASR WebSocket Server running");

        let entries = log_mgr.get_logs();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].logger, "ASR-Server");
        assert_eq!(entries[0].level, "ERROR");
        assert!(entries[0].message.contains("pkg_resources"));
        assert_eq!(entries[1].level, "INFO");
        assert!(root.join("data").join("app.log").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn connection_failure_cleanup_kills_owned_asr_process() {
        use std::os::windows::process::CommandExt;

        let client = WhisperWsClient::new();
        let child = Command::new("cmd.exe")
            .args(["/C", "ping", "127.0.0.1", "-n", "60"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a long-lived ASR test child");
        let pid = child.id();
        *client.child.lock() = Some(child);
        let job = attach_asr_job(client.child.lock().as_ref().expect("tracked child"))
            .expect("attach ASR child to kill-on-close job");
        *client.job.lock() = Some(job);

        cleanup_failed_worker(
            &client.is_started,
            &client.is_ready,
            &client.connection_active,
            &client.readiness_notify,
            &client.connection_generation,
            0,
            &client.child,
            &client.process_lock,
            &client.job,
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let listing = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {}", pid), "/NH"])
                .output()
                .expect("inspect test child");
            let output = String::from_utf8_lossy(&listing.stdout);
            if !output.contains(&pid.to_string()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let listing = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .expect("inspect terminated test child");
        let output = String::from_utf8_lossy(&listing.stdout);
        assert!(
            !output.contains(&pid.to_string()),
            "ASR process {} survived connection-failure cleanup: {}",
            pid,
            output
        );
        assert!(client.child.lock().is_none());
        assert!(client.job.lock().is_none());
    }
}
