#![recursion_limit = "256"]

pub mod ai_client;
pub mod asr;
pub mod audio;
pub mod audio_input;
pub mod bootstrap;
pub mod lance_memory;
pub mod local_summary;
pub mod logger;
pub mod prompts;
pub mod resource;
pub mod session;
pub mod settings;
pub mod summary_failure;
pub mod tts;
pub mod twitch;
pub mod web_search;
pub mod window_capture;

pub mod memory_v2;
pub mod model_manager;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

use ai_client::{AiClient, AiGenerateOptions, ChatMessage};
use audio::AudioDevicesResponse;
use lance_memory::{MemoryItem, MemoryListResponse};
use logger::{LogEntry, LogManager};
use memory_v2::repository::MemoryRepository;
use model_manager::{ModelManager, ModelStatus};
use resource::{ResourceManager, SystemResources};
use session::{SessionEvent, SessionManager};
use settings::SkillsResponse;
use tts::{TtsManager, TtsSettings};
use twitch::{TwitchBotSettings, TwitchService};
use web_search::{WebSearchClient, WebSearchResponse};

pub struct AppState {
    pub(crate) root_dir: PathBuf,
    resource_mgr: ResourceManager,
    tts_mgr: Arc<TtsManager>,
    twitch_service: Arc<TwitchService>,
    web_search_client: Arc<WebSearchClient>,
    ai_client: Arc<AiClient>,
    session_mgr: Arc<SessionManager>,
    log_mgr: Arc<LogManager>,
    model_mgr: Arc<ModelManager>,
    migration_progress: lance_memory::MigrationProgressHandle,
    runtime_initialization: RuntimeInitializationState,
}

/// A live, in-process snapshot of the expensive engine work performed after
/// the portable runtime has passed first-run setup.  Keeping this state in the
/// native layer means the main screen can render truthful progress even while
/// model loading or a large memory-v2 journal recovery is still running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInitializationStage {
    pub id: String,
    pub label: String,
    pub status: String,
    pub progress: f64,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInitializationStatus {
    pub status: String,
    pub progress: f64,
    pub current_stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub stages: Vec<RuntimeInitializationStage>,
    pub asr_ready: bool,
    pub embedding_ready: bool,
    pub memory_v2_ready: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
struct RuntimeInitializationState {
    status: Arc<StdMutex<RuntimeInitializationStatus>>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl RuntimeInitializationState {
    fn new() -> Self {
        Self {
            status: Arc::new(StdMutex::new(RuntimeInitializationStatus::idle())),
            gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn snapshot(&self) -> RuntimeInitializationStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace(&self, status: RuntimeInitializationStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    }
}

impl RuntimeInitializationStatus {
    fn default_stages() -> Vec<RuntimeInitializationStage> {
        vec![
            RuntimeInitializationStage {
                id: "asr".to_string(),
                label: "ASR / Faster-Whisper".to_string(),
                status: "pending".to_string(),
                progress: 0.0,
                elapsed_ms: 0,
                error: None,
            },
            RuntimeInitializationStage {
                id: "embedding".to_string(),
                label: "GLuCoSE-base-ja".to_string(),
                status: "pending".to_string(),
                progress: 0.0,
                elapsed_ms: 0,
                error: None,
            },
            RuntimeInitializationStage {
                id: "memory_v2".to_string(),
                label: "memory-v2".to_string(),
                status: "pending".to_string(),
                progress: 0.0,
                elapsed_ms: 0,
                error: None,
            },
        ]
    }

    fn idle() -> Self {
        Self {
            status: "idle".to_string(),
            progress: 0.0,
            current_stage: None,
            message: Some("初期化を開始する準備をしています。".to_string()),
            elapsed_ms: 0,
            started_at: None,
            completed_at: None,
            stages: Self::default_stages(),
            asr_ready: false,
            embedding_ready: false,
            memory_v2_ready: false,
            error: None,
        }
    }
}

pub fn resolve_project_root() -> PathBuf {
    // Release builds are portable: all runtime data is rooted beside the EXE.
    // Debug builds retain project-root discovery for the checked-out development tree.
    bootstrap::resolve_runtime_root()
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
    state.log_mgr.info(
        "Capture",
        &format!("Capturing window preview for: '{}'", title),
    );
    let result = window_capture::capture_window_base64(&title);
    if result.is_some() {
        state
            .log_mgr
            .info("Capture", "Window preview captured successfully");
    } else {
        state.log_mgr.warn(
            "Capture",
            &format!("Failed to capture window preview for: '{}'", title),
        );
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
            let _ = state
                .session_mgr
                .asr_engine
                .ws_client
                .set_preallocate_vram(enable);
            state
                .log_mgr
                .info("System", &format!("VRAM Preallocation updated: {}", enable));
        }
    }
    if key == "gemma_terms_accepted" && value.as_bool() == Some(true) {
        // Keep the legacy boolean for compatibility, but only a complete
        // version/source/model-hash record authorizes Gemma setup and use.
        settings::save_setting_key(&state.root_dir, &key, Value::Bool(true))?;
        settings::save_setting_key(
            &state.root_dir,
            "gemma_terms_version",
            Value::String(model_manager::GEMMA_TERMS_VERSION.to_string()),
        )?;
        settings::save_setting_key(
            &state.root_dir,
            "gemma_terms_model_sha256",
            Value::String(model_manager::GEMMA_EXPECTED_SHA256.to_string()),
        )?;
        settings::save_setting_key(
            &state.root_dir,
            "gemma_terms_source",
            Value::String(model_manager::GEMMA_TERMS_SOURCE.to_string()),
        )?;
        bootstrap::clear_setup_error(&state.root_dir)?;
        Ok(settings::load_settings_file(&state.root_dir))
    } else {
        settings::save_setting_key(&state.root_dir, &key, value)
    }
}

#[tauri::command]
fn accept_gemma_terms(state: State<AppState>) -> Result<Value, String> {
    save_setting(state, "gemma_terms_accepted".to_string(), Value::Bool(true))
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
fn save_skill_content(
    state: State<AppState>,
    id: String,
    content: String,
) -> Result<SkillsResponse, String> {
    settings::save_skill_content(&state.root_dir, &id, &content)?;
    state.log_mgr.info(
        "Settings",
        &format!("Saved customized skill content: '{}'", id),
    );
    Ok(settings::scan_skills(&state.root_dir))
}

#[tauri::command]
fn get_prompts(state: State<AppState>) -> Vec<prompts::PromptItem> {
    prompts::get_all_prompts(&state.root_dir)
}

#[tauri::command]
fn save_prompt(
    state: State<AppState>,
    id: String,
    value: String,
) -> Result<Vec<prompts::PromptItem>, String> {
    prompts::save_prompt_value(&state.root_dir, &id, &value)?;
    state
        .log_mgr
        .info("Settings", &format!("Saved customized prompt: '{}'", id));
    Ok(prompts::get_all_prompts(&state.root_dir))
}

#[tauri::command]
fn reset_prompt(state: State<AppState>, id: String) -> Result<Vec<prompts::PromptItem>, String> {
    prompts::reset_prompt_value(&state.root_dir, &id)?;
    state
        .log_mgr
        .info("Settings", &format!("Reset prompt to default: '{}'", id));
    Ok(prompts::get_all_prompts(&state.root_dir))
}

#[tauri::command]
fn list_audio_devices() -> AudioDevicesResponse {
    audio::list_input_devices()
}

#[tauri::command]
async fn list_lance_memories(
    state: State<'_, AppState>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<MemoryListResponse, String> {
    lance_memory::list_memories_with_progress(
        &state.root_dir,
        limit,
        offset,
        Some(state.migration_progress.clone()),
    )
    .await
}

#[tauri::command]
fn get_lance_migration_status(state: State<AppState>) -> lance_memory::MemoryMigrationStatus {
    state.migration_progress.snapshot()
}

#[tauri::command]
async fn delete_lance_memory(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    lance_memory::delete_memory(&state.root_dir, &id).await
}

#[tauri::command]
async fn delete_lance_memories_bulk(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<usize, String> {
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
    state
        .log_mgr
        .info("LanceDB", &format!("Created LanceDB backup: {}", res));
    Ok(res)
}

#[tauri::command]
fn lance_list_backups(state: State<AppState>) -> Result<Vec<String>, String> {
    lance_memory::list_lance_backups(&state.root_dir)
}

#[tauri::command]
fn lance_restore(state: State<AppState>, backup_name: String) -> Result<(), String> {
    lance_memory::restore_lance_backup(&state.root_dir, &backup_name)?;
    state.log_mgr.info(
        "LanceDB",
        &format!("Restored LanceDB from backup: {}", backup_name),
    );
    Ok(())
}

#[tauri::command]
async fn lance_export_json(
    state: State<'_, AppState>,
    output_filename: Option<String>,
) -> Result<String, String> {
    let res = lance_memory::export_lance_memories_json(&state.root_dir, output_filename).await?;
    state.log_mgr.info("LanceDB", &res);
    Ok(res)
}

// --- TTS 音声合成 & 再生 ---
#[tauri::command]
async fn tts_speak(
    state: State<'_, AppState>,
    text: String,
    settings: Option<TtsSettings>,
) -> Result<(), String> {
    let tts_cfg = settings.unwrap_or_default();
    state.tts_mgr.speak(&text, &tts_cfg).await
}

#[tauri::command]
fn tts_stop(state: State<AppState>) {
    state.tts_mgr.stop_playback();
}

#[tauri::command]
async fn tts_play_nod(state: State<'_, AppState>) -> Result<(), String> {
    state
        .tts_mgr
        .play_random_nod(&state.root_dir)
        .await
        .map_err(|error| error.to_string())
}

// --- Twitch IRC 連携 ---
#[tauri::command]
async fn twitch_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: TwitchBotSettings,
) -> Result<(), String> {
    state
        .twitch_service
        .connect(settings, Some(app), None)
        .await
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
    let redir = redirect_uri
        .unwrap_or_else(|| "https://k0ta0uchi.github.io/GameAssistant/auth.html".to_string());
    state
        .twitch_service
        .exchange_code(&client_id, &client_secret, &code, &redir)
        .await
}

#[tauri::command]
fn twitch_get_auth_url(client_id: String, redirect_uri: Option<String>) -> String {
    let redir = redirect_uri
        .unwrap_or_else(|| "https://k0ta0uchi.github.io/GameAssistant/auth.html".to_string());
    TwitchService::get_auth_url(&client_id, &redir)
}

#[tauri::command]
async fn twitch_validate_token(
    state: State<'_, AppState>,
    access_token: String,
) -> Result<twitch::TwitchValidateResponse, String> {
    state.twitch_service.validate_token(&access_token).await
}

#[tauri::command]
async fn twitch_refresh_token(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
    refresh_token: String,
) -> Result<twitch::TwitchTokenResponse, String> {
    state
        .twitch_service
        .refresh_token(&client_id, &client_secret, &refresh_token)
        .await
}

// --- Web 検索 ---
#[tauri::command]
async fn web_search_query(
    state: State<'_, AppState>,
    query: String,
    brave_api_key: Option<String>,
) -> Result<WebSearchResponse, String> {
    let key = brave_api_key.unwrap_or_default();
    Ok(state
        .web_search_client
        .search_and_format(&query, &key)
        .await)
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
    let disable_thinking = st
        .get("disable_thinking_mode")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let thinking_budget = if disable_thinking { Some(0) } else { None };

    let options = AiGenerateOptions {
        system_instruction: system_prompt,
        temperature: Some(0.7),
        max_output_tokens: Some(1024),
        image_base64,
        thinking_budget,
    };
    state
        .ai_client
        .generate_gemini(&gemini_api_key, &model_name, &messages, &options)
        .await
}

// --- AI / セッション オーケストレーション ---
#[tauri::command]
async fn session_start(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let requested_at = Instant::now();
    let readiness_id = uuid::Uuid::new_v4().to_string();
    state.log_mgr.info(
        "Session",
        &format!(
            "session_start_requested phase=readiness_gate readiness_id={}",
            readiness_id
        ),
    );
    if let Err(error) = bootstrap::runtime_is_ready(&state.root_dir) {
        state.log_mgr.error(
            "Session",
            &format!(
                "session_start_failed phase=runtime_ready readiness_id={} error={}",
                readiness_id, error
            ),
        );
        return Err(error);
    }
    // Startup warmup normally makes this an immediate readiness check.  If a
    // user clicks while the background warmup is still loading Whisper, wait
    // here instead of attaching the microphone to a server that cannot yet
    // consume audio (which would otherwise create a large startup backlog).
    if let Err(error) = state.session_mgr.ensure_asr_ready().await {
        state.log_mgr.error(
            "Session",
            &format!(
                "session_start_failed phase=asr_ready readiness_id={} error={}",
                readiness_id, error
            ),
        );
        return Err(format!("ASR worker is not ready: {}", error));
    }
    state.log_mgr.info(
        "ASR",
        &format!(
            "asr_ready phase=readiness_gate readiness_id={} wait_ms={}",
            readiness_id,
            requested_at.elapsed().as_millis()
        ),
    );
    emit_asr_ready(&app, &state.session_mgr);
    state.session_mgr.start_session_with_request_id(
        Some(app),
        Some(state.twitch_service.clone()),
        readiness_id,
    );
    Ok(())
}

#[tauri::command]
fn session_stop(app: AppHandle, state: State<AppState>) {
    state
        .session_mgr
        .stop_session_with_services(Some(&state.twitch_service), Some(app));
}

// --- ASR ウォームアップ (GUI 表示時事前ロード) ---
#[tauri::command]
async fn warmup_asr(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    if let Err(error) = bootstrap::runtime_is_ready(&state.root_dir) {
        let message = format!("ASR warmup blocked: {}", error);
        state.log_mgr.error("ASR", &message);
        emit_asr_warmup_failed(&app, &message);
        return Err(message);
    }
    state.log_mgr.info(
        "ASR",
        "Warmup requested: Preloading Faster-Whisper CUDA INT8 server into VRAM...",
    );

    match state.session_mgr.ensure_asr_ready().await {
        Ok(()) => {
            state.log_mgr.info(
                "ASR",
                "Faster-Whisper CUDA INT8 warmup complete! Ready for instant transcription.",
            );
            emit_asr_ready(&app, &state.session_mgr);
            Ok("Warmup completed for Faster-Whisper CUDA INT8".to_string())
        }
        Err(error) => {
            state
                .log_mgr
                .error("ASR", &format!("Warmup failed: {}", error));
            emit_asr_warmup_failed(&app, &error);
            Err(error)
        }
    }
}

#[tauri::command]
async fn restart_whisper(state: State<'_, AppState>) -> Result<String, String> {
    bootstrap::runtime_is_ready(&state.root_dir)
        .map_err(|error| format!("Whisper restart blocked: {}", error))?;
    state
        .log_mgr
        .info("ASR", "Restarting Whisper GPU worker...");
    state.session_mgr.asr_engine.ws_client.restart().await?;
    state.log_mgr.info(
        "ASR",
        "Whisper GPU worker restarted and warmed up successfully.",
    );
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
    state.log_mgr.info(
        "Model",
        &format!("Starting download for model: {}", model_id),
    );
    let res = model_mgr
        .download_model(app, root_dir, model_id.clone(), custom_dir)
        .await;
    if let Err(ref e) = res {
        state
            .log_mgr
            .error("Model", &format!("Failed to download {}: {}", model_id, e));
    } else {
        state.log_mgr.info(
            "Model",
            &format!("Successfully downloaded model: {}", model_id),
        );
    }
    res
}

#[tauri::command]
fn cancel_download_model(state: State<AppState>, model_id: String) -> bool {
    state.log_mgr.info(
        "Model",
        &format!("Cancelled download for model: {}", model_id),
    );
    state.model_mgr.cancel_download(&model_id)
}

// --- Portable runtime bootstrap ---
#[tauri::command]
fn get_runtime_status(state: State<AppState>) -> Result<bootstrap::RuntimeStatus, String> {
    let mut status = bootstrap::runtime_status(&state.root_dir)?;
    status.asr_websocket_ready = state.session_mgr.asr_engine.ws_client.is_ready();
    Ok(status)
}

#[tauri::command]
fn get_setup_status(state: State<AppState>) -> Result<bootstrap::RuntimeStatus, String> {
    let mut status = bootstrap::runtime_status(&state.root_dir)?;
    status.asr_websocket_ready = state.session_mgr.asr_engine.ws_client.is_ready();
    Ok(status)
}

fn publish_runtime_initialization(
    app: &AppHandle,
    state: &RuntimeInitializationState,
    status: &RuntimeInitializationStatus,
) {
    state.replace(status.clone());
    let _ = app.emit("runtime_initialization", status);
}

fn set_runtime_initialization_stage(
    status: &mut RuntimeInitializationStatus,
    id: &str,
    stage_status: &str,
    progress: f64,
    elapsed_ms: u64,
    error: Option<String>,
) {
    if let Some(stage) = status.stages.iter_mut().find(|stage| stage.id == id) {
        stage.status = stage_status.to_string();
        stage.progress = progress.clamp(0.0, 100.0);
        stage.elapsed_ms = elapsed_ms;
        stage.error = error;
    }
}

/// Keep the aggregate gauge meaningful when independent stages finish in a
/// different order.  The weights match the three equal-cost startup lanes
/// closely enough while still reserving a small extra point for ASR's model
/// handshake.
fn update_runtime_initialization_progress(status: &mut RuntimeInitializationStatus) {
    let mut progress = 0.0;
    if status
        .stages
        .iter()
        .any(|stage| stage.id == "asr" && stage.status == "completed")
    {
        progress += 34.0;
    }
    if status
        .stages
        .iter()
        .any(|stage| stage.id == "embedding" && stage.status == "completed")
    {
        progress += 33.0;
    }
    if status
        .stages
        .iter()
        .any(|stage| stage.id == "memory_v2" && stage.status == "completed")
    {
        progress += 33.0;
    }
    status.progress = progress;
}

fn join_runtime_initialization_task(
    result: Result<Result<(), String>, tauri::Error>,
    stage: &str,
) -> Result<(), String> {
    match result {
        Ok(result) => result,
        Err(error) => Err(format!("{} initialization task failed: {}", stage, error)),
    }
}

#[tauri::command]
fn get_runtime_initialization_status(state: State<AppState>) -> RuntimeInitializationStatus {
    state.runtime_initialization.snapshot()
}

/// Initialize the live engines from the main screen in one measured,
/// concurrent startup lanes.  The portable setup command only verifies/downloads
/// files; this command performs the work that used to be paid on the first
/// utterance (Whisper, GLuCoSE and memory-v2 journal recovery).
#[tauri::command]
async fn initialize_runtime(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RuntimeInitializationStatus, String> {
    let init_state = state.runtime_initialization.clone();
    let _gate = init_state.gate.lock().await;
    let existing = init_state.snapshot();
    if existing.status == "completed"
        && existing.asr_ready
        && existing.embedding_ready
        && existing.memory_v2_ready
    {
        return Ok(existing);
    }

    if let Err(error) = bootstrap::runtime_is_ready(&state.root_dir) {
        let mut failed = existing;
        failed.status = "error".to_string();
        failed.message = Some("ランタイムセットアップが完了していません。".to_string());
        failed.error = Some(error.clone());
        failed.current_stage = None;
        failed.elapsed_ms = 0;
        publish_runtime_initialization(&app, &init_state, &failed);
        return Err(error);
    }

    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut status = RuntimeInitializationStatus::idle();
    status.status = "running".to_string();
    status.message = Some("ASR、GLuCoSE、memory-v2を初期化しています。".to_string());
    status.started_at = Some(started_at);
    status.completed_at = None;
    status.error = None;
    publish_runtime_initialization(&app, &init_state, &status);
    state.log_mgr.info(
        "Bootstrap",
        "Runtime initialization started from the main screen",
    );

    // ASR and memory-v2 have independent resources, so start both lanes at
    // once.  GLuCoSE remains deliberately downstream of ASR because the
    // current embedding command is carried by that worker's WebSocket.
    status.current_stage = Some("asr".to_string());
    status.message = Some("ASR と memory-v2を並列で初期化しています。".to_string());
    set_runtime_initialization_stage(&mut status, "asr", "running", 0.0, 0, None);
    set_runtime_initialization_stage(&mut status, "memory_v2", "running", 0.0, 0, None);
    publish_runtime_initialization(&app, &init_state, &status);

    let asr_started = Instant::now();
    let memory_started = Instant::now();
    let asr_session_mgr = state.session_mgr.clone();
    let asr_task =
        tauri::async_runtime::spawn(async move { asr_session_mgr.ensure_asr_ready().await });
    let memory_root = state.root_dir.clone();
    let memory_task = tauri::async_runtime::spawn(async move {
        MemoryRepository::open(memory_root).await.map(|_| ())
    });
    let mut asr_task = Box::pin(asr_task);
    let mut memory_task = Box::pin(memory_task);
    let mut memory_finished = false;
    let mut memory_error: Option<String> = None;

    // Wait for ASR while still consuming memory-v2 completion immediately so
    // the panel reflects whichever independent lane finishes first.
    let asr_result = loop {
        tokio::select! {
            result = &mut asr_task => {
                break join_runtime_initialization_task(result, "ASR");
            }
            result = &mut memory_task, if !memory_finished => {
                memory_finished = true;
                match join_runtime_initialization_task(result, "memory-v2") {
                    Ok(()) => {
                        status.memory_v2_ready = true;
                        set_runtime_initialization_stage(
                            &mut status,
                            "memory_v2",
                            "completed",
                            100.0,
                            memory_started.elapsed().as_millis() as u64,
                            None,
                        );
                    }
                    Err(error) => {
                        memory_error = Some(error.clone());
                        set_runtime_initialization_stage(
                            &mut status,
                            "memory_v2",
                            "error",
                            0.0,
                            memory_started.elapsed().as_millis() as u64,
                            Some(error.clone()),
                        );
                        status.message = Some("memory-v2の初期化に失敗しました。".to_string());
                        status.error = Some(error);
                    }
                }
                update_runtime_initialization_progress(&mut status);
                publish_runtime_initialization(&app, &init_state, &status);
            }
        }
    };

    if let Err(error) = asr_result {
        set_runtime_initialization_stage(
            &mut status,
            "asr",
            "error",
            0.0,
            asr_started.elapsed().as_millis() as u64,
            Some(error.clone()),
        );
        status.message = Some("ASRの初期化に失敗しました。".to_string());
        status.error = Some(error.clone());
        status.status = "error".to_string();
        status.current_stage = Some("asr".to_string());
        status.elapsed_ms = started.elapsed().as_millis() as u64;
        publish_runtime_initialization(&app, &init_state, &status);

        // Do not cancel a database recovery half-way through a LanceDB
        // operation.  Let the independent lane reach its safe boundary before
        // returning the ASR error.
        if !memory_finished {
            match join_runtime_initialization_task((&mut memory_task).await, "memory-v2") {
                Ok(()) => {
                    status.memory_v2_ready = true;
                    set_runtime_initialization_stage(
                        &mut status,
                        "memory_v2",
                        "completed",
                        100.0,
                        memory_started.elapsed().as_millis() as u64,
                        None,
                    );
                }
                Err(memory_failure) => {
                    set_runtime_initialization_stage(
                        &mut status,
                        "memory_v2",
                        "error",
                        0.0,
                        memory_started.elapsed().as_millis() as u64,
                        Some(memory_failure),
                    );
                }
            }
            update_runtime_initialization_progress(&mut status);
            publish_runtime_initialization(&app, &init_state, &status);
        }
        return Err(error);
    }

    status.asr_ready = true;
    set_runtime_initialization_stage(
        &mut status,
        "asr",
        "completed",
        100.0,
        asr_started.elapsed().as_millis() as u64,
        None,
    );
    update_runtime_initialization_progress(&mut status);
    status.current_stage = Some("embedding".to_string());
    status.message = Some("ASR接続完了。GLuCoSE-base-jaを読み込んでいます。".to_string());
    publish_runtime_initialization(&app, &init_state, &status);
    state.log_mgr.info(
        "Bootstrap",
        &format!(
            "Runtime initialization stage completed: stage=asr elapsed_ms={}",
            status
                .stages
                .iter()
                .find(|stage| stage.id == "asr")
                .map(|stage| stage.elapsed_ms)
                .unwrap_or(0)
        ),
    );
    emit_asr_ready(&app, &state.session_mgr);

    // GLuCoSE uses the already-connected ASR worker.  It runs concurrently
    // with any memory-v2 recovery that is still in flight.
    set_runtime_initialization_stage(&mut status, "embedding", "running", 0.0, 0, None);
    publish_runtime_initialization(&app, &init_state, &status);
    let embedding_started = Instant::now();
    let probe = vec!["GameAssistant 起動時の埋め込み初期化確認".to_string()];
    let ws_client = state.session_mgr.asr_engine.ws_client.clone();
    let mut embedding_task = Box::pin(async move {
        let vectors = ws_client
            .embed_texts_with_timeout(&probe, Duration::from_secs(60))
            .await?;
        if vectors.len() == 1 && vectors[0].len() == memory_v2::validation::EMBEDDING_DIMENSIONS {
            Ok(())
        } else {
            Err(format!(
                "GLuCoSEの埋め込み結果が不正です（{}件、{}次元）。",
                vectors.len(),
                vectors.first().map(|vector| vector.len()).unwrap_or(0)
            ))
        }
    });
    let mut embedding_finished = false;
    let mut embedding_error: Option<String> = None;

    while !embedding_finished || !memory_finished {
        tokio::select! {
            result = &mut embedding_task, if !embedding_finished => {
                embedding_finished = true;
                match result {
                    Ok(()) => {
                        status.embedding_ready = true;
                        set_runtime_initialization_stage(
                            &mut status,
                            "embedding",
                            "completed",
                            100.0,
                            embedding_started.elapsed().as_millis() as u64,
                            None,
                        );
                    }
                    Err(error) => {
                        embedding_error = Some(error.clone());
                        set_runtime_initialization_stage(
                            &mut status,
                            "embedding",
                            "error",
                            0.0,
                            embedding_started.elapsed().as_millis() as u64,
                            Some(error.clone()),
                        );
                        status.message = Some("GLuCoSEの初期化に失敗しました。".to_string());
                        status.error = Some(error);
                    }
                }
                update_runtime_initialization_progress(&mut status);
                publish_runtime_initialization(&app, &init_state, &status);
            }
            result = &mut memory_task, if !memory_finished => {
                memory_finished = true;
                match join_runtime_initialization_task(result, "memory-v2") {
                    Ok(()) => {
                        status.memory_v2_ready = true;
                        set_runtime_initialization_stage(
                            &mut status,
                            "memory_v2",
                            "completed",
                            100.0,
                            memory_started.elapsed().as_millis() as u64,
                            None,
                        );
                    }
                    Err(error) => {
                        memory_error = Some(error.clone());
                        set_runtime_initialization_stage(
                            &mut status,
                            "memory_v2",
                            "error",
                            0.0,
                            memory_started.elapsed().as_millis() as u64,
                            Some(error.clone()),
                        );
                        status.message = Some("memory-v2の初期化に失敗しました。".to_string());
                        status.error = Some(error);
                    }
                }
                update_runtime_initialization_progress(&mut status);
                publish_runtime_initialization(&app, &init_state, &status);
            }
        }
    }

    if let Some(error) = embedding_error.or(memory_error) {
        status.status = "error".to_string();
        status.current_stage = if status.embedding_ready {
            Some("memory_v2".to_string())
        } else {
            Some("embedding".to_string())
        };
        status.message = Some("ローカルエンジンの初期化に失敗しました。".to_string());
        status.error = Some(error.clone());
        status.elapsed_ms = started.elapsed().as_millis() as u64;
        publish_runtime_initialization(&app, &init_state, &status);
        return Err(error);
    }

    status.progress = 100.0;
    status.status = "completed".to_string();
    status.current_stage = Some("complete".to_string());
    status.message = Some("ASR、GLuCoSE、memory-v2の初期化が完了しました。".to_string());
    status.error = None;
    status.elapsed_ms = started.elapsed().as_millis() as u64;
    status.completed_at = Some(chrono::Utc::now().to_rfc3339());
    publish_runtime_initialization(&app, &init_state, &status);
    state.log_mgr.info(
        "Bootstrap",
        &format!(
            "Runtime initialization stage completed: stage=memory_v2 elapsed_ms={}",
            status
                .stages
                .iter()
                .find(|stage| stage.id == "memory_v2")
                .map(|stage| stage.elapsed_ms)
                .unwrap_or(0)
        ),
    );
    state.log_mgr.info(
        "Bootstrap",
        &format!(
            "Runtime initialization complete: elapsed_ms={} asr_ms={} embedding_ms={} memory_v2_ms={}",
            status.elapsed_ms,
            status.stages.iter().find(|stage| stage.id == "asr").map(|stage| stage.elapsed_ms).unwrap_or(0),
            status.stages.iter().find(|stage| stage.id == "embedding").map(|stage| stage.elapsed_ms).unwrap_or(0),
            status.stages.iter().find(|stage| stage.id == "memory_v2").map(|stage| stage.elapsed_ms).unwrap_or(0),
        ),
    );
    Ok(status)
}

#[tauri::command]
async fn setup_runtime(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<bootstrap::RuntimeStatus, String> {
    let root_dir = state.root_dir.clone();
    let model_mgr = state.model_mgr.clone();
    bootstrap::run_setup(&root_dir, Some(model_mgr), Some(app)).await
}

#[tauri::command]
async fn run_setup(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<bootstrap::RuntimeStatus, String> {
    let root_dir = state.root_dir.clone();
    let model_mgr = state.model_mgr.clone();
    bootstrap::run_setup(&root_dir, Some(model_mgr), Some(app)).await
}

#[tauri::command]
fn cancel_runtime_setup(state: State<AppState>) {
    bootstrap::cancel_setup();
    for model in model_manager::get_defined_models()
        .into_iter()
        .filter(|model| model.required)
    {
        state.model_mgr.cancel_download(&model.id);
    }
}

#[tauri::command]
fn cancel_setup(state: State<AppState>) {
    bootstrap::cancel_setup();
    for model in model_manager::get_defined_models()
        .into_iter()
        .filter(|model| model.required)
    {
        state.model_mgr.cancel_download(&model.id);
    }
}

#[tauri::command]
fn request_setup_elevation() -> Result<bootstrap::ElevationRequestResult, String> {
    bootstrap::relaunch_setup_elevated()
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

#[tauri::command]
async fn unload_local_summary_model(state: State<'_, AppState>) -> Result<(), String> {
    state.session_mgr.unload_local_summary_model().await;
    Ok(())
}

/// Return the local-summary runtime state using the camelCase payload consumed
/// by the Settings UI.  Status is deliberately a successful response even
/// when the optional model is missing or the last inference failed: those are
/// actionable runtime states, not IPC failures.
#[tauri::command]
async fn get_local_summary_status(
    state: State<'_, AppState>,
) -> Result<local_summary::SummaryRuntimeStatus, String> {
    Ok(state.session_mgr.local_summary().status().await)
}

/// Run the Settings UI's fixed local-summary probe.  This path only exercises
/// the summary runtime and never persists the returned decision to memory.
#[tauri::command]
async fn test_local_summary(
    state: State<'_, AppState>,
    text: String,
) -> Result<local_summary::SummaryDecision, String> {
    state.session_mgr.local_summary().test_summary(&text).await
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

/// Take one non-blocking LanceDB snapshot for each application launch.  The
/// copy is deliberately detached from `session_start`: a large database must
/// never delay microphone activation or compete with the live ASR callback.
fn spawn_startup_lance_backup(root_dir: PathBuf, log_mgr: Arc<LogManager>) {
    tauri::async_runtime::spawn(async move {
        let backup_root = root_dir.clone();
        let result =
            tokio::task::spawn_blocking(move || lance_memory::backup_lance_db(&backup_root)).await;

        match result {
            Ok(Ok(backup_name)) => log_mgr.info(
                "LanceDB",
                &format!("Auto-backup created at app startup: {}", backup_name),
            ),
            Ok(Err(error)) => log_mgr.warn(
                "LanceDB",
                &format!("Startup auto-backup skipped: {}", error),
            ),
            Err(error) => log_mgr.warn(
                "LanceDB",
                &format!("Startup auto-backup worker failed: {}", error),
            ),
        }
    });
}

fn emit_asr_ready(app: &AppHandle, session_mgr: &SessionManager) {
    // This event is intentionally guarded by the client's readiness bit.  A
    // spawned Python child or a second probe connection is not sufficient;
    // the client bit flips only after the exact audio WebSocket is attached.
    if !session_mgr.asr_engine.ws_client.is_ready() {
        return;
    }
    let _ = app.emit(
        "asr_ready",
        serde_json::json!({
            "ready": true,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    );
}

fn emit_asr_warmup_failed(app: &AppHandle, error: &str) {
    let _ = app.emit(
        "asr_warmup_failed",
        serde_json::json!({
            "message": error,
        }),
    );
}

pub fn run() {
    let root_dir = resolve_project_root();
    // Several legacy helpers (for example the app log and nod WAV lookup) use
    // relative paths.  Normalize the process working directory once so those
    // paths stay beside the portable EXE even when it is launched from a
    // desktop shortcut or another working directory.
    if let Err(error) = std::env::set_current_dir(&root_dir) {
        eprintln!(
            "[Bootstrap] unable to set portable working directory {:?}: {}",
            root_dir, error
        );
    }
    load_dotenv(&root_dir);

    let log_mgr = Arc::new(LogManager::new(root_dir.clone()));
    logger::set_global_logger(log_mgr.clone());

    let tts_mgr = Arc::new(TtsManager::new());
    let twitch_service = Arc::new(TwitchService::new());
    let web_search_client = Arc::new(WebSearchClient::new());
    let ai_client = Arc::new(AiClient::new());
    let session_mgr = Arc::new(SessionManager::new(
        root_dir.clone(),
        tts_mgr.clone(),
        log_mgr.clone(),
    ));
    let model_mgr = Arc::new(ModelManager::new());
    let migration_progress = Arc::new(lance_memory::MemoryMigrationProgress::default());

    let app_state = AppState {
        root_dir: root_dir.clone(),
        resource_mgr: ResourceManager::new(),
        tts_mgr,
        twitch_service,
        web_search_client,
        ai_client,
        session_mgr: session_mgr.clone(),
        log_mgr: log_mgr.clone(),
        model_mgr: model_mgr.clone(),
        migration_progress,
        runtime_initialization: RuntimeInitializationState::new(),
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
            accept_gemma_terms,
            list_skills,
            get_skill_content,
            save_skill_content,
            get_prompts,
            save_prompt,
            reset_prompt,
            list_audio_devices,
            list_lance_memories,
            get_lance_migration_status,
            delete_lance_memory,
            delete_lance_memories_bulk,
            import_memories_to_lance,
            memory_v2::api::memory_manager_list_raw,
            memory_v2::api::memory_manager_list_facts,
            memory_v2::api::memory_manager_list_summaries,
            memory_v2::api::memory_manager_get_fact_evidence,
            memory_v2::api::memory_manager_get_raw_event,
            memory_v2::api::memory_manager_get_fact_conflict,
            memory_v2::api::memory_manager_confirm_facts,
            memory_v2::api::memory_manager_edit_fact,
            memory_v2::api::memory_manager_edit_facts_bulk,
            memory_v2::api::memory_manager_delete_facts,
            memory_v2::api::memory_manager_undo,
            memory_v2::api::memory_manager_retry_summary,
            memory_v2::api::memory_manager_process_all,
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
            get_runtime_status,
            get_runtime_initialization_status,
            initialize_runtime,
            setup_runtime,
            cancel_runtime_setup,
            get_setup_status,
            run_setup,
            cancel_setup,
            request_setup_elevation,
            get_app_logs,
            clear_app_logs,
            unload_local_summary_model,
            get_local_summary_status,
            test_local_summary,
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            log_mgr.set_app_handle(app_handle.clone());
            session_mgr
                .local_summary()
                .set_status_emitter(app_handle.clone());

            // 1. Rust ネイティブエンジンの起動通知ログ
            log_mgr.info(
                "RustNative",
                "Pure Rust Native Core & LanceDB Engine Online",
            );
            log_mgr.info(
                "LanceDB",
                &format!(
                    "Database path initialized at: {:?}",
                    root_dir.join("data/lancedb")
                ),
            );
            log_mgr.info("System", &format!("Project root directory: {:?}", root_dir));

            // Database snapshots belong to application startup, not to the
            // Start Session button. Run the potentially large copy on a
            // blocking worker so the window and audio controls stay
            // responsive while it is created.
            spawn_startup_lance_backup(root_dir.clone(), log_mgr.clone());

            // Extract embedded scripts/uv immediately, then continue long-running
            // Python/dependency/model setup in the background so the UI can poll
            // get_runtime_status and show progress/retry controls.
            log_mgr.info("Bootstrap", "prepare_runtime starting");
            match bootstrap::prepare_runtime(&root_dir) {
                Err(error) => {
                    log_mgr.error("Bootstrap", &error);
                }
                Ok(status) => {
                    log_mgr.info(
                        "Bootstrap",
                        &format!(
                            "prepare_runtime complete: ready={} setup_required={} stage={:?}",
                            status.ready, status.setup_required, status.current_stage
                        ),
                    );
                    // Do not re-enter the setup worker once the portable runtime
                    // is already healthy.  Re-running it on every launch creates
                    // a transient stage transition while the main UI initializes.
                    if status.ready {
                        log_mgr.info(
                            "Bootstrap",
                            "runtime already ready; waiting for main-screen engine initialization",
                        );
                    } else if model_manager::gemma_terms_accepted(&root_dir) {
                        let setup_root = root_dir.clone();
                        let setup_manager = model_mgr.clone();
                        let setup_app = app.handle().clone();
                        let setup_log = log_mgr.clone();
                        let elevated_setup = std::env::args().any(|arg| arg == "--elevated-setup");
                        if elevated_setup {
                            // The elevated process is a setup worker only.  Keep the
                            // original standard-user window visible and avoid showing
                            // a duplicate UI while the UAC-approved worker runs.
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.hide();
                            }
                        }
                        tauri::async_runtime::spawn(async move {
                            setup_log.info("Bootstrap", "setup worker starting");
                            let setup_result = bootstrap::run_setup(
                                &setup_root,
                                Some(setup_manager),
                                Some(setup_app.clone()),
                            )
                            .await;
                            if let Err(error) = setup_result {
                                setup_log.error("Bootstrap", &error);
                                if elevated_setup {
                                    setup_app.exit(1);
                                }
                            } else {
                                setup_log.info("Bootstrap", "setup worker completed");
                                if elevated_setup {
                                    // The elevated helper is only a setup worker; return to
                                    // the original standard-user process once it completes.
                                    setup_app.exit(0);
                                }
                            }
                        });
                    } else {
                        // Terms are a mandatory user acknowledgement. Keep the first
                        // launch screen actionable without running a setup attempt that
                        // would only persist a misleading error state.
                        log_mgr.info(
                            "Bootstrap",
                            "Waiting for Gemma Terms acknowledgement before starting setup",
                        );
                    }
                }
            }

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
                    state.session_mgr.shutdown();
                    tauri::async_runtime::block_on(state.session_mgr.unload_local_summary_model());
                }
            }
        });
}
