use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use chrono::{Local, Utc};
use tauri::{AppHandle, Emitter};

use crate::asr::WakeWordAction;

use super::blog::unique_blog_path;
use super::commentary::auto_commentary_enabled;
use super::input_pipeline::{
    admit_twitch_comment, effective_wake_words, wake_word_config_from_settings,
};
use super::types::*;
use super::SessionManager;

pub(crate) fn automatic_blog_post_enabled(settings: &serde_json::Value) -> bool {
    settings
        .get("create_blog_post")
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

pub(crate) fn resolve_effective_twitch_channel(settings: &serde_json::Value) -> String {
    let explicit = settings
        .get("twitch_channel")
        .or_else(|| settings.get("twitch_bot_channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if !explicit.is_empty() {
        return explicit.trim_start_matches('#').to_string();
    }

    if let Some(u) = settings.get("user_name").and_then(|v| v.as_str()) {
        let trimmed = u.trim().trim_start_matches('#');
        if !trimmed.is_empty()
            && trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return trimmed.to_string();
        }
    }

    String::new()
}

pub(crate) fn session_context_fields(
    manager: &SessionManager,
    context: Option<&SessionContext>,
) -> (String, u64) {
    context
        .map(|context| (context.session_id.clone(), context.generation))
        .unwrap_or_else(|| {
            let session_id = manager.session_id.lock().clone();
            (
                if session_id.is_empty() {
                    "-".to_string()
                } else {
                    session_id
                },
                manager.session_generation.load(Ordering::SeqCst),
            )
        })
}


impl SessionManager {
    pub(crate) fn current_session_context(&self) -> Option<SessionContext> {
        if !self.is_active.load(Ordering::SeqCst) {
            return None;
        }
        let session_id = self.session_id.lock().clone();
        let started_at = (*self.session_started_at.lock())?;
        if session_id.is_empty() {
            return None;
        }
        Some(SessionContext {
            session_id,
            generation: self.session_generation.load(Ordering::SeqCst),
            started_at,
        })
    }

    pub(crate) fn is_current_session(&self, context: &SessionContext) -> bool {
        self.is_active.load(Ordering::SeqCst)
            && self.session_generation.load(Ordering::SeqCst) == context.generation
            && self.session_id.lock().as_str() == context.session_id
    }

    pub(crate) fn context_allows_ui(&self, context: Option<&SessionContext>) -> bool {
        context
            .map(|value| self.is_current_session(value))
            .unwrap_or(true)
    }

    pub(crate) fn allows_fact_ui_emit(&self, context: &SessionContext) -> bool {
        let _lifecycle_guard = self.session_lifecycle.lock();
        if !self.is_active.load(Ordering::SeqCst) {
            return true;
        }
        self.session_generation.load(Ordering::SeqCst) == context.generation
            && self.session_id.lock().as_str() == context.session_id
    }

    pub(crate) fn begin_session(&self) -> Option<SessionContext> {
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

    pub(crate) fn append_event_to_session(&self, event: SessionEvent, context: Option<&SessionContext>) {
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

    pub(crate) fn stop_session_internal(&self) -> Option<SessionContext> {
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

    pub async fn ensure_asr_ready(&self) -> Result<(), String> {
        self.asr_engine.ws_client.warmup().await
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

    pub(crate) fn start_session_with_services_for_request(
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
        if let Some(ref handle) = app_handle {
            self.asr_engine.ws_client.set_app_handle(handle.clone());
        }
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
            let twitch_channel = resolve_effective_twitch_channel(&settings);

            let twitch_bot_username = settings
                .get("twitch_bot_username")
                .and_then(|v| v.as_str())
                .unwrap_or("justinfan12345")
                .trim()
                .to_string();

            let twitch_bot_token =
                crate::credentials::get_secret(&self.root_dir, "twitch_access_token")
                    .or_else(|| crate::credentials::get_secret(&self.root_dir, "twitch_bot_token"))
                    .unwrap_or_default();

            if !twitch_channel.is_empty() {
                let log_mgr_twitch = self.log_mgr.clone();
                let app_h = app_handle.clone();
                let session_self = self.clone();
                let twitch_context = session_context.clone();

                let auth_desc = if twitch_bot_token.is_empty() {
                    "anonymous reader (justinfan)".to_string()
                } else {
                    format!("authenticated nick='{}'", twitch_bot_username)
                };
                log_mgr_twitch.info(
                    "Twitch",
                    &format!(
                        "Attempting Twitch IRC connection as {} to channel '#{}'...",
                        auth_desc, twitch_channel
                    ),
                );

                let twitch_client_id = settings
                    .get("twitch_client_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let twitch_client_secret =
                    crate::credentials::get_secret(&self.root_dir, "twitch_client_secret");
                let twitch_refresh_token =
                    crate::credentials::get_secret(&self.root_dir, "twitch_refresh_token");

                tauri::async_runtime::spawn(async move {
                    let bot_settings = crate::twitch::TwitchBotSettings {
                        channel: twitch_channel.clone(),
                        bot_nick: twitch_bot_username.clone(),
                        oauth_token: twitch_bot_token,
                        client_id: twitch_client_id,
                        client_secret: twitch_client_secret,
                        refresh_token: twitch_refresh_token,
                    };

                    let session_for_msg = session_self.clone();
                    let app_for_msg = app_h.clone();
                    let context_for_msg = twitch_context.clone();
                    let on_msg = Arc::new(move |msg: crate::twitch::TwitchChatMessage| {
                        let sess = session_for_msg.clone();
                        let app_m = app_for_msg.clone();
                        let context = context_for_msg.clone();
                        sess.log_mgr.info(
                            "Twitch",
                            &format!(
                                "Received Twitch chat from '{}' in session={}: {}",
                                msg.author, context.session_id, msg.content
                            ),
                        );
                        if !sess.is_current_session(&context) {
                            sess.log_mgr.info(
                                "Session",
                                &format!(
                                    "session_id={} generation={} status=stale_twitch_callback_accepted_for_raw",
                                    context.session_id, context.generation
                                ),
                            );
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
                            if sess.is_current_session(&context) {
                                if let Some(ref handle) = app_m {
                                    let _ = handle.emit("session-event", &tw_event);
                                }
                            }
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
                            // 会話応答の受付判定 (trigger=twitch_wake_word): 最新設定の
                            // 有効ウェイクワードで「コメント本文だけ」を照合する (表示名・
                            // チャンネル名は対象外)。一致しないコメントは保存・表示のみで
                            // Gemini/TTS には渡さない (正常な抑止なのでトーストも出さない)。
                            let wake_words = effective_wake_words(&st);
                            let comment_prompt = match admit_twitch_comment(
                                &wake_words,
                                &msg.content,
                            ) {
                                Some(prompt) => prompt,
                                None => {
                                    sess.log_mgr.info(
                                        "Session",
                                        &format!(
                                            "session_id={} generation={} trigger=twitch_wake_word source_event_id={} status=skipped reason=wake_word_missing",
                                            context.session_id, context.generation, tw_event.id
                                        ),
                                    );
                                    return;
                                }
                            };
                            let gemini_key = sess.get_effective_gemini_key();
                            let brave_key =
                                crate::credentials::get_secret(&sess.root_dir, "brave_api_key")
                                    .unwrap_or_default();
                            let model = st
                                .get("gemini_model")
                                .and_then(|v| v.as_str())
                                .unwrap_or("latest")
                                .to_string();
                            let sys_prompt = crate::prompts::apply_prompt_placeholders(
                                &crate::prompts::get_prompt(
                                    &sess.root_dir,
                                    "system_instruction_character",
                                ),
                                st.get("user_name").and_then(|v| v.as_str()).unwrap_or(""),
                            );
                            let tts_cfg = extract_tts_settings(&st);

                            let _ = sess
                                .process_user_input_for_session(
                                    &context,
                                    &tw_event,
                                    &comment_prompt,
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
            } else {
                self.log_mgr.warn(
                    "Twitch",
                    "Twitch channel not configured (or user_name is not ASCII alphanumeric); skipping Twitch IRC connection. Please set 'twitch_channel' in Settings.",
                );
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
                if !session_cl.admit_asr_callback(&context, is_final) {
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
                        log_cl.info(
                            "TTS",
                            &format!(
                                "session_id={} generation={} event_id=- status=nod_not_requested phase=partial reason=stale_task",
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
                        if decision.should_acknowledge {
                            session_cl.schedule_nod_ack(
                                &context,
                                "-",
                                "partial",
                                decision.action,
                            );
                        } else {
                            let reason = if stream != "mic" {
                                "mic_only"
                            } else if decision.duplicate_suppressed {
                                "duplicate_suppressed"
                            } else if decision.cooldown_active {
                                "cooldown_active"
                            } else {
                                match decision.action {
                                    WakeWordAction::PartialWakeDetected => {
                                        "already_acknowledged"
                                    }
                                    WakeWordAction::PartialPromptCandidate => {
                                        "prompt_candidate_not_final"
                                    }
                                    _ => "action_not_acknowledged",
                                }
                            };
                            session_cl.log_nod_not_requested(
                                &context,
                                "-",
                                "partial",
                                &stream,
                                &decision,
                                reason,
                            );
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
                        let _ = session_cl
                            .process_asr_followup(&context, result, &text, app_cl.as_ref())
                            .await;
                    }
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
            Some(Arc::new(move |samples: Vec<f32>| {
                ws_for_mic.send_audio("mic", &samples);
            })),
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
                app_handle.clone(),
                Some(Arc::new(move |samples: Vec<f32>| {
                    ws_for_discord.send_audio("discord", &samples);
                })),
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
                if !auto_commentary_enabled(&st) {
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

                // 生成開始直前にも有効設定を再確認する (待機中にOFFへ変更された
                // 場合は生成せず、理由を記録する)。
                if !auto_commentary_enabled(&crate::settings::load_settings_file(
                    &session_for_comm.root_dir,
                )) {
                    log_mgr_comm.info(
                        "Commentary",
                        "status=skipped reason=auto_commentary_disabled (disabled during wait)",
                    );
                    continue;
                }

                let gemini_key = session_for_comm.get_effective_gemini_key();
                let model = st
                    .get("gemini_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("latest")
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

    pub fn start_audio_preview(
        &self,
        mic_device: Option<String>,
        discord_device: Option<String>,
        enable_discord: bool,
        app_handle: AppHandle,
    ) {
        if !self.is_active.load(Ordering::SeqCst) {
            let _ =
                self.audio_input_mgr
                    .start_mic_stream(mic_device, Some(app_handle.clone()), None);

            if enable_discord {
                let _ = self.audio_input_mgr.start_discord_stream(
                    discord_device,
                    Some(app_handle),
                    None,
                );
            } else {
                self.audio_input_mgr.stop_discord();
            }
        }
    }

    pub fn stop_audio_preview(&self) {
        if !self.is_active.load(Ordering::SeqCst) {
            self.audio_input_mgr.stop();
        }
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
                    .unwrap_or("latest")
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
}
