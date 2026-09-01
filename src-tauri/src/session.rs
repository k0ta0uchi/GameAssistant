use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use chrono::Local;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::ai_client::{AiClient, AiGenerateOptions, ChatMessage};
use crate::asr::AsrEngine;
use crate::audio_input::AudioInputManager;
use crate::lance_memory::{self, MemoryItem};
use crate::logger::LogManager;
use crate::tts::{TtsManager, TtsSettings};
use crate::web_search::WebSearchClient;
use crate::window_capture;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: String,
    pub r#type: String, // "twitch_chat" | "user_speech" | "ai_response" | "auto_commentary"
    pub author: String,
    pub content: String,
    pub timestamp: String,
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
}

impl SessionManager {
    pub fn new(root_dir: PathBuf, tts_mgr: Arc<TtsManager>, log_mgr: Arc<LogManager>) -> Self {
        Self {
            root_dir,
            ai_client: Arc::new(AiClient::new()),
            tts_mgr,
            search_client: Arc::new(WebSearchClient::new()),
            asr_engine: Arc::new(AsrEngine::new()),
            audio_input_mgr: Arc::new(AudioInputManager::new(log_mgr.clone())),
            events: Arc::new(Mutex::new(Vec::new())),
            is_active: Arc::new(AtomicBool::new(false)),
            auto_commentary_active: Arc::new(AtomicBool::new(false)),
            is_collecting_prompt: Arc::new(AtomicBool::new(false)),
            last_speak_time: Arc::new(Mutex::new(Instant::now())),
            log_mgr,
        }
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    pub fn get_effective_gemini_key(&self) -> String {
        let st = crate::settings::load_settings_file(&self.root_dir);
        let mut key = st.get("gemini_api_key").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if key.trim().is_empty() {
            key = std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY")).unwrap_or_default();
        }
        key.trim().to_string()
    }

    pub async fn save_event_to_memory(&self, event: &SessionEvent) {
        let doc_text = event.content.trim().to_string();
        if doc_text.is_empty() {
            return;
        }

        let vectors = match self.asr_engine.ws_client.embed_texts(&[doc_text.clone()]).await {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None,
        };

        let st = crate::settings::load_settings_file(&self.root_dir);
        let user_id_val = st.get("user_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "User".to_string());

        let has_vector = vectors.is_some();
        let mem_item = MemoryItem {
            id: event.id.clone(),
            document: doc_text.clone(),
            memory_type: event.r#type.clone(),
            source: event.author.clone(),
            timestamp: event.timestamp.clone(),
            user_id: Some(user_id_val),
        };
        match lance_memory::insert_memory_batch(&self.root_dir, vec![mem_item], vectors).await {
            Ok(_) => {
                let vec_status = if has_vector { "with GLuCoSE-base-ja vector" } else { "zero-vector fallback" };
                self.log_mgr.info("Memory", &format!("Saved event to LanceDB ({}): '{}'", vec_status, doc_text));
            }
            Err(e) => {
                self.log_mgr.error("Memory", &format!("Failed to save event to LanceDB: {}", e));
            }
        }
    }

    /// ローカル GLuCoSE-base-ja 埋め込みモデルを用いたセマンティック記憶検索
    pub async fn get_relevant_memory_context(&self, query_text: &str) -> String {
        let memories = if let Ok(q_vecs) = self.asr_engine.ws_client.embed_texts(&[query_text.to_string()]).await {
            if let Some(first_vec) = q_vecs.first() {
                if first_vec.len() == (lance_memory::VECTOR_DIM as usize) {
                    lance_memory::search_similar_memories(&self.root_dir, first_vec, 5).await.unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let memories = if memories.is_empty() {
            // ベクトル検索結果がない場合は直近の履歴を取得
            if let Ok(mem_res) = lance_memory::list_memories(&self.root_dir, Some(5), Some(0)).await {
                mem_res.memories
            } else {
                Vec::new()
            }
        } else {
            memories
        };

        let mut mem_docs = Vec::new();
        for m in memories {
            if !m.document.is_empty() {
                mem_docs.push(format!("- [{}] {}", m.memory_type, m.document));
            }
        }

        if mem_docs.is_empty() {
            String::new()
        } else {
            self.log_mgr.info("Memory", &format!("Retrieved {} relevant semantic memories from LanceDB (GLuCoSE-base-ja)", mem_docs.len()));
            format!("\n\n### 過去の関連記憶:\n{}", mem_docs.join("\n"))
        }
    }

    pub fn start_session(&self) {
        self.is_active.store(true, Ordering::SeqCst);
        self.events.lock().clear();
        *self.last_speak_time.lock() = Instant::now();
        self.log_mgr.info("Session", "Game Assistant AI Session started");
    }

    pub fn start_session_with_services(
        self: &Arc<Self>,
        app_handle: Option<AppHandle>,
        twitch_service: Option<Arc<crate::twitch::TwitchService>>,
    ) {
        if self.is_active.load(Ordering::SeqCst) {
            return;
        }
        self.start_session();

        // 0. LanceDB 自動スナップショットバックアップ
        if let Ok(backup_name) = lance_memory::backup_lance_db(&self.root_dir) {
            self.log_mgr.info("LanceDB", &format!("Auto-backup created: {}", backup_name));
        }

        let settings = crate::settings::load_settings_file(&self.root_dir);

        // 1. Twitch サービス連携
        if let Some(twitch_svc) = twitch_service {
            let twitch_channel = settings.get("twitch_channel")
                .or_else(|| settings.get("twitch_bot_channel"))
                .or_else(|| settings.get("user_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            let twitch_bot_username = settings.get("twitch_bot_username")
                .and_then(|v| v.as_str())
                .unwrap_or("justinfan12345")
                .trim()
                .to_string();

            let twitch_bot_token = settings.get("twitch_access_token")
                .or_else(|| settings.get("twitch_bot_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            if !twitch_channel.is_empty() {
                let log_mgr_twitch = self.log_mgr.clone();
                let app_h = app_handle.clone();
                let session_self = self.clone();

                log_mgr_twitch.info("Twitch", &format!("Logging in as '{}' to channel '{}'...", twitch_bot_username, twitch_channel));

                tauri::async_runtime::spawn(async move {
                    let bot_settings = crate::twitch::TwitchBotSettings {
                        channel: twitch_channel.clone(),
                        bot_nick: twitch_bot_username.clone(),
                        oauth_token: twitch_bot_token,
                    };

                    let session_for_msg = session_self.clone();
                    let app_for_msg = app_h.clone();
                    let on_msg = Arc::new(move |msg: crate::twitch::TwitchChatMessage| {
                        let sess = session_for_msg.clone();
                        let app_m = app_for_msg.clone();
                        tauri::async_runtime::spawn(async move {
                            // Twitch メッセージをイベント＆LanceDB に保存
                            let tw_event = SessionEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                r#type: "twitch_chat".to_string(),
                                author: msg.author.clone(),
                                content: msg.content.clone(),
                                timestamp: Local::now().to_rfc3339(),
                            };
                            sess.add_event(tw_event.clone());
                            sess.save_event_to_memory(&tw_event).await;

                            let st = crate::settings::load_settings_file(&sess.root_dir);
                            let gemini_key = sess.get_effective_gemini_key();
                            let brave_key = st.get("brave_api_key").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                            let model = st.get("gemini_model").and_then(|v| v.as_str()).unwrap_or("gemini-2.0-flash").to_string();
                            let sys_prompt = crate::prompts::get_prompt(&sess.root_dir, "system_instruction_character");
                            let tts_cfg = extract_tts_settings(&st);

                            let _ = sess.process_user_input(
                                &msg.author,
                                &msg.content,
                                "twitch_chat",
                                &gemini_key,
                                &brave_key,
                                &model,
                                &sys_prompt,
                                &tts_cfg,
                                app_m.as_ref(),
                            ).await;
                        });
                    });

                    if let Err(e) = twitch_svc.connect(bot_settings, app_h, Some(on_msg)).await {
                        log_mgr_twitch.error("Twitch", &format!("Twitch connection error: {}", e));
                    } else {
                        log_mgr_twitch.info("Twitch", &format!("Connected to Twitch channel '{}' successfully", twitch_channel));
                    }
                });
            }
        }

        // 2. 音声入力 & Faster-Whisper GPU IPC ワーカーの起動 & コールバック登録
        let audio_device = settings.get("audio_device").and_then(|v| v.as_str()).unwrap_or("Default").to_string();
        let session_for_callback = self.clone();
        let app_for_callback = app_handle.clone();
        let log_mgr_callback = self.log_mgr.clone();

        if let Err(e) = self.asr_engine.ws_client.start(move |stream: String, text: String, is_final: bool, latency_ms: Option<f64>| {
            let session_cl = session_for_callback.clone();
            let app_cl = app_for_callback.clone();
            let log_cl = log_mgr_callback.clone();

            tauri::async_runtime::spawn(async move {
                let is_collecting = session_cl.is_collecting_prompt.load(Ordering::SeqCst);
                let display_text = if stream == "discord" {
                    format!("[Discord] {}", text)
                } else {
                    text.clone()
                };

                if let Some(ref handle) = app_cl {
                    let _ = handle.emit("asr_result", serde_json::json!({
                        "text": display_text,
                        "is_final": is_final,
                        "stream": stream,
                        "is_prompt": is_collecting && stream == "mic",
                        "latency_ms": latency_ms,
                    }));
                }

                if is_final {
                    let lat_str = latency_ms.map(|l| format!(" [{:.1}ms]", l)).unwrap_or_default();
                    log_cl.info("ASR", &format!("Finalize [{}]: '{}'{}", stream, display_text, lat_str));

                    // VRAM 蓄積による遅延自動検知 & Whisper 自動再起動
                    if let Some(lat) = latency_ms {
                        let st_file = crate::settings::load_settings_file(&session_cl.root_dir);
                        let auto_restart = st_file.get("auto_restart_whisper").and_then(|v| v.as_bool()).unwrap_or(true);
                        let threshold = st_file.get("whisper_latency_threshold_ms").and_then(|v| v.as_f64()).unwrap_or(2500.0);

                        if auto_restart && lat > threshold {
                            log_cl.warn("ASR", &format!("Detected high inference latency ({:.1}ms > {:.0}ms). Auto-restarting Whisper GPU worker to clear VRAM cache...", lat, threshold));
                            let ws_cl = session_cl.asr_engine.ws_client.clone();
                            let app_cl_restart = app_cl.clone();
                            let log_cl_restart = log_cl.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(re) = ws_cl.restart().await {
                                    log_cl_restart.error("ASR", &format!("Auto-restart Whisper failed: {}", re));
                                } else {
                                    log_cl_restart.info("ASR", "Whisper GPU worker auto-restarted successfully (VRAM & cache refreshed).");
                                    if let Some(ref h) = app_cl_restart {
                                        let _ = h.emit("toast_notice", serde_json::json!({
                                            "message": "⚡ Whisper の推論遅延を検知したため、GPU ワーカーを自動再起動して VRAM をリフレッシュしました",
                                            "type": "info"
                                        }));
                                    }
                                }
                            });
                        }
                    }

                    let st_file = crate::settings::load_settings_file(&session_cl.root_dir);
                    let gemini_key = session_cl.get_effective_gemini_key();

                    let brave_key = st_file.get("brave_api_key").and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| std::env::var("BRAVE_API_KEY").unwrap_or_default());

                    let model = st_file.get("gemini_model").and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".to_string()));

                    let sys_prompt = crate::prompts::get_prompt(&session_cl.root_dir, "system_instruction_character");
                    let tts_cfg = extract_tts_settings(&st_file);

                    if stream == "discord" {
                        // Discord 音声文字起こしをイベント & LanceDB に保存
                        let discord_event = SessionEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            r#type: "discord_speech".to_string(),
                            author: "Discord".to_string(),
                            content: text.clone(),
                            timestamp: Local::now().to_rfc3339(),
                        };
                        session_cl.add_event(discord_event.clone());
                        session_cl.save_event_to_memory(&discord_event).await;
                        return;
                    }

                    if stream == "mic" {
                        // 1. ストップワード検知（「ストップ」「だまって」「静かに」等）
                        if text.contains("ストップ") || text.contains("だまって") || text.contains("静かに") {
                            log_cl.info("ASR", "Stop word detected! Stopping audio playback...");
                            session_cl.tts_mgr.stop_playback();
                            session_cl.is_collecting_prompt.store(false, Ordering::SeqCst);
                            return;
                        }

                        // 2. 通常の独り言・発話もすべてイベント & LanceDB に保存
                        let user_speech_event = SessionEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            r#type: "user_speech".to_string(),
                            author: "User".to_string(),
                            content: text.clone(),
                            timestamp: Local::now().to_rfc3339(),
                        };
                        session_cl.add_event(user_speech_event.clone());
                        session_cl.save_event_to_memory(&user_speech_event).await;

                        let raw_ww = st_file.get("custom_wake_words").and_then(|v| v.as_str()).unwrap_or("ねえぐり, ねぐり, ネグリ, アシスタント, ヘイぐり");
                        let custom_wws: Vec<String> = raw_ww.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        let (is_ww, clean_prompt) = AsrEngine::check_wake_word(&text, &custom_wws);

                        log_cl.info("ASR", &format!("Wake word check: triggered={}, clean_prompt='{}', is_collecting={}", 
                            is_ww, clean_prompt, session_cl.is_collecting_prompt.load(Ordering::SeqCst)));

                        if is_ww {
                            if clean_prompt.chars().count() >= 2 {
                                // パターン A: 「ねえぐり、〇〇して」と一息で言われた場合 -> プロンプトとして UI に emit
                                if let Some(ref handle) = app_cl {
                                    let _ = handle.emit("asr_result", serde_json::json!({
                                        "text": clean_prompt.clone(),
                                        "is_final": true,
                                        "stream": "mic",
                                        "is_prompt": true
                                    }));
                                }

                                log_cl.info("ASR", &format!("Wake word with prompt detected! Playing nod and sending prompt: '{}'", clean_prompt));
                                let _ = session_cl.tts_mgr.play_random_nod(&session_cl.root_dir).await;
                                session_cl.is_collecting_prompt.store(false, Ordering::SeqCst);

                                if let Err(e) = session_cl.process_user_input(
                                    "User",
                                    &clean_prompt,
                                    "user_speech",
                                    &gemini_key,
                                    &brave_key,
                                    &model,
                                    &sys_prompt,
                                    &tts_cfg,
                                    app_cl.as_ref(),
                                ).await {
                                    log_cl.error("AI", &format!("Gemini process error: {}", e));
                                }
                            } else {
                                // パターン B: 「ねえぐり」単体検知 -> 相槌を打ってプロンプト待機モードへ
                                log_cl.info("ASR", "Wake word only detected! Playing nod confirmation and entering prompt collection mode...");
                                let _ = session_cl.tts_mgr.play_random_nod(&session_cl.root_dir).await;
                                session_cl.is_collecting_prompt.store(true, Ordering::SeqCst);
                            }
                        } else if session_cl.is_collecting_prompt.load(Ordering::SeqCst) {
                            // プロンプト待機モード中に届いたユーザーの追従発話
                            let (_, candidate) = AsrEngine::check_wake_word(&text, &custom_wws);
                            let prompt_to_send = if candidate.is_empty() { &text } else { &candidate };

                            if prompt_to_send.chars().count() >= 2 {
                                if let Some(ref handle) = app_cl {
                                    let _ = handle.emit("asr_result", serde_json::json!({
                                        "text": prompt_to_send.to_string(),
                                        "is_final": true,
                                        "stream": "mic",
                                        "is_prompt": true
                                    }));
                                }

                                log_cl.info("ASR", &format!("Prompt collection received: '{}'. Playing nod confirmation and sending to Gemini...", prompt_to_send));
                                // ユーザーのプロンプトを受け取った合図として2回目の相槌音を再生
                                let _ = session_cl.tts_mgr.play_random_nod(&session_cl.root_dir).await;
                                session_cl.is_collecting_prompt.store(false, Ordering::SeqCst);

                                if let Err(e) = session_cl.process_user_input(
                                    "User",
                                    prompt_to_send,
                                    "user_speech",
                                    &gemini_key,
                                    &brave_key,
                                    &model,
                                    &sys_prompt,
                                    &tts_cfg,
                                    app_cl.as_ref(),
                                ).await {
                                    log_cl.error("AI", &format!("Gemini process error: {}", e));
                                }
                            }
                        } else if custom_wws.is_empty() {
                            // ウェイクワード未設定時は全発話を Gemini に送信
                            if let Err(e) = session_cl.process_user_input(
                                "User",
                                &text,
                                "user_speech",
                                &gemini_key,
                                &brave_key,
                                &model,
                                &sys_prompt,
                                &tts_cfg,
                                app_cl.as_ref(),
                            ).await {
                                log_cl.error("AI", &format!("Gemini process error: {}", e));
                            }
                        }
                    }
                }
            });
        }) {
            self.log_mgr.error("ASR", &format!("Failed to start Faster-Whisper GPU worker: {}", e));
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
        let enable_discord = settings.get("enable_discord_capture").and_then(|v| v.as_bool()).unwrap_or(false);
        if enable_discord {
            let discord_device = settings.get("discord_audio_device").and_then(|v| v.as_str()).unwrap_or("Default").to_string();
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

        tauri::async_runtime::spawn(async move {
            log_mgr_comm.info("Commentary", "Autonomous Live Commentary & Visual Context engine activated");

            while session_for_comm.is_active() && session_for_comm.auto_commentary_active.load(Ordering::SeqCst) {
                let st = crate::settings::load_settings_file(&session_for_comm.root_dir);
                let enable_auto = st.get("enable_auto_commentary").and_then(|v| v.as_bool()).unwrap_or(true);
                if !enable_auto {
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }

                let min_sec = st.get("auto_commentary_min").and_then(|v| v.as_u64()).unwrap_or(200);
                let max_sec = st.get("auto_commentary_max").and_then(|v| v.as_u64()).unwrap_or(400).max(min_sec);
                let avoid_dur = st.get("auto_commentary_avoid_duration").and_then(|v| v.as_u64()).unwrap_or(5);

                let cycle_sec = {
                    let mut rng = rand::thread_rng();
                    use rand::Rng;
                    rng.gen_range(min_sec..=max_sec)
                };

                log_mgr_comm.info("Commentary", &format!("Next autonomous commentary scheduled in {} seconds", cycle_sec));

                let start_time = Instant::now();

                while start_time.elapsed().as_secs() < cycle_sec {
                    if !session_for_comm.is_active() || !session_for_comm.auto_commentary_active.load(Ordering::SeqCst) {
                        return;
                    }
                    let elapsed = start_time.elapsed().as_secs();
                    let remaining = cycle_sec.saturating_sub(elapsed);

                    if let Some(ref handle) = app_for_comm {
                        let _ = handle.emit("auto_commentary_status", serde_json::json!({
                            "is_running": true,
                            "remaining_sec": remaining,
                            "total_sec": cycle_sec
                        }));
                    }

                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }

                // 割り込み回避チェック（誰かが直前に発話中またはTTS再生中か）
                loop {
                    if !session_for_comm.is_active() || !session_for_comm.auto_commentary_active.load(Ordering::SeqCst) {
                        return;
                    }

                    let elapsed_since_last_speak = session_for_comm.last_speak_time.lock().elapsed().as_secs();
                    if elapsed_since_last_speak >= avoid_dur {
                        break;
                    }

                    log_mgr_comm.info("Commentary", &format!("Speech activity detected, delaying commentary by {}s...", avoid_dur));
                    tokio::time::sleep(tokio::time::Duration::from_secs(avoid_dur)).await;
                }

                let gemini_key = session_for_comm.get_effective_gemini_key();
                let model = st.get("gemini_model").and_then(|v| v.as_str()).unwrap_or("gemini-2.0-flash").to_string();
                let tts_cfg = extract_tts_settings(&st);

                let _ = session_for_comm.execute_auto_commentary(
                    &gemini_key,
                    &model,
                    &tts_cfg,
                    app_for_comm.as_ref(),
                ).await;
            }
        });
    }

    pub fn stop_session(&self) {
        self.is_active.store(false, Ordering::SeqCst);
        self.auto_commentary_active.store(false, Ordering::SeqCst);
        self.audio_input_mgr.stop();
        self.asr_engine.ws_client.stop();
        self.tts_mgr.stop_playback();
        self.log_mgr.info("Session", "Game Assistant AI Session stopped");
    }

    pub fn stop_session_with_services(&self, twitch_service: Option<&crate::twitch::TwitchService>, app_handle: Option<AppHandle>) {
        self.stop_session();
        if let Some(twitch) = twitch_service {
            twitch.disconnect();
        }

        let st = crate::settings::load_settings_file(&self.root_dir);
        let create_blog = st.get("create_blog_post").and_then(|v| v.as_bool()).unwrap_or(false);

        if create_blog {
            let session_clone = self.clone();
            let app_h = app_handle;
            let log_mgr = self.log_mgr.clone();

            tauri::async_runtime::spawn(async move {
                let gemini_key = session_clone.get_effective_gemini_key();
                let st_file = crate::settings::load_settings_file(&session_clone.root_dir);
                let model = st_file.get("gemini_model").and_then(|v| v.as_str())
                    .unwrap_or("gemini-2.0-flash")
                    .to_string();
                let blog_prompt = crate::prompts::get_prompt(&session_clone.root_dir, "blog_writer_system_prompt");

                log_mgr.info("Blog", "自動ブログ記事生成を開始します (create_blog_post: true)...");
                if let Some(ref h) = app_h {
                    let _ = h.emit("toast_notice", serde_json::json!({
                        "message": "📝 セッション終了を検知しました。AIがnoteブログ記事を自動執筆中...",
                        "type": "info"
                    }));
                }

                match session_clone.generate_blog_article(&gemini_key, &model, &blog_prompt).await {
                    Ok(article) => {
                        let blogs_dir = session_clone.root_dir.join("blogs");
                        let _ = std::fs::create_dir_all(&blogs_dir);
                        let filename = format!("{}.md", Local::now().format("%Y-%m-%d_%H-%M-%S"));
                        let filepath = blogs_dir.join(&filename);

                        if let Err(e) = std::fs::write(&filepath, &article) {
                            log_mgr.error("Blog", &format!("ブログ記事のファイル書き込みに失敗しました: {}", e));
                        } else {
                            log_mgr.info("Blog", &format!("✅ ブログ記事を自動保存しました: {:?}", filepath));
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
                    }
                }
            });
        }
    }

    pub fn get_events(&self) -> Vec<SessionEvent> {
        self.events.lock().clone()
    }

    /// イベント追加
    pub fn add_event(&self, event: SessionEvent) {
        let mut evts = self.events.lock();
        evts.push(event);
        if evts.len() > 100 {
            evts.remove(0);
        }
    }

    /// 自立型ツッコミ・実況の実行 (指示文はUI/履歴に載せず、純粋なツッコミのみを生成・保存・発話)
    pub async fn execute_auto_commentary(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        tts_settings: &TtsSettings,
        app_handle: Option<&AppHandle>,
    ) -> Result<String, String> {
        self.log_mgr.info("Commentary", "Generating autonomous live commentary on current gameplay...");

        let st = crate::settings::load_settings_file(&self.root_dir);
        let sys_prompt = crate::prompts::get_prompt(&self.root_dir, "auto_commentary_prompt");

        // LanceDB 関連記憶をセマンティック検索 (直近の会話またはゲーム状況)
        let memory_context = self.get_relevant_memory_context("ゲームプレイ状況 実況 解説").await;

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
        let use_image = st.get("use_image").and_then(|v| v.as_bool()).unwrap_or(true);
        let screen_b64 = if use_image {
            let win_name = st.get("window").and_then(|v| v.as_str()).unwrap_or("");
            if !win_name.is_empty() {
                self.log_mgr.info("Visual", &format!("Capturing target window for commentary: '{}'", win_name));
                window_capture::capture_window_base64(win_name).or_else(|| {
                    window_capture::capture_primary_screen_base64()
                })
            } else {
                window_capture::capture_primary_screen_base64()
            }
        } else {
            None
        };

        let full_system_instruction = format!(
            "{}{}{}",
            sys_prompt,
            memory_context,
            history_context
        );

        let chat_messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "状況を見て、テンポよく実況ツッコミやボヤキを1〜2文でお願いします。".to_string(),
        }];

        let disable_thinking = st.get("disable_thinking_mode").and_then(|v| v.as_bool()).unwrap_or(true);
        let thinking_budget = if disable_thinking { Some(0) } else { None };

        let options = AiGenerateOptions {
            system_instruction: Some(full_system_instruction),
            temperature: Some(0.8),
            max_output_tokens: Some(200),
            image_base64: screen_b64,
            thinking_budget,
        };

        if let Some(handle) = app_handle {
            let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": true }));
        }

        let ai_res = match self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &chat_messages, &options)
            .await
        {
            Ok(res) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": false }));
                }
                res
            }
            Err(e) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": false }));
                }
                self.log_mgr.error("Commentary", &format!("Auto Commentary generation error: {}", e));
                return Err(e);
            }
        };

        let clean_ai_res = ai_res.trim().to_string();

        if !clean_ai_res.is_empty() {
            self.log_mgr.info("Commentary", &format!("Auto Commentary: {}", clean_ai_res));

            let ai_event = SessionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                r#type: "auto_commentary".to_string(),
                author: "AI_Auto".to_string(),
                content: clean_ai_res.clone(),
                timestamp: Local::now().to_rfc3339(),
            };
            self.add_event(ai_event.clone());
            self.save_event_to_memory(&ai_event).await;

            if let Some(handle) = app_handle {
                let _ = handle.emit("session-event", &ai_event);
            }

            // 音声合成 & 発話再生
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

        self.log_mgr.info("Input", &format!("[{}] {}: {}", input_type, author, clean_text));

        // イベント記録
        let user_event = SessionEvent {
            id: uuid::Uuid::new_v4().to_string(),
            r#type: input_type.to_string(),
            author: author.to_string(),
            content: clean_text.to_string(),
            timestamp: Local::now().to_rfc3339(),
        };
        self.add_event(user_event.clone());
        if let Some(handle) = app_handle {
            let _ = handle.emit("session-event", &user_event);
        }

        // LanceDB 関連記憶をセマンティック検索 (GLuCoSE-base-ja 埋め込みモデル)
        let memory_context = self.get_relevant_memory_context(clean_text).await;

        // Web 検索が必要か判定
        let is_search_needed = clean_text.contains("検索") || clean_text.contains("調べて") || clean_text.contains("最新情報");
        let search_context = if is_search_needed {
            self.log_mgr.info("WebSearch", &format!("Performing Brave web search for query: '{}'...", clean_text));
            let res = self.search_client.search_and_format(clean_text, brave_api_key).await;
            format!("\n\n{}", res.summary_text)
        } else {
            String::new()
        };

        let st = crate::settings::load_settings_file(&self.root_dir);
        let use_image = st.get("use_image").and_then(|v| v.as_bool()).unwrap_or(true);

        // ゲーム画面のキャプチャ（選択中ウィンドウを優先、なければプライマリスクリーン）
        let screen_b64 = if use_image {
            let win_name = st.get("window").and_then(|v| v.as_str()).unwrap_or("");
            if !win_name.is_empty() {
                self.log_mgr.info("Visual", &format!("Capturing target window: '{}'", win_name));
                window_capture::capture_window_base64(win_name).or_else(|| {
                    self.log_mgr.warn("Visual", "Window capture fallback to primary screen");
                    window_capture::capture_primary_screen_base64()
                })
            } else {
                self.log_mgr.info("Visual", "Capturing primary screen...");
                window_capture::capture_primary_screen_base64()
            }
        } else {
            None
        };

        // プロンプト構築
        let full_system_instruction = format!(
            "{}{}{}",
            system_prompt,
            memory_context,
            search_context
        );

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
        let disable_thinking = st.get("disable_thinking_mode").and_then(|v| v.as_bool()).unwrap_or(true);
        let thinking_budget = if disable_thinking { Some(0) } else { None };

        let options = AiGenerateOptions {
            system_instruction: Some(full_system_instruction),
            temperature: Some(0.7),
            max_output_tokens: Some(300),
            image_base64: screen_b64,
            thinking_budget,
        };

        self.log_mgr.info("Gemini", &format!("Requesting AI generation for: '{}' (model: {})...", clean_text, gemini_model));

        if let Some(handle) = app_handle {
            let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": true }));
        }

        // Gemini AI 推論
        let ai_res = match self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &chat_messages, &options)
            .await
        {
            Ok(res) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": false }));
                }
                res
            }
            Err(e) => {
                if let Some(handle) = app_handle {
                    let _ = handle.emit("gemini_status", serde_json::json!({ "is_generating": false }));
                }
                self.log_mgr.error("Gemini", &format!("Gemini API failed: {}", e));
                return Err(e);
            }
        };

        let clean_ai_res = ai_res.trim().to_string();

        if !clean_ai_res.is_empty() {
            self.log_mgr.info("AI", &format!("Generated response: {}", clean_ai_res));

            let ai_event = SessionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                r#type: "ai_response".to_string(),
                author: "Assistant".to_string(),
                content: clean_ai_res.clone(),
                timestamp: Local::now().to_rfc3339(),
            };
            self.add_event(ai_event.clone());
            if let Some(handle) = app_handle {
                let _ = handle.emit("session-event", &ai_event);
            }

            self.save_event_to_memory(&ai_event).await;

            // 音声合成 & 発話再生
            self.log_mgr.info("TTS", "Synthesizing and playing speech...");
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
        let events = self.get_events();
        if events.is_empty() {
            return Err("会話履歴がありません。".to_string());
        }

        self.log_mgr.info("Blog", "Generating note blog article from session history...");

        let mut logs = String::new();
        for ev in &events {
            logs.push_str(&format!("[{}] {}: {}\n", ev.timestamp, ev.author, ev.content));
        }

        let st = crate::settings::load_settings_file(&self.root_dir);

        // 1. スキル適用の判定と読み込み (enable_blog_skills & enabled_blog_skills)
        let enable_skills = st.get("enable_blog_skills").and_then(|v| v.as_bool()).unwrap_or(true);
        let mut skill_instructions = String::new();

        if enable_skills {
            let enabled_skills: Vec<String> = st
                .get("enabled_blog_skills")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| vec!["k0ta-writing-style".to_string()]);

            if !enabled_skills.is_empty() {
                let skills_text = crate::settings::load_enabled_skills_text(&self.root_dir, &enabled_skills);
                if !skills_text.is_empty() {
                    self.log_mgr.info("Blog", &format!("ブログ記事生成にスキルを適用します: {:?}", enabled_skills));
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
        let blog_use_thinking = st.get("blog_use_thinking").and_then(|v| v.as_bool()).unwrap_or(true);
        let thinking_budget = if blog_use_thinking { Some(2048) } else { Some(0) };

        self.log_mgr.info("Blog", &format!("ブログ記事生成パラメータ (model: {}, thinking: {})", gemini_model, blog_use_thinking));

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

        self.log_mgr.info("Blog", &format!("✅ note ブログ記事の生成に成功しました (文字数: {})", blog_article.chars().count()));
        Ok(blog_article)
    }
}

/// settings.json から最新の TTS 設定を抽出
pub fn extract_tts_settings(st: &serde_json::Value) -> TtsSettings {
    let tts_engine = st.get("tts_engine").and_then(|v| v.as_str()).unwrap_or("voicevox").to_string();
    let speaker_id = st.get("speaker_id").and_then(|v| v.as_i64()).unwrap_or(46) as i32;
    let vits2_speaker_id = st.get("vits2_speaker_id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let voicevox_url = st.get("voicevox_url").and_then(|v| v.as_str()).unwrap_or("http://127.0.0.1:50021").to_string();
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
