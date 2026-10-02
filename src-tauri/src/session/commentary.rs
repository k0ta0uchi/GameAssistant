use chrono::Local;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use crate::ai_client::{AiGenerateOptions, ChatMessage};
use crate::tts::TtsSettings;
use crate::window_capture;

use super::ai_pipeline::{ai_error_reason, emit_toast_notice};
use super::persistence::{memory_event_log_message, raw_durability_gap_message};
use super::types::*;
use super::SessionManager;

/// 自動実況の有効設定。UI と同じく、キー未設定は OFF、明示 true のみ ON
/// (旧実装は unwrap_or(true) で未設定が ON 扱いだった)。
pub(crate) fn auto_commentary_enabled(settings: &serde_json::Value) -> bool {
    settings
        .get("enable_auto_commentary")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

impl SessionManager {
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

    pub(crate) async fn execute_auto_commentary_for_session(
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

    pub(crate) async fn execute_auto_commentary_inner(
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
            self.record_input_drop(context, "-", InputDropReason::StaleAtEntry, app_handle);
            return Err("commentary dropped: stale_at_entry".to_string());
        }

        self.log_mgr.info(
            "Commentary",
            "Generating autonomous live commentary on current gameplay...",
        );

        let st = crate::settings::load_settings_file(&self.root_dir);
        // 配信者名は設定 (user_name) で管理する。テンプレート内の
        // {user_name} をここで動的に注入し、ハードコードされた名前に依存しない。
        let sys_prompt = crate::prompts::apply_prompt_placeholders(
            &crate::prompts::get_prompt(&self.root_dir, "auto_commentary_prompt"),
            st.get("user_name").and_then(|v| v.as_str()).unwrap_or(""),
        );

        if let Err(reason) = crate::ai_client::validate_gemini_api_key(gemini_api_key) {
            self.log_mgr.warn(
                "Gemini",
                &format!(
                    "session_id={} generation={} event_id=- status=generation_blocked stage=preflight reason={}",
                    context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                    context.map(|session| session.generation).unwrap_or(0),
                    reason.code()
                ),
            );
            if self.context_allows_ui(context) {
                emit_toast_notice(
                    app_handle,
                    match reason {
                        crate::ai_client::GeminiKeyError::Missing => {
                            "⚠️ Gemini APIキーが未設定のため、実況コメントを生成できません。"
                        }
                        crate::ai_client::GeminiKeyError::Invalid => {
                            "⚠️ Gemini APIキーの設定が不正なため、実況コメントを生成できません。"
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

        // LanceDB 関連記憶をセマンティック検索 (直近の会話またはゲーム状況)
        let memory_context = self
            .get_relevant_memory_context("ゲームプレイ状況 実況 解説")
            .await;
        if context
            .map(|session| !self.is_current_session(session))
            .unwrap_or(false)
        {
            self.record_input_drop(
                context,
                "-",
                InputDropReason::StaleAfterMemorySearch,
                app_handle,
            );
            return Err("commentary dropped: stale_after_memory_search".to_string());
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
                        &format!("Capturing target window for commentary: '{}'", win_name),
                    );
                    window_capture::capture_window_base64(&win_name)
                        .or_else(window_capture::capture_primary_screen_base64)
                } else {
                    log_mgr.info("Visual", "Capturing primary screen for commentary...");
                    window_capture::capture_primary_screen_base64()
                }
            });
            match tokio::time::timeout(INPUT_SCREEN_CAPTURE_TIMEOUT, capture).await {
                Ok(Ok(image)) => image,
                Ok(Err(_)) => {
                    self.log_mgr.warn(
                        "Visual",
                        "commentary screen capture worker failed; continuing without image",
                    );
                    None
                }
                Err(_) => {
                    self.log_mgr.warn(
                        "Visual",
                        "commentary screen capture timed out; continuing without image",
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
                "-",
                InputDropReason::StaleAfterScreenCapture,
                app_handle,
            );
            return Err("commentary dropped: stale_after_screen_capture".to_string());
        }

        // Auto Commentary が取得した最新フレームを Target Window カードの
        // プレビューへ即時反映する。以前は UI 側が選択時に撮った静止画のまま
        // 古くなっていたため、実況が実際に参照した画面をフロントへ通知する。
        if let Some(ref image) = screen_b64 {
            if self.context_allows_ui(context) {
                if let Some(handle) = app_handle {
                    let _ = handle.emit(
                        "window_preview_updated",
                        serde_json::json!({ "image": image, "source": "auto_commentary" }),
                    );
                }
            }
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
                self.log_mgr.error(
                    "Commentary",
                    &format!(
                        "session_id={} generation=- status=generation_failed reason={}",
                        context
                            .map(|session| session.session_id.as_str())
                            .unwrap_or("-"),
                        ai_error_reason(&e)
                    ),
                );
                if self.context_allows_ui(context) {
                    emit_toast_notice(
                        app_handle,
                        "⚠️ Gemini実況コメントの生成に失敗しました。設定とネットワークを確認してください。",
                        "error",
                    );
                }
                return Err(e);
            }
        };

        let clean_ai_res = ai_res.trim().to_string();

        if !prompt_is_sendable(&clean_ai_res) {
            self.record_input_drop(context, "-", InputDropReason::EmptyResponse, app_handle);
            return Err("commentary dropped: empty_response".to_string());
        }

        // Gemini要求は取り消せない: 待機中に実況がOFFへ変更された、または
        // セッションが切り替わった場合は、返却済みの結果の表示・TTSを抑止し、
        // キャンセル相当として記録する (正規の auto_commentary 識別は維持)。
        let enable_now =
            auto_commentary_enabled(&crate::settings::load_settings_file(&self.root_dir));
        let session_live = context
            .map(|session| self.is_current_session(session))
            .unwrap_or(true);
        if !enable_now || !session_live {
            self.log_mgr.info(
                "Commentary",
                &format!(
                    "session_id={} status=cancelled reason={} (result discarded before display/tts)",
                    context.map(|session| session.session_id.as_str()).unwrap_or("-"),
                    if !enable_now {
                        "auto_commentary_disabled"
                    } else {
                        "stale_session"
                    }
                ),
            );
            return Err("commentary cancelled: result suppressed".to_string());
        }

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
        let raw_persisted = self
            .save_event_to_memory_with_app_context(&ai_event, app_handle.cloned(), context.cloned())
            .await;
        if !raw_persisted {
            self.log_mgr.warn(
                "Commentary",
                &raw_durability_gap_message(self, &ai_event.id, context, "generated_not_durable"),
            );
        }

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
            self.record_input_drop(
                context,
                &ai_event.id,
                InputDropReason::StaleBeforeTts,
                app_handle,
            );
            return Err("commentary dropped: stale_before_tts".to_string());
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
        match self.tts_mgr.speak(&clean_ai_res, tts_settings).await {
            Ok(()) => self.log_mgr.info(
                "TTS",
                &format!(
                    "session_id={} generation={} event_id={} status=tts_succeeded",
                    context
                        .map(|session| session.session_id.as_str())
                        .unwrap_or("-"),
                    context.map(|session| session.generation).unwrap_or(0),
                    ai_event.id
                ),
            ),
            Err(error) => {
                self.log_mgr.warn(
                    "TTS",
                    &format!(
                        "session_id={} generation={} event_id={} status=tts_failed reason={}",
                        context
                            .map(|session| session.session_id.as_str())
                            .unwrap_or("-"),
                        context.map(|session| session.generation).unwrap_or(0),
                        ai_event.id,
                        ai_error_reason(&error)
                    ),
                );
                if self.context_allows_ui(context) {
                    emit_toast_notice(
                        app_handle,
                        "⚠️ 実況コメントは取得しましたが、音声再生に失敗しました。",
                        "warning",
                    );
                }
            }
        }
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

        Ok(clean_ai_res)
    }
}
