use chrono::Local;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use crate::ai_client::{AiGenerateOptions, ChatMessage};
use crate::tts::TtsSettings;
use crate::window_capture;

use super::lifecycle::session_context_fields;
use super::persistence::{memory_event_log_message, raw_durability_gap_message};
use super::types::*;
use super::SessionManager;

pub(crate) fn emit_toast_notice(app_handle: Option<&AppHandle>, message: &str, kind: &str) {
    if let Some(handle) = app_handle {
        let _ = handle.emit(
            "toast_notice",
            serde_json::json!({
                "message": message,
                "type": kind,
            }),
        );
    }
}

pub(crate) fn build_chat_messages(events: &[SessionEvent]) -> Vec<ChatMessage> {
    let mut selected = Vec::new();
    let mut used_chars = 0usize;

    for event in events.iter().rev().take(MAX_CHAT_HISTORY_MESSAGES) {
        let role = if event.r#type == "ai_response" || event.r#type == "auto_commentary" {
            "assistant"
        } else {
            "user"
        };
        let content = format!("{}: {}", event.author, event.content);
        let content_chars = content.chars().count();
        if used_chars.saturating_add(content_chars) > MAX_CHAT_HISTORY_CHARS && !selected.is_empty()
        {
            break;
        }

        let bounded_content = if used_chars.saturating_add(content_chars) > MAX_CHAT_HISTORY_CHARS {
            content
                .chars()
                .take(MAX_CHAT_HISTORY_CHARS.saturating_sub(used_chars))
                .collect::<String>()
        } else {
            content
        };
        used_chars = used_chars.saturating_add(bounded_content.chars().count());
        selected.push(ChatMessage {
            role: role.to_string(),
            content: bounded_content,
        });
    }
    selected.reverse();
    selected
}

pub(crate) fn ai_error_reason(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("api key") || lower.contains("key is not set") {
        "api_key_configuration"
    } else if lower.contains("http request")
        || lower.contains("api error")
        || lower.contains("timeout")
    {
        "http_or_api_failure"
    } else if lower.contains("parse") || lower.contains("response") {
        "invalid_api_response"
    } else {
        "generation_failure"
    }
}

impl SessionManager {
    pub fn get_effective_gemini_key(&self) -> String {
        crate::credentials::get_secret(&self.root_dir, "gemini_api_key").unwrap_or_default()
    }

    /// 設定/環境から解決した有効キーで利用可能な Gemini モデル一覧を返す
    /// (trigger=selected_memories_blog や設定UIのモデル選択用)。
    pub async fn list_gemini_models(&self) -> Result<Vec<String>, String> {
        let key = self.get_effective_gemini_key();
        if key.trim().is_empty() {
            return Err(
                "Gemini API キーが未設定です。設定画面でAPIキーを保存してください。".to_string(),
            );
        }
        self.ai_client.list_models(&key).await
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
        let prompt_text = normalize_prompt_text(clean_text);
        if !prompt_is_sendable(&prompt_text) {
            self.log_mgr.warn(
                "AI",
                "event_id=- session_id=- generation=- status=input_dropped reason=empty_prompt",
            );
            emit_toast_notice(
                app_handle,
                "⚠️ 空の入力はGeminiへ送信できませんでした。",
                "warning",
            );
            return Err("input dropped: empty_prompt".to_string());
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
        if !self
            .save_event_to_memory_with_app_context(&user_event, app_handle.cloned(), None)
            .await
        {
            self.log_mgr.warn(
                "Memory",
                &format!(
                    "event_id={} status=raw_not_durable_before_generation reason=persistence_failed",
                    user_event.id
                ),
            );
        }

        match self
            .process_user_input_for_event(
                None,
                &user_event,
                &prompt_text,
                gemini_api_key,
                brave_api_key,
                gemini_model,
                system_prompt,
                tts_settings,
                app_handle,
            )
            .await?
        {
            InputProcessingOutcome::Generated { response, .. } => Ok(response),
            InputProcessingOutcome::Dropped { reason } => {
                Err(format!("input dropped: {}", reason.code()))
            }
        }
    }

    /// Continue processing an ASR event that was already persisted by the
    /// finalized callback.  The prompt text may be the wake-word-cleaned
    /// candidate, while `event` remains the one authoritative raw event.
    pub(crate) async fn process_user_input_for_session(
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
    ) -> InputProcessingResult {
        if !self.is_current_session(context) {
            self.record_input_drop(
                Some(context),
                &event.id,
                InputDropReason::StaleAtEntry,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAtEntry,
            });
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
    pub(crate) async fn process_user_input_for_event(
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
    ) -> InputProcessingResult {
        let clean_text = normalize_prompt_text(clean_text);
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::StaleAtEntry,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAtEntry,
            });
        }
        if !prompt_is_sendable(&clean_text) {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::EmptyPrompt,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::EmptyPrompt,
            });
        }

        if let Err(reason) = crate::ai_client::validate_gemini_api_key(gemini_api_key) {
            let reason_code = reason.code();
            let (session_id, generation) = session_context_fields(self, context);
            self.log_mgr.warn(
                "Gemini",
                &format!(
                    "session_id={} generation={} event_id={} status=generation_blocked stage=preflight reason={}",
                    session_id, generation, user_event.id, reason_code
                ),
            );
            if self.context_allows_ui(context) {
                emit_toast_notice(
                    app_handle,
                    match reason {
                        crate::ai_client::GeminiKeyError::Missing => {
                            "⚠️ Gemini APIキーが未設定です。設定画面でAPIキーを保存してください。"
                        }
                        crate::ai_client::GeminiKeyError::Invalid => {
                            "⚠️ Gemini APIキーの設定が不正です。空白や引用符を確認してください。"
                        }
                    },
                    "warning",
                );
            }
            return Err(match reason {
                crate::ai_client::GeminiKeyError::Missing => {
                    "Gemini API key is not set".to_string()
                }
                crate::ai_client::GeminiKeyError::Invalid => {
                    "Gemini API key configuration is invalid".to_string()
                }
            });
        }

        // LanceDB 関連記憶をセマンティック検索 (GLuCoSE-base-ja 埋め込みモデル)
        let memory_context = self.get_relevant_memory_context(&clean_text).await;
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::StaleAfterMemorySearch,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAfterMemorySearch,
            });
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
                    &clean_text,
                    "web_search_started",
                ),
            );
            match tokio::time::timeout(
                INPUT_WEB_SEARCH_TIMEOUT,
                self.search_client
                    .search_and_format(&clean_text, brave_api_key),
            )
            .await
            {
                Ok(res) => format!("\n\n{}", res.summary_text),
                Err(_) => {
                    self.log_mgr.warn(
                        "WebSearch",
                        &format!(
                            "event_id={} status=web_search_failed reason=timeout",
                            user_event.id
                        ),
                    );
                    String::new()
                }
            }
        } else {
            String::new()
        };
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::StaleAfterWebSearch,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAfterWebSearch,
            });
        }

        let st = crate::settings::load_settings_file(&self.root_dir);
        let use_image = st
            .get("use_image")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // ゲーム画面のキャプチャ（選択中ウィンドウを優先、なければプライマリスクリーン）
        let screen_b64 = if use_image {
            let win_name = st
                .get("window")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let log_mgr = self.log_mgr.clone();
            let capture = tokio::task::spawn_blocking(move || {
                if !win_name.is_empty() {
                    log_mgr.info(
                        "Visual",
                        &format!("Capturing target window: '{}'", win_name),
                    );
                    window_capture::capture_window_base64(&win_name).or_else(|| {
                        log_mgr.warn("Visual", "Window capture fallback to primary screen");
                        window_capture::capture_primary_screen_base64()
                    })
                } else {
                    log_mgr.info("Visual", "Capturing primary screen...");
                    window_capture::capture_primary_screen_base64()
                }
            });
            match tokio::time::timeout(INPUT_SCREEN_CAPTURE_TIMEOUT, capture).await {
                Ok(Ok(image)) => image,
                Ok(Err(_)) => {
                    self.log_mgr.warn(
                        "Visual",
                        &format!(
                            "event_id={} status=screen_capture_failed reason=worker_failed",
                            user_event.id
                        ),
                    );
                    None
                }
                Err(_) => {
                    self.log_mgr.warn(
                        "Visual",
                        &format!(
                            "event_id={} status=screen_capture_failed reason=timeout",
                            user_event.id
                        ),
                    );
                    None
                }
            }
        } else {
            None
        };
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::StaleAfterScreenCapture,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAfterScreenCapture,
            });
        }

        // プロンプト構築
        let full_system_instruction =
            format!("{}{}{}", system_prompt, memory_context, search_context);

        // 会話履歴。長時間セッションでも送信前の処理量を有界にし、
        // 直近の発話（通常は user_event）を必ず残す。
        let events = self.get_events();
        let chat_messages = build_chat_messages(&events);

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

        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::StaleBeforeGeneration,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleBeforeGeneration,
            });
        }

        self.log_mgr.info(
            "Gemini",
            &format!(
                "session_id={} generation={} {} model={}",
                context
                    .map(|session| session.session_id.as_str())
                    .unwrap_or("-"),
                context
                    .map(|session| session.generation)
                    .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                memory_event_log_message(
                    &user_event.id,
                    &user_event.r#type,
                    &user_event.author,
                    &clean_text,
                    "generation_started"
                ),
                gemini_model
            ),
        );

        if self.context_allows_ui(context) {
            if let Some(handle) = app_handle {
                let _ = handle.emit(
                    "gemini_status",
                    serde_json::json!({
                        "is_generating": true,
                        "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                        "generation": context.map(|session| session.generation).unwrap_or(0),
                        "status": "generation_started"
                    }),
                );
            }
        }

        // Gemini AI 推論
        let ai_res = match self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &chat_messages, &options)
            .await
        {
            Ok(res) => {
                if self.context_allows_ui(context) {
                    if let Some(handle) = app_handle {
                        let _ = handle.emit(
                            "gemini_status",
                            serde_json::json!({
                                "is_generating": false,
                                "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                                "generation": context.map(|session| session.generation).unwrap_or(0),
                                "status": "response_received"
                            }),
                        );
                    }
                }
                res
            }
            Err(e) => {
                if self.context_allows_ui(context) {
                    if let Some(handle) = app_handle {
                        let _ = handle.emit(
                            "gemini_status",
                            serde_json::json!({
                                "is_generating": false,
                                "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                                "generation": context.map(|session| session.generation).unwrap_or(0),
                                "status": "generation_failed",
                                "reason": ai_error_reason(&e)
                            }),
                        );
                    }
                }
                let (session_id, generation) = session_context_fields(self, context);
                self.log_mgr.error(
                    "Gemini",
                    &format!(
                        "session_id={} generation={} event_id={} status=generation_failed stage=post_send reason={} detail={}",
                        session_id,
                        generation,
                        user_event.id,
                        ai_error_reason(&e),
                        ai_error_reason(&e)
                    ),
                );
                if self.context_allows_ui(context) {
                    emit_toast_notice(
                        app_handle,
                        "⚠️ Geminiへの送信に失敗しました。設定とネットワークを確認してください。",
                        "error",
                    );
                }
                return Err(e);
            }
        };

        let clean_ai_res = ai_res.trim().to_string();

        if !prompt_is_sendable(&clean_ai_res) {
            if self.context_allows_ui(context) {
                if let Some(handle) = app_handle {
                    let _ = handle.emit(
                        "gemini_status",
                        serde_json::json!({
                            "is_generating": false,
                            "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                            "generation": context.map(|session| session.generation).unwrap_or(0),
                            "status": "empty_response"
                        }),
                    );
                }
            }
            self.record_input_drop(
                context,
                &user_event.id,
                InputDropReason::EmptyResponse,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::EmptyResponse,
            });
        }

        let ai_event = SessionEvent {
            id: uuid::Uuid::new_v4().to_string(),
            r#type: "ai_response".to_string(),
            author: "Assistant".to_string(),
            content: clean_ai_res.clone(),
            timestamp: Local::now().to_rfc3339(),
        };
        self.log_mgr.info(
            "AI",
            &format!(
                "session_id={} generation={} {} status=generation_succeeded",
                context
                    .map(|session| session.session_id.as_str())
                    .unwrap_or("-"),
                context
                    .map(|session| session.generation)
                    .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                memory_event_log_message(
                    &ai_event.id,
                    &ai_event.r#type,
                    &ai_event.author,
                    &ai_event.content,
                    "generated"
                )
            ),
        );
        self.append_event_to_session(ai_event.clone(), context);
        if self.context_allows_ui(context) {
            if let Some(handle) = app_handle {
                let _ = handle.emit("session-event", &ai_event);
            }
        }

        let raw_persisted = self
            .save_event_to_memory_with_app_context(&ai_event, app_handle.cloned(), context.cloned())
            .await;
        if !raw_persisted {
            self.log_mgr.warn(
                "AI",
                &raw_durability_gap_message(
                    self,
                    &ai_event.id,
                    context,
                    "displayed_without_durable_raw",
                ),
            );
        }

        // A response generated by an older session is still retained in the
        // authoritative raw journal, but it must not reach TTS or the live UI.
        if !self.context_allows_ui(context) {
            self.record_input_drop(
                context,
                &ai_event.id,
                InputDropReason::StaleBeforeTts,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleBeforeTts,
            });
        }

        // 音声合成 & 発話再生
        self.log_mgr.info(
            "TTS",
            &format!(
                "session_id={} generation={} event_id={} status=tts_started",
                context
                    .map(|session| session.session_id.as_str())
                    .unwrap_or("-"),
                context
                    .map(|session| session.generation)
                    .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                ai_event.id
            ),
        );
        *self.last_speak_time.lock() = Instant::now();
        if self.context_allows_ui(context) {
            if let Some(handle) = app_handle {
                let _ = handle.emit(
                    "tts_status",
                    serde_json::json!({
                        "is_playing": true,
                        "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                        "generation": context.map(|session| session.generation).unwrap_or(0),
                        "status": "tts_started"
                    }),
                );
            }
        }
        let tts_result = self.tts_mgr.speak(&clean_ai_res, tts_settings).await;
        if self.context_allows_ui(context) {
            if let Some(handle) = app_handle {
                let _ = handle.emit(
                    "tts_status",
                    serde_json::json!({
                        "is_playing": false,
                        "session_id": context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                        "generation": context.map(|session| session.generation).unwrap_or(0),
                        "status": if tts_result.is_ok() { "tts_succeeded" } else { "tts_failed" }
                    }),
                );
            }
        }
        let tts_outcome = match tts_result {
            Ok(()) => {
                self.log_mgr.info(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=tts_succeeded",
                        context
                            .map(|session| session.session_id.as_str())
                            .unwrap_or("-"),
                        context
                            .map(|session| session.generation)
                            .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                        ai_event.id
                    ),
                );
                TtsOutcome::Succeeded
            }
            Err(error) => {
                self.log_mgr.warn(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=tts_failed reason={}",
                        context
                            .map(|session| session.session_id.as_str())
                            .unwrap_or("-"),
                        context
                            .map(|session| session.generation)
                            .unwrap_or_else(|| self.session_generation.load(Ordering::SeqCst)),
                        ai_event.id,
                        ai_error_reason(&error)
                    ),
                );
                if self.context_allows_ui(context) {
                    emit_toast_notice(
                        app_handle,
                        "⚠️ Geminiの応答は取得しましたが、音声再生に失敗しました。",
                        "warning",
                    );
                }
                TtsOutcome::Failed
            }
        };
        if self.context_allows_ui(context) {
            *self.last_speak_time.lock() = Instant::now();
        }

        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                &ai_event.id,
                InputDropReason::StaleAfterTts,
                app_handle,
            );
            return Ok(InputProcessingOutcome::Dropped {
                reason: InputDropReason::StaleAfterTts,
            });
        }

        Ok(InputProcessingOutcome::Generated {
            response: clean_ai_res,
            tts: tts_outcome,
        })
    }
}
