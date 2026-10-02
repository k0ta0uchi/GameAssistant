use std::sync::atomic::Ordering;
use std::time::Instant;
use chrono::{Local, Utc};
use tauri::{AppHandle, Emitter};

use crate::asr::{
    WakeWordAction, WakeWordConfig, WakeWordDecision, WakeWordMode, WakeWordPhase,
};

use super::ai_pipeline::{ai_error_reason, emit_toast_notice};
use super::lifecycle::session_context_fields;
use super::persistence::{
    admit_redacted_memory_text_with_reason, memory_event_log_message, raw_admission_drop_message,
};
use super::types::*;
use super::SessionManager;

pub(crate) fn effective_wake_words(settings: &serde_json::Value) -> Vec<String> {
    let mut words: Vec<String> = match settings.get("custom_wake_words") {
        None => crate::asr::DEFAULT_WAKE_WORDS
            .iter()
            .map(|word| word.to_string())
            .collect(),
        Some(value) => value
            .as_str()
            .map(|text| {
                text.split([',', '、'])
                    .map(str::trim)
                    .filter(|word| !word.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .or_else(|| {
                value.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|word| word.as_str())
                        .map(|word| word.trim().to_string())
                        .filter(|word| !word.is_empty())
                        .collect()
                })
            })
            .unwrap_or_default(),
    };
    let mut seen = std::collections::HashSet::new();
    words.retain(|word| seen.insert(word.clone()));
    words
}

/// Twitch コメント本文を有効ウェイクワードで照合し、呼びかけを除去した本文を
/// 返す。一致なし・ウェイクワードのみ (空プロンプト) の場合は None を返す。

pub(crate) fn admit_twitch_comment(wake_words: &[String], body: &str) -> Option<String> {
    let clean = crate::asr::match_wake_word_in_source(body, wake_words)?;
    let clean = clean.trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

pub(crate) fn wake_word_config_from_settings(settings: &serde_json::Value) -> (WakeWordConfig, bool) {
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

pub(crate) fn prompt_collection_from_decision(decision: &WakeWordDecision) -> bool {
    matches!(decision.phase, WakeWordPhase::AwaitingPrompt)
        || (decision.phase == WakeWordPhase::Armed && decision.clean_prompt.trim().is_empty())
}

pub(crate) fn stale_wake_word_decision(
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

pub(crate) fn wake_decision_log_message(
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


impl SessionManager {
    pub(crate) fn admit_asr_callback(&self, context: &SessionContext, is_final: bool) -> bool {
        if self.is_current_session(context) {
            return true;
        }
        let status = if is_final {
            "stale_asr_final_accepted_for_raw"
        } else {
            "stale_asr_callback_dropped"
        };
        self.log_mgr.info(
            "Session",
            &format!(
                "session_id={} generation={} status={}",
                context.session_id, context.generation, status
            ),
        );
        self.log_mgr.info(
            "TTS",
            &format!(
                "session_id={} generation={} event_id=- status=nod_not_requested phase={} reason=stale_callback",
                context.session_id,
                context.generation,
                if is_final { "final" } else { "partial" }
            ),
        );
        is_final
    }

    pub(crate) fn record_input_drop(
        &self,
        context: Option<&SessionContext>,
        event_id: &str,
        reason: InputDropReason,
        app_handle: Option<&AppHandle>,
    ) {
        let (session_id, generation) = session_context_fields(self, context);
        self.log_mgr.warn(
            "AI",
            &format!(
                "session_id={} generation={} event_id={} status=input_dropped reason={}",
                session_id,
                generation,
                event_id,
                reason.code()
            ),
        );
        let message = match reason {
            InputDropReason::StaleAtEntry
            | InputDropReason::StaleAfterMemorySearch
            | InputDropReason::StaleAfterWebSearch
            | InputDropReason::StaleAfterScreenCapture
            | InputDropReason::StaleBeforeGeneration
            | InputDropReason::StaleAfterGeneration
            | InputDropReason::StaleBeforeTts
            | InputDropReason::StaleAfterTts => {
                "⚠️ セッションが切り替わったため、発話の応答を破棄しました。"
            }
            InputDropReason::WakeWordRequired => {
                "ℹ️ Wake Wordを含まない発話はGeminiへ送信しませんでした。"
            }
            InputDropReason::WakeOnly => {
                "ℹ️ Wake Wordを受け付けました。続けて質問を話してください。"
            }
            InputDropReason::DuplicateSuppressed => "ℹ️ 重複した発話を抑制しました。",
            InputDropReason::StopWord => "ℹ️ 停止語を受け付け、発話を停止しました。",
            InputDropReason::EmptyPrompt | InputDropReason::EmptyPromptAfterWakeRemoval => {
                "⚠️ 空の入力はGeminiへ送信できませんでした。"
            }
            InputDropReason::EmptyResponse => {
                "⚠️ Geminiから空の応答が返ったため、発話を生成できませんでした。"
            }
            InputDropReason::UnsupportedAction => "ℹ️ この発話はGemini送信条件を満たしていません。",
        };
        let kind = if matches!(
            reason,
            InputDropReason::StaleAtEntry
                | InputDropReason::StaleAfterMemorySearch
                | InputDropReason::StaleAfterWebSearch
                | InputDropReason::StaleAfterScreenCapture
                | InputDropReason::StaleBeforeGeneration
                | InputDropReason::StaleAfterGeneration
                | InputDropReason::StaleBeforeTts
                | InputDropReason::StaleAfterTts
                | InputDropReason::WakeWordRequired
                | InputDropReason::WakeOnly
                | InputDropReason::DuplicateSuppressed
                | InputDropReason::StopWord
                | InputDropReason::UnsupportedAction
        ) {
            "info"
        } else {
            "warning"
        };
        // A stale generation is deliberately invisible to the current UI:
        // showing a toast from an old callback can make a newly started
        // session appear to have dropped its own input. Keep the structured
        // log above for diagnostics, but only notify callers for the active
        // generation (or context-free/manual work).
        if should_emit_input_drop_toast(reason) && self.context_allows_ui(context) {
            emit_toast_notice(app_handle, message, kind);
        }
    }

    pub(crate) fn log_nod_not_requested(
        &self,
        context: &SessionContext,
        event_id: &str,
        phase: &str,
        stream: &str,
        decision: &WakeWordDecision,
        reason: &str,
    ) {
        self.log_mgr.info(
            "TTS",
            &format!(
                "session_id={} generation={} event_id={} status=nod_not_requested phase={} stream={} action={:?} should_acknowledge={} cooldown_active={} duplicate_suppressed={} reason={}",
                context.session_id,
                context.generation,
                event_id,
                phase,
                stream,
                decision.action,
                decision.should_acknowledge,
                decision.cooldown_active,
                decision.duplicate_suppressed,
                reason
            ),
        );
    }

    /// Schedule a nod without making Gemini wait for the audio duration. The
    /// task carries the originating session identity and checks it again just
    /// before touching the audio queue, so a stopped/restarted session cannot

    pub(crate) fn schedule_nod_ack(
        &self,
        context: &SessionContext,
        event_id: &str,
        phase: &str,
        action: WakeWordAction,
    ) {
        if !self.is_current_session(context) {
            self.log_mgr.info(
                "TTS",
                &format!(
                    "session_id={} generation={} event_id={} status=nod_not_requested phase={} action={:?} reason=stale_before_schedule",
                    context.session_id, context.generation, event_id, phase, action
                ),
            );
            return;
        }

        self.log_mgr.info(
            "TTS",
            &format!(
                "session_id={} generation={} event_id={} status=nod_scheduled phase={} action={:?} wait=background",
                context.session_id, context.generation, event_id, phase, action
            ),
        );
        let this = self.clone();
        let context = context.clone();
        let event_id = event_id.to_string();
        let phase = phase.to_string();
        tauri::async_runtime::spawn(async move {
            if !this.is_current_session(&context) {
                this.log_mgr.info(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=nod_not_requested phase={} action={:?} reason=stale_before_start",
                        context.session_id, context.generation, event_id, phase, action
                    ),
                );
                return;
            }

            let started = Instant::now();
            this.log_mgr.info(
                "TTS",
                &format!(
                    "session_id={} generation={} event_id={} status=nod_started phase={} action={:?}",
                    context.session_id, context.generation, event_id, phase, action
                ),
            );
            let result = this.tts_mgr.play_random_nod(&this.root_dir).await;
            let duration_ms = started.elapsed().as_millis();
            let current = this.is_current_session(&context);
            match result {
                Ok(()) => this.log_mgr.info(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=nod_succeeded phase={} action={:?} duration_ms={} current_session={}",
                        context.session_id,
                        context.generation,
                        event_id,
                        phase,
                        action,
                        duration_ms,
                        current
                    ),
                ),
                Err(error) => this.log_mgr.warn(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=nod_failed phase={} action={:?} reason={} duration_ms={} current_session={}",
                        context.session_id,
                        context.generation,
                        event_id,
                        phase,
                        action,
                        error.code(),
                        duration_ms,
                        current
                    ),
                ),
            }
        });
    }

    pub(crate) fn asr_clock_ms(&self, context: &SessionContext) -> u64 {
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

    pub(crate) fn emit_asr_result(
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

    pub(crate) fn handle_partial_asr_result(
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
        if detector_is_current {
            Self::emit_asr_result(
                app_handle,
                &display_text,
                stream,
                false,
                decision.is_prompt,
                latency_ms,
                None,
            );
        } else {
            self.log_mgr.info(
                "Session",
                &format!(
                    "session_id={} generation={} event_id=- status=stale_partial_ui_dropped",
                    context.session_id, context.generation
                ),
            );
        }
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

    pub(crate) async fn persist_final_asr_result(
        &self,
        context: &SessionContext,
        stream: &str,
        text: &str,
        latency_ms: Option<f64>,
        app_handle: Option<&AppHandle>,
    ) -> Option<PersistedAsrEvent> {
        let event_id = uuid::Uuid::new_v4().to_string();
        // Allocate the durable identity and append the redacted SessionEvent
        // before any wake-word/UI/AI work.  This keeps the in-memory session
        // history lossless even when a later detector or transport step
        // fails, and makes the callback's first observable action the same
        // event that will be written to the authoritative raw journal.
        let (event_type, author) = match stream {
            "mic" => ("user_speech", "User"),
            "discord" => ("discord_speech", "Discord"),
            _ => {
                self.log_mgr.warn(
                    "ASR",
                    &raw_admission_drop_message(
                        self,
                        &event_id,
                        Some(context),
                        "unknown_stream_dropped",
                        MemoryAdmissionDropReason::UnknownStream,
                    ),
                );
                return None;
            }
        };
        let content = match admit_redacted_memory_text_with_reason(text) {
            Ok(content) => content,
            Err(reason) => {
                self.log_mgr.warn(
                    "ASR",
                    &raw_admission_drop_message(
                        self,
                        &event_id,
                        Some(context),
                        "raw_admission_dropped",
                        reason,
                    ),
                );
                return None;
            }
        };
        let event = SessionEvent {
            id: event_id.clone(),
            r#type: event_type.to_string(),
            author: author.to_string(),
            content,
            timestamp: Local::now().to_rfc3339(),
        };
        self.append_event_to_session(event.clone(), Some(context));

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

        if !self
            .save_event_to_memory_with_app_context(
                &event,
                app_handle.cloned(),
                Some(context.clone()),
            )
            .await
        {
            // The persistence boundary already emitted a classified,
            // non-sensitive diagnostic (including the durable-vs-follow-up
            // distinction when applicable). Avoid a second generic message
            // that would obscure that reason.
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

    pub(crate) async fn process_asr_followup(
        &self,
        context: &SessionContext,
        result: PersistedAsrEvent,
        original_text: &str,
        app_handle: Option<&AppHandle>,
    ) -> InputProcessingResult {
        if !self.is_current_session(context) {
            self.log_mgr.info(
                "TTS",
                &format!(
                    "session_id={} generation={} event_id={} status=nod_not_requested phase=final action={:?} reason=stale_at_entry",
                    context.session_id, context.generation, result.event.id, result.decision.action
                ),
            );
            self.record_input_drop(
                Some(context),
                &result.event.id,
                InputDropReason::StaleAtEntry,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAtEntry,
            });
        }
        if result.stop_word_detected {
            self.log_nod_not_requested(
                context,
                &result.event.id,
                "final",
                &result.decision.stream,
                &result.decision,
                "stop_word",
            );
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
            self.record_input_drop(
                Some(context),
                &result.event.id,
                InputDropReason::StopWord,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StopWord,
            });
        }

        let prompt = match result.decision.action {
            WakeWordAction::PromptDetected | WakeWordAction::PromptReceived
                if result.decision.is_prompt =>
            {
                normalize_prompt_text(&result.decision.clean_prompt)
            }
            _ => normalize_prompt_text(original_text),
        };
        if result.decision.should_acknowledge {
            self.schedule_nod_ack(context, &result.event.id, "final", result.decision.action);
        } else {
            let reason = if result.decision.stream != "mic" {
                "mic_only"
            } else if result.decision.duplicate_suppressed {
                "duplicate_suppressed"
            } else if result.decision.cooldown_active {
                "cooldown_active"
            } else {
                match result.decision.action {
                    WakeWordAction::PromptDetected | WakeWordAction::FinalWakeOnly => {
                        "already_acknowledged"
                    }
                    WakeWordAction::PromptReceived => "prompt_acknowledgement_suppressed",
                    _ => "action_not_acknowledged",
                }
            };
            self.log_nod_not_requested(
                context,
                &result.event.id,
                "final",
                &result.decision.stream,
                &result.decision,
                reason,
            );
        }
        if !self.is_current_session(context) {
            self.record_input_drop(
                Some(context),
                &result.event.id,
                InputDropReason::StaleBeforeGeneration,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleBeforeGeneration,
            });
        }
        if result.decision.action == WakeWordAction::FinalWakeOnly {
            self.is_collecting_prompt.store(true, Ordering::SeqCst);
            self.record_input_drop(
                Some(context),
                &result.event.id,
                InputDropReason::WakeOnly,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::WakeOnly,
            });
        }

        let prompt = if matches!(
            result.decision.action,
            WakeWordAction::PromptDetected | WakeWordAction::PromptReceived
        ) && result.decision.is_prompt
        {
            prompt
        } else {
            let reason = match result.decision.action {
                WakeWordAction::DuplicateSuppressed => InputDropReason::DuplicateSuppressed,
                WakeWordAction::FinalSpeech | WakeWordAction::Ignored => {
                    InputDropReason::WakeWordRequired
                }
                _ => InputDropReason::UnsupportedAction,
            };
            self.record_input_drop(Some(context), &result.event.id, reason, app_handle);
            return Ok(InputProcessingOutcome::Dropped { reason });
        };

        if !asr_prompt_is_sendable(&prompt) {
            self.is_collecting_prompt.store(false, Ordering::SeqCst);
            self.record_input_drop(
                Some(context),
                &result.event.id,
                InputDropReason::EmptyPromptAfterWakeRemoval,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::EmptyPromptAfterWakeRemoval,
            });
        }

        let st_file = crate::settings::load_settings_file(&self.root_dir);
        let gemini_key = self.get_effective_gemini_key();
        let brave_key =
            crate::credentials::get_secret(&self.root_dir, "brave_api_key").unwrap_or_default();
        let model = st_file
            .get("gemini_model")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "latest".to_string())
            });
        // {user_name} がカスタムプロンプトに含まれる場合は設定値を注入する
        // (デフォルトのプロンプトにプレースホルダーがなければ何も変わらない)。
        let sys_prompt = crate::prompts::apply_prompt_placeholders(
            &crate::prompts::get_prompt(&self.root_dir, "system_instruction_character"),
            st_file
                .get("user_name")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
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
        let outcome = self
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
            .await;
        match &outcome {
            Ok(InputProcessingOutcome::Generated { .. }) => self.log_mgr.info(
                "AI",
                &format!(
                    "session_id={} generation={} event_id={} status=input_processing_completed",
                    context.session_id, context.generation, result.event.id
                ),
            ),
            Ok(InputProcessingOutcome::Dropped { reason }) => self.log_mgr.info(
                "AI",
                &format!(
                    "session_id={} generation={} event_id={} status=input_processing_dropped reason={}",
                    context.session_id,
                    context.generation,
                    result.event.id,
                    reason.code()
                ),
            ),
            Err(error) => {
                self.log_mgr.error(
                    "AI",
                    &format!(
                        "session_id={} generation={} event_id={} status=input_processing_failed reason={}",
                        context.session_id,
                        context.generation,
                        result.event.id,
                        ai_error_reason(error)
                    ),
                );
                if self.context_allows_ui(Some(context)) {
                    emit_toast_notice(
                        app_handle,
                        "⚠️ Geminiへの入力処理に失敗しました。設定とネットワークを確認してください。",
                        "error",
                    );
                }
            }
        }
        outcome
    }
}
