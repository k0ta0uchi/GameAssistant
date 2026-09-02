pub mod resource;
pub mod window_capture;
pub mod settings;
pub mod audio;
pub mod lance_memory;
pub mod tts;
pub mod twitch;
pub mod web_search;
pub mod ai_client;
pub mod session;
pub mod logger;
pub mod asr;
pub mod audio_input;
pub mod prompts;

pub mod model_manager;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use resource::{ResourceManager, SystemResources};
use settings::SkillsResponse;
use audio::AudioDevicesResponse;
use lance_memory::{MemoryItem, MemoryListResponse};
use tts::{TtsManager, TtsSettings};
use twitch::{TwitchBotSettings, TwitchService};
use web_search::{WebSearchClient, WebSearchResponse};
use ai_client::{AiClient, AiGenerateOptions, ChatMessage};
use session::{SessionEvent, SessionManager};
use logger::{LogEntry, LogManager};
use model_manager::{ModelManager, ModelStatus};

struct AppState {
    root_dir: PathBuf,
    resource_mgr: ResourceManager,
    tts_mgr: Arc<TtsManager>,
    twitch_service: Arc<TwitchService>,
    web_search_client: Arc<WebSearchClient>,
    ai_client: Arc<AiClient>,
    session_mgr: Arc<SessionManager>,
    log_mgr: Arc<LogManager>,
    model_mgr: Arc<ModelManager>,
}

pub fn resolve_project_root() -> PathBuf {
    // 1. カレントディレクトリ
    if let Ok(current) = std::env::current_dir() {
        if current.join("settings.json").exists() || current.join("scripts").join("asr_server.py").exists() {
            return current;
        }
        if let Some(parent) = current.parent() {
            if parent.join("settings.json").exists() || parent.join("scripts").join("asr_server.py").exists() {
                return parent.to_path_buf();
            }
        }
    }

    // 2. 実行ファイル（EXE）のディレクトリ及びその上位
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            if exe_dir.join("settings.json").exists() || exe_dir.join("scripts").join("asr_server.py").exists() {
                return exe_dir.to_path_buf();
            }
            if let Some(parent) = exe_dir.parent() {
                if parent.join("settings.json").exists() || parent.join("scripts").join("asr_server.py").exists() {
                    return parent.to_path_buf();
                }
            }
        }
    }

    // 3. 既知のワークスペースパス
    let default_path = PathBuf::from("C:/Workspace/GameAssistant");
    if default_path.exists() {
        return default_path;
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// -------------------------------------------------------------
// Tauri Commands (Rust Native 処理)
// -------------------------------------------------------------

#[tauri::command]
fn get_system_resources(state: State<AppState>) -> SystemResources {
    state.resource_mgr.get_resources()
}

#[tauri::command]
fn list_windows() -> Vec<String> {
    window_capture::list_windows()
}

#[tauri::command]
fn capture_window_preview(state: State<AppState>, title: String) -> Option<String> {
    state.log_mgr.info("Capture", &format!("Capturing window preview for: '{}'", title));
    let result = window_capture::capture_window_base64(&title);
    if result.is_some() {
        state.log_mgr.info("Capture", "Window preview captured successfully");
    } else {
        state.log_mgr.warn("Capture", &format!("Failed to capture window preview for: '{}'", title));
    }
    result
}

#[tauri::command]
fn load_settings(state: State<AppState>) -> Value {
    settings::load_settings_file(&state.root_dir)
}

#[tauri::command]
fn save_setting(state: State<AppState>, key: String, value: Value) -> Result<Value, String> {
    if key == "preallocate_vram" {
        if let Some(enable) = value.as_bool() {
            let _ = state.session_mgr.asr_engine.ws_client.set_preallocate_vram(enable);
            state.log_mgr.info("System", &format!("VRAM Preallocation updated: {}", enable));
        }
    }
    settings::save_setting_key(&state.root_dir, &key, value)
}

#[tauri::command]
fn list_skills(state: State<AppState>) -> SkillsResponse {
    settings::scan_skills(&state.root_dir)
}

#[tauri::command]
fn get_skill_content(state: State<AppState>, id: String) -> Result<String, String> {
    settings::get_skill_content(&state.root_dir, &id)
}

#[tauri::command]
fn save_skill_content(state: State<AppState>, id: String, content: String) -> Result<SkillsResponse, String> {
    settings::save_skill_content(&state.root_dir, &id, &content)?;
    state.log_mgr.info("Settings", &format!("Saved customized skill content: '{}'", id));
    Ok(settings::scan_skills(&state.root_dir))
}

#[tauri::command]
fn get_prompts(state: State<AppState>) -> Vec<prompts::PromptItem> {
    prompts::get_all_prompts(&state.root_dir)
}

#[tauri::command]
fn save_prompt(state: State<AppState>, id: String, value: String) -> Result<Vec<prompts::PromptItem>, String> {
    prompts::save_prompt_value(&state.root_dir, &id, &value)?;
    state.log_mgr.info("Settings", &format!("Saved customized prompt: '{}'", id));
    Ok(prompts::get_all_prompts(&state.root_dir))
}

#[tauri::command]
fn reset_prompt(state: State<AppState>, id: String) -> Result<Vec<prompts::PromptItem>, String> {
    prompts::reset_prompt_value(&state.root_dir, &id)?;
    state.log_mgr.info("Settings", &format!("Reset prompt to default: '{}'", id));
    Ok(prompts::get_all_prompts(&state.root_dir))
}

#[tauri::command]
fn list_audio_devices() -> AudioDevicesResponse {
    audio::list_input_devices()
}

#[tauri::command]
async fn list_lance_memories(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> Result<MemoryListResponse, String> {
    lance_memory::list_memories(&state.root_dir, limit, offset).await
}

#[tauri::command]
async fn delete_lance_memory(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    lance_memory::delete_memory(&state.root_dir, &id).await
}

#[tauri::command]
async fn delete_lance_memories_bulk(state: State<'_, AppState>, ids: Vec<String>) -> Result<usize, String> {
    lance_memory::delete_memories_bulk(&state.root_dir, &ids).await
}

#[tauri::command]
async fn import_memories_to_lance(
    state: State<'_, AppState>,
    items: Vec<MemoryItem>,
    vectors: Option<Vec<Vec<f32>>>,
) -> Result<usize, String> {
    lance_memory::insert_memory_batch(&state.root_dir, items, vectors).await
}

#[tauri::command]
fn lance_backup(state: State<AppState>) -> Result<String, String> {
    let res = lance_memory::backup_lance_db(&state.root_dir)?;
    state.log_mgr.info("LanceDB", &format!("Created LanceDB backup: {}", res));
    Ok(res)
}

#[tauri::command]
fn lance_list_backups(state: State<AppState>) -> Result<Vec<String>, String> {
    lance_memory::list_lance_backups(&state.root_dir)
}

#[tauri::command]
fn lance_restore(state: State<AppState>, backup_name: String) -> Result<(), String> {
    lance_memory::restore_lance_backup(&state.root_dir, &backup_name)?;
    state.log_mgr.info("LanceDB", &format!("Restored LanceDB from backup: {}", backup_name));
    Ok(())
}

#[tauri::command]
async fn lance_export_json(state: State<'_, AppState>, output_filename: Option<String>) -> Result<String, String> {
    let res = lance_memory::export_lance_memories_json(&state.root_dir, output_filename).await?;
    state.log_mgr.info("LanceDB", &res);
    Ok(res)
}

// --- TTS 音声合成 & 再生 ---
#[tauri::command]
async fn tts_speak(state: State<'_, AppState>, text: String, settings: Option<TtsSettings>) -> Result<(), String> {
    let tts_cfg = settings.unwrap_or_default();
    state.tts_mgr.speak(&text, &tts_cfg).await
}

#[tauri::command]
fn tts_stop(state: State<AppState>) {
    state.tts_mgr.stop_playback();
}

#[tauri::command]
async fn tts_play_nod(state: State<'_, AppState>) -> Result<(), String> {
    state.tts_mgr.play_random_nod(&state.root_dir).await
}

// --- Twitch IRC 連携 ---
#[tauri::command]
async fn twitch_connect(app: AppHandle, state: State<'_, AppState>, settings: TwitchBotSettings) -> Result<(), String> {
    state.twitch_service.connect(settings, Some(app), None).await
}

#[tauri::command]
fn twitch_send(state: State<AppState>, channel: String, message: String) -> Result<(), String> {
    state.twitch_service.send_chat(&channel, &message)
}

#[tauri::command]
fn twitch_disconnect(state: State<AppState>) {
    state.twitch_service.disconnect();
}

#[tauri::command]
fn twitch_get_status(state: State<AppState>) -> serde_json::Value {
    serde_json::json!({
        "connected": state.twitch_service.is_connected()
    })
}

#[tauri::command]
async fn twitch_register_code(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
    code: String,
    redirect_uri: Option<String>,
) -> Result<twitch::TwitchTokenResponse, String> {
    let redir = redirect_uri.unwrap_or_else(|| "https://k0ta0uchi.github.io/GameAssistant/auth.html".to_string());
    state.twitch_service.exchange_code(&client_id, &client_secret, &code, &redir).await
}

#[tauri::command]
fn twitch_get_auth_url(client_id: String, redirect_uri: Option<String>) -> String {
    let redir = redirect_uri.unwrap_or_else(|| "https://k0ta0uchi.github.io/GameAssistant/auth.html".to_string());
    TwitchService::get_auth_url(&client_id, &redir)
}

#[tauri::command]
async fn twitch_validate_token(state: State<'_, AppState>, access_token: String) -> Result<twitch::TwitchValidateResponse, String> {
    state.twitch_service.validate_token(&access_token).await
}

#[tauri::command]
async fn twitch_refresh_token(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
    refresh_token: String,
) -> Result<twitch::TwitchTokenResponse, String> {
    state.twitch_service.refresh_token(&client_id, &client_secret, &refresh_token).await
}

// --- Web 検索 ---
#[tauri::command]
async fn web_search_query(state: State<'_, AppState>, query: String, brave_api_key: Option<String>) -> Result<WebSearchResponse, String> {
    let key = brave_api_key.unwrap_or_default();
    Ok(state.web_search_client.search_and_format(&query, &key).await)
}

// --- AI 生成 ---
#[tauri::command]
async fn ai_generate(
    state: State<'_, AppState>,
    gemini_api_key: String,
    model: Option<String>,
    prompt: String,
    system_prompt: Option<String>,
    image_base64: Option<String>,
) -> Result<String, String> {
    let model_name = model.unwrap_or_else(|| "gemini-2.0-flash".to_string());
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    let st = settings::load_settings_file(&state.root_dir);
    let disable_thinking = st.get("disable_thinking_mode").and_then(|v| v.as_bool()).unwrap_or(true);
    let thinking_budget = if disable_thinking { Some(0) } else { None };

    let options = AiGenerateOptions {
        system_instruction: system_prompt,
        temperature: Some(0.7),
        max_output_tokens: Some(1024),
        image_base64,
        thinking_budget,
    };
    state.ai_client.generate_gemini(&gemini_api_key, &model_name, &messages, &options).await
}

// --- AI / セッション オーケストレーション ---
#[tauri::command]
fn session_start(app: AppHandle, state: State<AppState>) {
    state.session_mgr.start_session_with_services(Some(app), Some(state.twitch_service.clone()));
}

#[tauri::command]
fn session_stop(app: AppHandle, state: State<AppState>) {
    state.session_mgr.stop_session_with_services(Some(&state.twitch_service), Some(app));
}

use std::sync::atomic::{AtomicBool, Ordering};
static IS_WARMING_UP: AtomicBool = AtomicBool::new(false);
static IS_WARMED_UP: AtomicBool = AtomicBool::new(false);

// --- ASR ウォームアップ (GUI 表示時事前ロード) ---
#[tauri::command]
async fn warmup_asr(state: State<'_, AppState>) -> Result<String, String> {
    if IS_WARMED_UP.load(Ordering::SeqCst) {
        return Ok("Already warmed up".to_string());
    }

    if IS_WARMING_UP.swap(true, Ordering::SeqCst) {
        let asr_engine = state.session_mgr.asr_engine.clone();
        return asr_engine.ws_client.warmup().await.map(|_| "Warmup completed".to_string());
    }

    let asr_engine = state.session_mgr.asr_engine.clone();
    let log_mgr = state.log_mgr.clone();

    log_mgr.info("ASR", "Warmup requested: Preloading Faster-Whisper CUDA INT8 server into VRAM...");

    if let Err(e) = asr_engine.ws_client.warmup().await {
        IS_WARMING_UP.store(false, Ordering::SeqCst);
        return Err(e);
    }

    IS_WARMED_UP.store(true, Ordering::SeqCst);
    IS_WARMING_UP.store(false, Ordering::SeqCst);

    log_mgr.info("ASR", "Faster-Whisper CUDA INT8 warmup complete! Ready for instant transcription.");
    Ok("Warmup completed for Faster-Whisper CUDA INT8".to_string())
}

#[tauri::command]
async fn restart_whisper(state: State<'_, AppState>) -> Result<String, String> {
    state.log_mgr.info("ASR", "Restarting Whisper GPU worker...");
    state.session_mgr.asr_engine.ws_client.restart().await?;
    state.log_mgr.info("ASR", "Whisper GPU worker restarted and warmed up successfully.");
    Ok("Whisper GPU worker restarted successfully".to_string())
}

// --- モデル管理 (Models Manager) ---
#[tauri::command]
fn get_models_status(state: State<AppState>, custom_dir: Option<String>) -> Vec<ModelStatus> {
    ModelManager::scan_models_status(&state.root_dir, custom_dir)
}

#[tauri::command]
async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
    custom_dir: Option<String>,
) -> Result<(), String> {
    let model_mgr = state.model_mgr.clone();
    let root_dir = state.root_dir.clone();
    state.log_mgr.info("Model", &format!("Starting download for model: {}", model_id));
    let res = model_mgr.download_model(app, root_dir, model_id.clone(), custom_dir).await;
    if let Err(ref e) = res {
        state.log_mgr.error("Model", &format!("Failed to download {}: {}", model_id, e));
    } else {
        state.log_mgr.info("Model", &format!("Successfully downloaded model: {}", model_id));
    }
    res
}

#[tauri::command]
fn cancel_download_model(state: State<AppState>, model_id: String) {
    state.log_mgr.info("Model", &format!("Cancelled download for model: {}", model_id));
    state.model_mgr.cancel_download(&model_id);
}

#[tauri::command]
fn session_get_events(state: State<AppState>) -> Vec<SessionEvent> {
    state.session_mgr.get_events()
}

#[tauri::command]
async fn session_process_input(
    app: AppHandle,
    state: State<'_, AppState>,
    author: String,
    text: String,
    input_type: String,
    gemini_api_key: String,
    brave_api_key: Option<String>,
    gemini_model: Option<String>,
    system_prompt: Option<String>,
    tts_settings: Option<TtsSettings>,
) -> Result<String, String> {
    let brave_key = brave_api_key.unwrap_or_default();
    let model = gemini_model.unwrap_or_else(|| "gemini-2.0-flash".to_string());
    let sys_prompt = system_prompt.unwrap_or_default();
    let tts_cfg = tts_settings.unwrap_or_default();

    state
        .session_mgr
        .process_user_input(
            &author,
            &text,
            &input_type,
            &gemini_api_key,
            &brave_key,
            &model,
            &sys_prompt,
            &tts_cfg,
            Some(&app),
        )
        .await
}

#[tauri::command]
async fn session_generate_blog(
    state: State<'_, AppState>,
    gemini_api_key: String,
    gemini_model: Option<String>,
    blog_system_prompt: Option<String>,
) -> Result<String, String> {
    let model = gemini_model.unwrap_or_else(|| "gemini-2.0-flash".to_string());
    let prompt = blog_system_prompt.unwrap_or_default();
    state
        .session_mgr
        .generate_blog_article(&gemini_api_key, &model, &prompt)
        .await
}

#[tauri::command]
fn get_app_logs(state: State<AppState>) -> Vec<LogEntry> {
    state.log_mgr.get_logs()
}

#[tauri::command]
fn clear_app_logs(state: State<AppState>) {
    state.log_mgr.clear();
}

// -------------------------------------------------------------
// アプリケーションエントリポイント
// -------------------------------------------------------------

fn load_dotenv(root_dir: &std::path::Path) {
    let env_path = root_dir.join(".env");
    if let Ok(content) = std::fs::read_to_string(env_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, val)) = trimmed.split_once('=') {
                let k = key.trim();
                let v = val.trim();
                if std::env::var(k).is_err() {
                    std::env::set_var(k, v);
                }
            }
        }
    }
}

pub fn run() {
    let root_dir = resolve_project_root();
    load_dotenv(&root_dir);

    let log_mgr = Arc::new(LogManager::new());
    logger::set_global_logger(log_mgr.clone());

    let tts_mgr = Arc::new(TtsManager::new());
    let twitch_service = Arc::new(TwitchService::new());
    let web_search_client = Arc::new(WebSearchClient::new());
    let ai_client = Arc::new(AiClient::new());
    let session_mgr = Arc::new(SessionManager::new(root_dir.clone(), tts_mgr.clone(), log_mgr.clone()));
    let model_mgr = Arc::new(ModelManager::new());

    let app_state = AppState {
        root_dir: root_dir.clone(),
        resource_mgr: ResourceManager::new(),
        tts_mgr,
        twitch_service,
        web_search_client,
        ai_client,
        session_mgr: session_mgr.clone(),
        log_mgr: log_mgr.clone(),
        model_mgr,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_system_resources,
            list_windows,
            capture_window_preview,
            load_settings,
            save_setting,
            list_skills,
            get_skill_content,
            save_skill_content,
            get_prompts,
            save_prompt,
            reset_prompt,
            list_audio_devices,
            list_lance_memories,
            delete_lance_memory,
            delete_lance_memories_bulk,
            import_memories_to_lance,
            lance_backup,
            lance_list_backups,
            lance_restore,
            lance_export_json,
            tts_speak,
            tts_stop,
            tts_play_nod,
            twitch_connect,
            twitch_send,
            twitch_disconnect,
            twitch_get_status,
            twitch_get_auth_url,
            twitch_register_code,
            twitch_validate_token,
            twitch_refresh_token,
            web_search_query,
            ai_generate,
            session_start,
            session_stop,
            session_get_events,
            session_process_input,
            session_generate_blog,
            warmup_asr,
            restart_whisper,
            get_models_status,
            download_model,
            cancel_download_model,
            get_app_logs,
            clear_app_logs,
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            log_mgr.set_app_handle(app_handle.clone());

            // 1. Rust ネイティブエンジンの起動通知ログ
            log_mgr.info("RustNative", "Pure Rust Native Core & LanceDB Engine Online");
            log_mgr.info("LanceDB", &format!("Database path initialized at: {:?}", root_dir.join("data/lancedb")));
            log_mgr.info("System", &format!("Project root directory: {:?}", root_dir));

            // 2. バックグラウンドでシステムリソースを 1 秒ごとにフロントエンドに emit
            let app_handle_res = app.handle().clone();
            std::thread::spawn(move || {
                let resource_mgr = ResourceManager::new();
                loop {
                    std::thread::sleep(Duration::from_millis(1000));
                    let res = resource_mgr.get_resources();
                    let _ = app_handle_res.emit("resource_status", res);
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    state.session_mgr.stop_session();
                }
            }
        });
}
