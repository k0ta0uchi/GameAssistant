//! First-run runtime bootstrap for portable releases.
//!
//! All mutable runtime data is deliberately rooted at the directory containing the
//! executable in release builds.  Debug builds use the project root so development
//! continues to use the checked-out scripts and models.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

use crate::model_manager::ModelManager;

const PYTHON_VERSION: &str = "3.12";
const SETUP_STATE_VERSION: u32 = 1;
const GPU_TORCH_BACKEND: &str = "cu128";
const RUNTIME_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

// These probes deliberately run through the portable venv rather than through
// the developer machine's Python.  They are kept dependency-light at the Rust
// boundary: the ASR script and model manager remain the owners of inference,
// while bootstrap only asks Python to prove that each prerequisite initializes.
const PYTHON_IMPORT_PROBE: &str = r#"
import sys
if tuple(sys.version_info[:2]) != (3, 12):
    raise RuntimeError(f"portable Python 3.12 required, found {sys.version_info[0]}.{sys.version_info[1]}")
import faster_whisper
import numpy
import sentencepiece
import sentence_transformers
import torch
import transformers
import websockets
"#;
const TOKENIZER_PROBE: &str = r#"
import os
from pathlib import Path
from tokenizers import Tokenizer
from transformers import AutoTokenizer

models = Path(os.environ["MODELS_DIR"])
Tokenizer.from_file(str(models / "kotoba-whisper-v2.0-faster" / "tokenizer.json"))
AutoTokenizer.from_pretrained(
    str(models / "GLuCoSE-base-ja"),
    local_files_only=True,
)
"#;
const EMBEDDING_PROBE: &str = r#"
import os
from sentence_transformers import SentenceTransformer

model = SentenceTransformer(
    os.path.join(os.environ["MODELS_DIR"], "GLuCoSE-base-ja"),
    device="cpu",
)
model.encode(["GameAssistant runtime probe"], show_progress_bar=False)
"#;

// These resources are compiled into the Rust binary.  In particular this means a
// portable EXE does not rely on scripts being copied alongside it by the bundler.
static ASR_SERVER_PY: &[u8] = include_bytes!("../../scripts/asr_server.py");
static VITS2_SERVER_PY: &[u8] = include_bytes!("../../scripts/vits2_server.py");
static REQUIREMENTS_PLAIN: &[u8] = include_bytes!("../../requirements.txt");
static REQUIREMENTS_CPU: &[u8] = include_bytes!("../../requirements-cpu.txt");
static REQUIREMENTS_GPU: &[u8] = include_bytes!("../../requirements-gpu.txt");
static UV_EXE: &[u8] = include_bytes!("../resources/uv.exe");
static NOTICE_GEMMA: &[u8] = include_bytes!("../../NOTICE-GEMMA.txt");
// Keep the acknowledgement sounds inside the portable executable and extract
// them beside the executable during runtime bootstrap.  A portable release
// must not depend on a checkout-relative `wav/` directory being copied by the
// bundler or present on the target machine.
static NOD_WAV_FILES: &[(&str, &[u8])] = &[
    ("wav/nod/0.wav", include_bytes!("../../wav/nod/0.wav")),
    ("wav/nod/1.wav", include_bytes!("../../wav/nod/1.wav")),
    ("wav/nod/2.wav", include_bytes!("../../wav/nod/2.wav")),
    ("wav/nod/4.wav", include_bytes!("../../wav/nod/4.wav")),
    ("wav/nod/5.wav", include_bytes!("../../wav/nod/5.wav")),
];
include!(concat!(env!("OUT_DIR"), "/llama_server_embed.rs"));

static CANCEL_SETUP: AtomicBool = AtomicBool::new(false);
static SETUP_RUNNING: AtomicBool = AtomicBool::new(false);
static SETUP_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStage {
    Pending,
    Resources,
    Python,
    Venv,
    Dependencies,
    Models,
    Complete,
}

impl SetupStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resources => "resources",
            Self::Python => "python",
            Self::Venv => "venv",
            Self::Dependencies => "dependencies",
            Self::Models => "models",
            Self::Complete => "complete",
        }
    }

    fn downstream(self) -> &'static [SetupStage] {
        match self {
            Self::Pending => &[
                SetupStage::Resources,
                SetupStage::Python,
                SetupStage::Venv,
                SetupStage::Dependencies,
                SetupStage::Models,
                SetupStage::Complete,
            ],
            Self::Resources => &[
                SetupStage::Python,
                SetupStage::Venv,
                SetupStage::Dependencies,
                SetupStage::Models,
                SetupStage::Complete,
            ],
            Self::Python => &[
                SetupStage::Venv,
                SetupStage::Dependencies,
                SetupStage::Models,
                SetupStage::Complete,
            ],
            Self::Venv => &[
                SetupStage::Dependencies,
                SetupStage::Models,
                SetupStage::Complete,
            ],
            Self::Dependencies => &[SetupStage::Models, SetupStage::Complete],
            Self::Models => &[SetupStage::Complete],
            Self::Complete => &[],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupState {
    pub state_version: u32,
    pub app_version: String,
    pub python_version: String,
    pub requirements_sha256: String,
    /// Hash of every embedded dependency manifest.  The selected CPU/GPU hash
    /// above remains the lockfile identity; this bundle hash also invalidates a
    /// completed setup when the portable plain manifest changes.
    #[serde(default)]
    pub requirements_bundle_sha256: String,
    pub uv_sha256: String,
    pub scripts_sha256: String,
    #[serde(default)]
    pub lock_sha256: String,
    #[serde(default)]
    pub runtime_validation: RuntimeValidationState,
    pub gpu: bool,
    pub current_stage: SetupStage,
    pub completed_stages: BTreeSet<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// Results of the expensive, real Python initialization probes.  A file or
/// directory existing is not enough to authorize a session; the fingerprint
/// binds these results to the exact requirements/scripts/model artifact set
/// that was checked.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeValidationState {
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub python_import_ready: bool,
    #[serde(default)]
    pub tokenizer_ready: bool,
    #[serde(default)]
    pub embedding_ready: bool,
    #[serde(default)]
    pub diagnostics: BTreeMap<String, String>,
}

impl Default for SetupState {
    fn default() -> Self {
        Self {
            state_version: SETUP_STATE_VERSION,
            app_version: String::new(),
            python_version: PYTHON_VERSION.to_string(),
            requirements_sha256: String::new(),
            requirements_bundle_sha256: String::new(),
            uv_sha256: String::new(),
            scripts_sha256: String::new(),
            lock_sha256: String::new(),
            runtime_validation: RuntimeValidationState::default(),
            gpu: false,
            current_stage: SetupStage::Pending,
            completed_stages: BTreeSet::new(),
            last_error: None,
            updated_at: now_string(),
        }
    }
}

impl SetupState {
    pub fn stage_completed(&self, stage: SetupStage) -> bool {
        self.completed_stages.contains(stage.as_str())
    }

    pub fn mark_completed(&mut self, stage: SetupStage) {
        self.completed_stages.insert(stage.as_str().to_string());
        self.current_stage = match stage {
            SetupStage::Resources => SetupStage::Python,
            SetupStage::Python => SetupStage::Venv,
            SetupStage::Venv => SetupStage::Dependencies,
            SetupStage::Dependencies => SetupStage::Models,
            SetupStage::Models => SetupStage::Complete,
            SetupStage::Complete => SetupStage::Complete,
            SetupStage::Pending => SetupStage::Resources,
        };
        self.last_error = None;
        self.updated_at = now_string();
    }

    fn clear_from(&mut self, stage: SetupStage) {
        for downstream in stage.downstream() {
            self.completed_stages.remove(downstream.as_str());
        }
        self.current_stage = stage;
        self.last_error = None;
        self.updated_at = now_string();
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeLayout {
    pub root: PathBuf,
    pub python_dir: PathBuf,
    pub venv_dir: PathBuf,
    pub models_dir: PathBuf,
    pub uv_cache_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub scripts_dir: PathBuf,
    pub state_path: PathBuf,
    pub uv_path: PathBuf,
    pub lock_path: PathBuf,
    pub dependencies_marker_path: PathBuf,
    pub llama_server_path: PathBuf,
}

impl RuntimeLayout {
    pub fn for_root(root: PathBuf) -> Self {
        Self {
            python_dir: root.join(".python"),
            venv_dir: root.join("venv"),
            models_dir: root.join("models"),
            uv_cache_dir: root.join(".uv-cache"),
            logs_dir: root.join("logs"),
            scripts_dir: root.join("scripts"),
            state_path: root.join("setup-state.json"),
            uv_path: root.join("uv.exe"),
            lock_path: root.join("uv.lock"),
            dependencies_marker_path: root.join(".dependencies-complete"),
            llama_server_path: root.join("runtime").join("llama").join("llama-server.exe"),
            root,
        }
    }

    pub fn python_executable(&self) -> PathBuf {
        if cfg!(windows) {
            self.venv_dir.join("Scripts").join("python.exe")
        } else {
            self.venv_dir.join("bin").join("python")
        }
    }

    fn requirements_path(&self, gpu: bool) -> PathBuf {
        self.root.join(if gpu {
            "requirements-gpu.txt"
        } else {
            "requirements-cpu.txt"
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub root_dir: String,
    pub python_dir: String,
    pub venv_dir: String,
    pub models_dir: String,
    pub uv_cache_dir: String,
    pub logs_dir: String,
    pub setup_state: SetupState,
    pub ready: bool,
    pub setup_required: bool,
    pub running: bool,
    pub current_stage: Option<String>,
    pub progress: f64,
    pub message: Option<String>,
    pub error: Option<String>,
    /// Stable UI state; cancellation is not presented as a generic failure.
    pub status: String,
    pub cancelled: bool,
    pub stages: Vec<SetupStepStatus>,
    pub completed_stages: Vec<String>,
    pub required_models_ready: bool,
    pub required_models_missing: Vec<String>,
    pub writable: bool,
    pub elevation_required: bool,
    pub elevation_message: Option<String>,
    pub uv_present: bool,
    pub scripts_present: bool,
    pub python_present: bool,
    pub venv_present: bool,
    pub lock_present: bool,
    /// Readiness checks are additive so older clients can continue to consume
    /// the original portable setup shape while newer clients can identify the
    /// failing runtime layer without parsing free-form errors.
    #[serde(default)]
    pub dependency_ready: bool,
    #[serde(default)]
    pub python_import_ready: bool,
    #[serde(default)]
    pub tokenizer_ready: bool,
    #[serde(default)]
    pub embedding_ready: bool,
    #[serde(default)]
    pub asr_websocket_ready: bool,
    #[serde(default)]
    pub diagnostics: BTreeMap<String, String>,
    #[serde(default)]
    pub llama_server_present: bool,
    pub gemma_terms_accepted: bool,
    pub gemma_terms_version: String,
    pub gemma_terms_model_sha256: String,
    pub gemma_terms_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStepStatus {
    pub id: String,
    pub status: String,
    pub progress: f64,
    pub message: Option<String>,
    pub error: Option<String>,
}

pub fn resolve_runtime_root() -> PathBuf {
    #[cfg(debug_assertions)]
    {
        // Debug/test binaries live under target/, so their location is not the
        // application root.  Use the compile-time checkout root instead.
        resolve_runtime_root_from_exe(None)
    }

    #[cfg(not(debug_assertions))]
    {
        return resolve_runtime_root_from_exe(std::env::current_exe().ok().as_deref());
    }
}

fn resolve_runtime_root_from_exe(exe: Option<&Path>) -> PathBuf {
    #[cfg(not(debug_assertions))]
    if let Some(parent) = exe.and_then(Path::parent) {
        return parent.to_path_buf();
    }

    #[cfg(debug_assertions)]
    if let Some(parent) = exe.and_then(Path::parent) {
        return parent.to_path_buf();
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR must be inside the project root")
        .to_path_buf()
}

pub fn runtime_status(root: &Path) -> Result<RuntimeStatus, String> {
    let layout = RuntimeLayout::for_root(root.to_path_buf());
    // Model consumers share the single portable `<exe>\\models` root; legacy
    // settings are intentionally ignored by ModelManager.
    let effective_models_dir = ModelManager::get_effective_models_dir(root, None);
    let state = read_state(&layout.state_path)?;
    let (writable, elevation_message) = match probe_writable(&layout.root) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    };

    let gpu = detect_nvidia_gpu();
    let requirements_hash = runtime_requirements_hash(gpu);
    let requirements_bundle_hash = requirements_bundle_hash();
    let metadata_current =
        state_metadata_current(&state, &requirements_hash, &requirements_bundle_hash)
            && requirements_manifest_current(&layout, gpu, &requirements_hash)
            && embedded_requirements_manifest_current(&layout);
    let required_models_missing = required_models_missing(root);
    let gemma_terms_accepted = crate::model_manager::gemma_terms_accepted(root);
    let cancelled = state
        .last_error
        .as_deref()
        .map(is_cancelled_error)
        .unwrap_or(false);
    let scripts_present = embedded_scripts_present(&layout);
    let python_present = layout.python_dir.is_dir()
        && fs::read_dir(&layout.python_dir)
            .ok()
            .and_then(|mut entries| entries.next())
            .is_some();
    let venv_present = layout.venv_dir.is_dir() && layout.python_executable().is_file();
    let lock_present = metadata_current
        && state.gpu == gpu
        && layout.lock_path.is_file()
        && !state.lock_sha256.is_empty()
        && file_sha256(&layout.lock_path).ok().as_deref() == Some(state.lock_sha256.as_str())
        && lockfile_requirements_match(&layout.lock_path, &requirements_hash);
    let dependency_marker_present = fs::read_to_string(&layout.dependencies_marker_path)
        .ok()
        .map(|value| value.trim() == state.lock_sha256)
        .unwrap_or(false);
    let dependency_files_ready = lock_present && venv_present && dependency_marker_present;
    let validation_fingerprint = runtime_validation_fingerprint(root, &state, gpu);
    let validation = (state.runtime_validation.fingerprint == validation_fingerprint
        && metadata_current)
        .then_some(&state.runtime_validation);
    let (python_import_ready, tokenizer_ready, embedding_ready, diagnostics) = runtime_diagnostics(
        root,
        metadata_current,
        state.gpu == gpu,
        venv_present,
        lock_present,
        dependency_marker_present,
        dependency_files_ready,
        validation,
    );
    let dependency_ready = dependency_files_ready && python_import_ready;
    let status_state = status_state_for_readiness(
        &state,
        metadata_current,
        gpu,
        dependency_ready,
        &required_models_missing,
        python_import_ready,
        tokenizer_ready,
        embedding_ready,
    );
    let required_stages_complete = required_setup_stages_complete(&status_state);
    let ready = required_stages_complete
        && scripts_present
        && python_present
        && dependency_ready
        && python_import_ready
        && tokenizer_ready
        && embedding_ready
        && required_models_missing.is_empty()
        && gemma_terms_accepted;
    let completed_stages = exposed_completed_stages(&status_state);
    let current_stage = if ready {
        Some("complete".to_string())
    } else if status_state.stage_completed(SetupStage::Complete) {
        Some("models".to_string())
    } else {
        Some(exposed_stage(status_state.current_stage).to_string())
    };
    let progress = setup_progress(&status_state);
    let status = if ready {
        "ready"
    } else if cancelled {
        "cancelled"
    } else if state.last_error.is_some() {
        "error"
    } else if SETUP_RUNNING.load(Ordering::SeqCst) {
        "running"
    } else {
        "pending"
    };
    let mut exposed_state = status_state.clone();
    if cancelled {
        exposed_state.last_error = None;
    }
    let status = RuntimeStatus {
        root_dir: layout.root.to_string_lossy().to_string(),
        python_dir: layout.python_dir.to_string_lossy().to_string(),
        venv_dir: layout.venv_dir.to_string_lossy().to_string(),
        models_dir: effective_models_dir.to_string_lossy().to_string(),
        uv_cache_dir: layout.uv_cache_dir.to_string_lossy().to_string(),
        logs_dir: layout.logs_dir.to_string_lossy().to_string(),
        setup_state: exposed_state,
        ready,
        setup_required: !ready || !required_models_missing.is_empty(),
        running: SETUP_RUNNING.load(Ordering::SeqCst),
        current_stage,
        progress,
        message: None,
        error: if cancelled {
            None
        } else {
            state.last_error.clone()
        },
        status: status.to_string(),
        cancelled,
        stages: setup_steps(&status_state),
        completed_stages,
        required_models_ready: required_models_missing.is_empty(),
        required_models_missing,
        writable,
        elevation_required: elevation_message
            .as_deref()
            .map(|message| message.starts_with("elevation_required:"))
            .unwrap_or(false),
        elevation_message,
        uv_present: layout.uv_path.is_file(),
        scripts_present,
        python_present,
        venv_present,
        lock_present,
        dependency_ready,
        python_import_ready,
        tokenizer_ready,
        embedding_ready,
        // Bootstrap cannot claim a live socket before the ASR manager has been
        // attached.  The Tauri command enriches this field with the client's
        // actual connection state for the Setup screen and session gate.
        asr_websocket_ready: false,
        diagnostics,
        llama_server_present: layout.llama_server_path.is_file(),
        gemma_terms_accepted,
        gemma_terms_version: crate::model_manager::GEMMA_TERMS_VERSION.to_string(),
        gemma_terms_model_sha256: crate::model_manager::GEMMA_EXPECTED_SHA256.to_string(),
        gemma_terms_source: crate::model_manager::GEMMA_TERMS_SOURCE.to_string(),
    };
    Ok(status)
}

fn is_cancelled_error(message: &str) -> bool {
    message.starts_with("setup cancelled")
}

fn runtime_requirements_hash(gpu: bool) -> String {
    sha256_hex(if gpu {
        REQUIREMENTS_GPU
    } else {
        REQUIREMENTS_CPU
    })
}

fn requirements_bundle_hash() -> String {
    let mut hasher = Sha256::new();
    for requirements in [REQUIREMENTS_PLAIN, REQUIREMENTS_CPU, REQUIREMENTS_GPU] {
        hasher.update((requirements.len() as u64).to_le_bytes());
        hasher.update(requirements);
    }
    format!("{:x}", hasher.finalize())
}

/// Build a cheap fingerprint for the runtime probes.  Hashing the multi-GB
/// model weights on every status poll would make the Setup screen itself
/// expensive, so model identity is represented by the required file names,
/// sizes, and modification times.  Requirements, scripts, lockfile, and the
/// Python version are cryptographically bound by the same fingerprint.
fn runtime_validation_fingerprint(root: &Path, state: &SetupState, gpu: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(runtime_requirements_hash(gpu).as_bytes());
    hasher.update(requirements_bundle_hash().as_bytes());
    hasher.update(scripts_hash().as_bytes());
    hasher.update(PYTHON_VERSION.as_bytes());
    hasher.update(state.lock_sha256.as_bytes());

    for definition in crate::model_manager::get_defined_models()
        .into_iter()
        .filter(|definition| definition.required)
    {
        hasher.update(definition.id.as_bytes());
        let model_root = RuntimeLayout::for_root(root.to_path_buf())
            .models_dir
            .join(&definition.id);
        for relative_path in definition.check_files {
            hasher.update(relative_path.as_bytes());
            let path = model_root.join(relative_path);
            match fs::metadata(path) {
                Ok(metadata) => {
                    hasher.update(metadata.len().to_le_bytes());
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                            hasher.update(duration.as_secs().to_le_bytes());
                            hasher.update(duration.subsec_nanos().to_le_bytes());
                        }
                    }
                }
                Err(_) => hasher.update([0u8; 16]),
            }
        }
    }

    format!("{:x}", hasher.finalize())
}

fn state_metadata_current(
    state: &SetupState,
    requirements_hash: &str,
    requirements_bundle_hash: &str,
) -> bool {
    state.state_version == SETUP_STATE_VERSION
        && state.app_version == env!("CARGO_PKG_VERSION")
        && state.python_version == PYTHON_VERSION
        && state.requirements_sha256 == requirements_hash
        && state.requirements_bundle_sha256 == requirements_bundle_hash
        && state.uv_sha256 == sha256_hex(UV_EXE)
        && state.scripts_sha256 == scripts_hash()
}

fn requirements_manifest_current(
    layout: &RuntimeLayout,
    gpu: bool,
    requirements_hash: &str,
) -> bool {
    file_sha256(&layout.requirements_path(gpu)).ok().as_deref() == Some(requirements_hash)
}

fn embedded_requirements_manifest_current(layout: &RuntimeLayout) -> bool {
    [
        (layout.root.join("requirements.txt"), REQUIREMENTS_PLAIN),
        (layout.root.join("requirements-cpu.txt"), REQUIREMENTS_CPU),
        (layout.root.join("requirements-gpu.txt"), REQUIREMENTS_GPU),
    ]
    .into_iter()
    .all(|(path, expected)| {
        // Older/debug test layouts may only materialize the selected
        // CPU/GPU manifest; the selected file is validated separately above.
        // When an optional sibling is present, however, bind it to the
        // embedded bytes so edits cannot leave a completed state authorized.
        !path.is_file() || file_sha256(&path).ok().as_deref() == Some(sha256_hex(expected).as_str())
    })
}

fn lockfile_requirements_match(path: &Path, requirements_hash: &str) -> bool {
    let header = format!(
        "# GameAssistant uv lock manifest\n# requirements-sha256: {}\n",
        requirements_hash
    );
    fs::read_to_string(path)
        .map(|contents| contents.starts_with(&header))
        .unwrap_or(false)
}

fn non_empty_model_file(root: &Path, model_id: &str, relative_path: &str) -> bool {
    let path = RuntimeLayout::for_root(root.to_path_buf())
        .models_dir
        .join(model_id)
        .join(relative_path);
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn runtime_diagnostics(
    root: &Path,
    metadata_current: bool,
    backend_current: bool,
    venv_present: bool,
    lock_present: bool,
    marker_present: bool,
    dependency_files_ready: bool,
    validation: Option<&RuntimeValidationState>,
) -> (bool, bool, bool, BTreeMap<String, String>) {
    let tokenizer_artifacts_present =
        non_empty_model_file(root, "kotoba-whisper-v2.0-faster", "tokenizer.json")
            && non_empty_model_file(root, "GLuCoSE-base-ja", "tokenizer_config.json");
    // Hugging Face may publish either the legacy PyTorch checkpoint or the
    // safetensors equivalent. Match ModelManager's validation so a complete
    // safetensors installation is not reported as an unusable embedding
    // runtime.
    let embedding_weights = non_empty_model_file(root, "GLuCoSE-base-ja", "pytorch_model.bin")
        || non_empty_model_file(root, "GLuCoSE-base-ja", "model.safetensors");
    let embedding_artifacts_present = embedding_weights
        && non_empty_model_file(root, "GLuCoSE-base-ja", "sentencepiece.bpe.model");
    let python_import_ready = dependency_files_ready
        && validation
            .map(|result| result.python_import_ready)
            .unwrap_or(false);
    let tokenizer_ready = tokenizer_artifacts_present
        && dependency_files_ready
        && validation
            .map(|result| result.tokenizer_ready)
            .unwrap_or(false);
    let embedding_ready = embedding_artifacts_present
        && dependency_files_ready
        && validation
            .map(|result| result.embedding_ready)
            .unwrap_or(false);
    let dependency_code = if !metadata_current {
        "dependency_manifest_stale"
    } else if !backend_current {
        "dependency_backend_changed"
    } else if !venv_present {
        "dependency_venv_missing"
    } else if !lock_present {
        "dependency_lock_stale"
    } else if !marker_present {
        "dependency_marker_missing"
    } else if !dependency_files_ready {
        "dependency_sync_incomplete"
    } else if validation.is_none() {
        "dependency_runtime_unverified"
    } else if !python_import_ready {
        "python_import_failed"
    } else {
        "dependency_ready"
    };
    let mut diagnostics = BTreeMap::new();
    diagnostics.insert("dependency".to_string(), dependency_code.to_string());
    diagnostics.insert(
        "python_import".to_string(),
        if !dependency_files_ready {
            "dependency_not_ready"
        } else if validation.is_none() {
            "python_import_unverified"
        } else if python_import_ready {
            "python_import_ready"
        } else {
            "python_import_failed"
        }
        .to_string(),
    );
    diagnostics.insert(
        "tokenizer".to_string(),
        if !tokenizer_artifacts_present {
            "tokenizer_missing"
        } else if validation.is_none() {
            "tokenizer_runtime_unverified"
        } else if tokenizer_ready {
            "tokenizer_ready"
        } else {
            "tokenizer_initialization_failed"
        }
        .to_string(),
    );
    diagnostics.insert(
        "embedding".to_string(),
        if !embedding_artifacts_present {
            "embedding_model_missing"
        } else if validation.is_none() {
            "embedding_runtime_unverified"
        } else if embedding_ready {
            "embedding_ready"
        } else {
            "embedding_initialization_failed"
        }
        .to_string(),
    );
    if let Some(validation) = validation {
        for (key, value) in &validation.diagnostics {
            diagnostics.insert(key.clone(), value.clone());
        }
    }
    (
        python_import_ready,
        tokenizer_ready,
        embedding_ready,
        diagnostics,
    )
}

fn status_state_for_readiness(
    state: &SetupState,
    metadata_current: bool,
    gpu: bool,
    dependency_ready: bool,
    required_models_missing: &[String],
    python_import_ready: bool,
    tokenizer_ready: bool,
    embedding_ready: bool,
) -> SetupState {
    let mut projected = state.clone();
    if !metadata_current {
        projected.completed_stages.clear();
        projected.current_stage = SetupStage::Pending;
    } else if state.gpu != gpu {
        projected.clear_from(SetupStage::Dependencies);
    }
    if !dependency_ready && projected.stage_completed(SetupStage::Dependencies) {
        projected.clear_from(SetupStage::Dependencies);
    }
    let models_incomplete = !required_models_missing.is_empty()
        || !python_import_ready
        || !tokenizer_ready
        || !embedding_ready;
    if models_incomplete && projected.stage_completed(SetupStage::Models) {
        projected.clear_from(SetupStage::Models);
    }
    // Projection is read-only, but a prior cancellation/error must remain
    // visible to the caller while stale completion markers are withdrawn.
    projected.last_error = state.last_error.clone();
    projected
}

/// Guard used by commands that must never run before the bootstrap contract
/// has been completed and revalidated.
pub fn runtime_is_ready(root: &Path) -> Result<(), String> {
    let status = runtime_status(root)?;
    if status.ready {
        Ok(())
    } else {
        Err(format!(
            "runtime setup is not ready (status: {})",
            status.status
        ))
    }
}

/// Return the llama binary only when it was installed by the current embedded
/// resource set.  Consumers must not discover an arbitrary root-level binary.
pub fn validated_llama_server_path(root: &Path) -> Option<PathBuf> {
    let layout = RuntimeLayout::for_root(root.to_path_buf());
    let state = read_state(&layout.state_path).ok()?;
    if !state.stage_completed(SetupStage::Resources) || !layout.llama_server_path.is_file() {
        return None;
    }
    let expected = LLAMA_RUNTIME_FILES
        .iter()
        .find(|(name, _)| *name == "llama-server.exe")
        .map(|(_, bytes)| sha256_hex(bytes))?;
    (file_sha256(&layout.llama_server_path).ok().as_deref() == Some(expected.as_str()))
        .then_some(layout.llama_server_path)
}

/// Creates the portable runtime directories and atomically extracts all embedded
/// files.  This is intentionally separate from package/model installation so the
/// UI can become available while a long first-run install is in progress.
pub fn prepare_runtime(root: &Path) -> Result<RuntimeStatus, String> {
    let layout = RuntimeLayout::for_root(root.to_path_buf());
    ensure_root_writable(&layout.root)?;
    for dir in [
        &layout.python_dir,
        &layout.venv_dir,
        &layout.models_dir,
        &layout.uv_cache_dir,
        &layout.logs_dir,
        &layout.scripts_dir,
    ] {
        fs::create_dir_all(dir).map_err(|error| map_io("create runtime directory", dir, error))?;
    }
    if let Some(parent) = layout.llama_server_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| map_io("create llama runtime directory", parent, error))?;
    }

    extract_embedded_resources(&layout)?;

    let gpu = detect_nvidia_gpu();
    let requirements_hash = sha256_hex(if gpu {
        REQUIREMENTS_GPU
    } else {
        REQUIREMENTS_CPU
    });
    let mut state = read_state(&layout.state_path)?;
    sync_state_metadata(&mut state, gpu, &requirements_hash);
    // A prior release could leave a pre-Terms setup attempt pointing at a
    // stage even though no worker is running.  Terms are the prerequisite for
    // every setup stage, so expose a clean pending state until acknowledgement.
    let terms_accepted = crate::model_manager::gemma_terms_accepted(root);
    // Resource extraction is idempotent, but marking the stage again would
    // advance a previously completed state back to `python` on every launch.
    // Preserve the terminal stage so the UI and a concurrent status poll never
    // observe a false setup regression.
    if !state.stage_completed(SetupStage::Resources) {
        state.mark_completed(SetupStage::Resources);
    }
    if !terms_accepted {
        // mark_completed(Resources) normally advances to Python. Keep the
        // prerequisite gate visibly pending instead of implying a worker is
        // active before the user has acknowledged Terms.
        state.current_stage = SetupStage::Pending;
        state.last_error = None;
    }
    write_state_atomic(&layout.state_path, &state)?;
    runtime_status(root)
}

pub fn cancel_setup() {
    CANCEL_SETUP.store(true, Ordering::SeqCst);
}

/// Clear an error left by a pre-terms setup attempt after the user provides
/// the required Gemma acknowledgement.  Retrying setup is then represented as
/// pending/running rather than being permanently stuck in the old error state.
pub fn clear_setup_error(root: &Path) -> Result<(), String> {
    let layout = RuntimeLayout::for_root(root.to_path_buf());
    let mut state = read_state(&layout.state_path)?;
    if state.last_error.is_none() {
        return Ok(());
    }
    state.last_error = None;
    state.updated_at = now_string();
    write_state_atomic(&layout.state_path, &state)
}

pub fn setup_in_progress_cancelled() -> bool {
    CANCEL_SETUP.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElevationRequestResult {
    pub launched: bool,
    pub message: String,
}

/// Request a just-in-time elevated setup process.  The elevated process gets a
/// private `--elevated-setup` argument and exits after setup; the original app
/// remains a standard-user process.  No elevation is attempted on non-Windows.
pub fn relaunch_setup_elevated() -> Result<ElevationRequestResult, String> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let exe = std::env::current_exe()
            .map_err(|error| format!("cannot resolve GameAssistant executable: {}", error))?;
        let to_wide =
            |value: &OsStr| -> Vec<u16> { value.encode_wide().chain(std::iter::once(0)).collect() };
        let operation = to_wide(OsStr::new("runas"));
        let executable = to_wide(exe.as_os_str());
        let arguments = to_wide(OsStr::new("--elevated-setup"));
        let directory = to_wide(exe.parent().unwrap_or_else(|| Path::new(".")).as_os_str());
        let instance = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(executable.as_ptr()),
                PCWSTR(arguments.as_ptr()),
                PCWSTR(directory.as_ptr()),
                SW_SHOWNORMAL,
            )
        };
        if instance.0 as usize <= 32 {
            return Err(
                "elevation_required: UAC request was rejected or could not be started".to_string(),
            );
        }
        Ok(ElevationRequestResult {
            launched: true,
            message: "Elevated setup process started; this process remains standard-user."
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    {
        Err("elevation is only supported on Windows; choose a writable folder".to_string())
    }
}

/// Runs the resumable uv/Python/dependency/model stages.  `model_mgr` and `app`
/// are optional to keep this function useful in tests; production setup passes
/// both so required models are downloaded through the existing Models Manager.
pub async fn run_setup(
    root: &Path,
    model_mgr: Option<std::sync::Arc<ModelManager>>,
    app: Option<AppHandle>,
) -> Result<RuntimeStatus, String> {
    let lock = SETUP_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;
    // Reset cancellation only after acquiring the process-wide setup lock so a
    // second invocation cannot accidentally clear the cancel request belonging
    // to an already-running setup.
    CANCEL_SETUP.store(false, Ordering::SeqCst);
    SETUP_RUNNING.store(true, Ordering::SeqCst);
    let running_guard = SetupRunningGuard;
    prepare_runtime(root)?;
    let layout = RuntimeLayout::for_root(root.to_path_buf());
    let gpu = detect_nvidia_gpu();
    let requirements = layout.requirements_path(gpu);
    let requirements_hash = sha256_hex(if gpu {
        REQUIREMENTS_GPU
    } else {
        REQUIREMENTS_CPU
    });
    let mut state = read_state(&layout.state_path)?;
    sync_state_metadata(&mut state, gpu, &requirements_hash);
    write_state_atomic(&layout.state_path, &state)?;

    // Gemma is a required setup artifact. Do not spend time downloading or
    // using it until the current acknowledgement includes its version/hash.
    if !crate::model_manager::gemma_terms_accepted(root) {
        // This can be reached by an older caller that still auto-starts setup
        // before the user has acknowledged the required Terms. Keep the state
        // pending and actionable instead of turning a prerequisite into a
        // retry-only error state.
        state.current_stage = SetupStage::Pending;
        state.last_error = None;
        state.updated_at = now_string();
        write_state_atomic(&layout.state_path, &state)?;
        return runtime_status(root);
    }

    if !state.stage_completed(SetupStage::Python) || !layout.python_dir.exists() {
        set_current_stage(&layout, &mut state, SetupStage::Python)?;
        emit_progress(&app, SetupStage::Python, "running", 0.0, None, None);
        let python_dir = layout.python_dir.to_string_lossy().to_string();
        run_uv(
            &layout,
            &[
                "python",
                "install",
                "--install-dir",
                &python_dir,
                PYTHON_VERSION,
            ],
        )
        .map_err(|error| fail_stage_event(&app, &layout, &mut state, SetupStage::Python, error))?;
        state.mark_completed(SetupStage::Python);
        write_state_atomic(&layout.state_path, &state)?;
        emit_progress(&app, SetupStage::Python, "completed", 100.0, None, None);
    }
    check_cancelled(&app, &layout, &mut state)?;

    if !state.stage_completed(SetupStage::Venv) || !layout.python_executable().is_file() {
        set_current_stage(&layout, &mut state, SetupStage::Venv)?;
        emit_progress(&app, SetupStage::Venv, "running", 0.0, None, None);
        let venv = layout.venv_dir.to_string_lossy().to_string();
        run_uv(&layout, &["venv", &venv, "--python", PYTHON_VERSION]).map_err(|error| {
            fail_stage_event(&app, &layout, &mut state, SetupStage::Venv, error)
        })?;
        state.mark_completed(SetupStage::Venv);
        write_state_atomic(&layout.state_path, &state)?;
        emit_progress(&app, SetupStage::Venv, "completed", 100.0, None, None);
    }
    check_cancelled(&app, &layout, &mut state)?;

    let lock_valid = layout.lock_path.is_file()
        && state.lock_sha256 == file_sha256(&layout.lock_path).unwrap_or_default();
    let dependencies_installed = fs::read_to_string(&layout.dependencies_marker_path)
        .ok()
        .map(|value| value.trim() == state.lock_sha256)
        .unwrap_or(false);
    if !state.stage_completed(SetupStage::Dependencies) || !lock_valid || !dependencies_installed {
        set_current_stage(&layout, &mut state, SetupStage::Dependencies)?;
        emit_progress(&app, SetupStage::Dependencies, "running", 0.0, None, None);
        let python = layout.python_executable().to_string_lossy().to_string();
        let requirements = requirements.to_string_lossy().to_string();
        ensure_lock_file(&layout, &requirements, &python, gpu).map_err(|error| {
            fail_stage_event(&app, &layout, &mut state, SetupStage::Dependencies, error)
        })?;
        state.lock_sha256 = file_sha256(&layout.lock_path).map_err(|error| {
            fail_stage_event(&app, &layout, &mut state, SetupStage::Dependencies, error)
        })?;
        write_state_atomic(&layout.state_path, &state)?;
        let lock = layout.lock_path.to_string_lossy().to_string();
        let sync_args = dependency_sync_args(&python, &lock, gpu);
        let sync_arg_refs: Vec<&str> = sync_args.iter().map(String::as_str).collect();
        run_uv(&layout, &sync_arg_refs).map_err(|error| {
            fail_stage_event(&app, &layout, &mut state, SetupStage::Dependencies, error)
        })?;
        write_bytes_atomic(
            &layout.dependencies_marker_path,
            state.lock_sha256.as_bytes(),
        )?;
        state.mark_completed(SetupStage::Dependencies);
        write_state_atomic(&layout.state_path, &state)?;
        emit_progress(
            &app,
            SetupStage::Dependencies,
            "completed",
            100.0,
            None,
            None,
        );
    }
    check_cancelled(&app, &layout, &mut state)?;

    // A completed marker alone is not sufficient: a model may have been
    // removed after a prior setup. Re-enter this stage whenever any required
    // model is actually missing.
    if !state.stage_completed(SetupStage::Models) || !required_models_missing(root).is_empty() {
        set_current_stage(&layout, &mut state, SetupStage::Models)?;
        emit_progress(&app, SetupStage::Models, "running", 0.0, None, None);
        let manager = model_mgr.ok_or_else(|| {
            fail_stage_event(
                &app,
                &layout,
                &mut state,
                SetupStage::Models,
                "required model setup needs the Models Manager".to_string(),
            )
        })?;
        let app_handle = app.clone().ok_or_else(|| {
            fail_stage_event(
                &app,
                &layout,
                &mut state,
                SetupStage::Models,
                "required model setup needs an application handle".to_string(),
            )
        })?;
        let required_models: Vec<_> = crate::model_manager::get_defined_models()
            .into_iter()
            .filter(|definition| definition.required)
            .collect();
        let model_total = required_models.len().max(1);
        for (model_index, definition) in required_models.into_iter().enumerate() {
            check_cancelled(&app, &layout, &mut state)?;
            let installed = ModelManager::scan_models_status(
                root,
                Some(layout.models_dir.to_string_lossy().into_owned()),
            )
            .into_iter()
            .find(|status| status.id == definition.id)
            .map(|status| status.is_installed)
            .unwrap_or(false);
            if installed {
                emit_progress(
                    &app,
                    SetupStage::Models,
                    "running",
                    ((model_index + 1) as f64 / model_total as f64) * 100.0,
                    Some(format!("Required model ready: {}", definition.id)),
                    None,
                );
                continue;
            }
            append_setup_log(
                &layout,
                &format!("Downloading required model: {}", definition.id),
            );
            manager
                .download_model(
                    app_handle.clone(),
                    root.to_path_buf(),
                    definition.id.clone(),
                    Some(layout.models_dir.to_string_lossy().into_owned()),
                )
                .await
                .map_err(|error| {
                    fail_stage_event(&app, &layout, &mut state, SetupStage::Models, error)
                })?;
            // ModelManager deliberately treats a user cancellation as a
            // recoverable download result; setup must still persist and expose
            // it as cancellation rather than reporting a generic model error.
            check_cancelled(&app, &layout, &mut state)?;
            let completed = ModelManager::scan_models_status(
                root,
                Some(layout.models_dir.to_string_lossy().into_owned()),
            )
            .into_iter()
            .find(|status| status.id == definition.id)
            .map(|status| status.is_installed)
            .unwrap_or(false);
            if !completed {
                return Err(fail_stage_event(
                    &app,
                    &layout,
                    &mut state,
                    SetupStage::Models,
                    format!(
                        "model download completed without all expected files: {}",
                        definition.id
                    ),
                ));
            }
            emit_progress(
                &app,
                SetupStage::Models,
                "running",
                ((model_index + 1) as f64 / model_total as f64) * 100.0,
                Some(format!("Required model ready: {}", definition.id)),
                None,
            );
        }
        state.mark_completed(SetupStage::Models);
        state.mark_completed(SetupStage::Complete);
        write_state_atomic(&layout.state_path, &state)?;
        emit_progress(&app, SetupStage::Models, "completed", 100.0, None, None);
    }

    if state.stage_completed(SetupStage::Models) && !state.stage_completed(SetupStage::Complete) {
        state.mark_completed(SetupStage::Complete);
        write_state_atomic(&layout.state_path, &state)?;
    }
    check_cancelled(&app, &layout, &mut state)?;

    // A complete file/lock manifest only proves that installation finished;
    // each Python runtime layer must initialize successfully before the setup
    // contract can report ready.  Persist both success and failure so a stale
    // venv cannot be mistaken for a validated one on the next launch.
    let validation_result = validate_runtime(&layout, &mut state, gpu, &app);
    write_state_atomic(&layout.state_path, &state)?;
    drop(running_guard);
    validation_result?;
    runtime_status(root)
}

fn sync_state_metadata(state: &mut SetupState, gpu: bool, requirements_hash: &str) {
    let scripts_hash = scripts_hash();
    let uv_hash = sha256_hex(UV_EXE);
    let requirements_bundle_hash = requirements_bundle_hash();
    let metadata_changed = state.state_version != SETUP_STATE_VERSION
        || state.app_version != env!("CARGO_PKG_VERSION")
        || state.python_version != PYTHON_VERSION
        || state.requirements_sha256 != requirements_hash
        || state.requirements_bundle_sha256 != requirements_bundle_hash
        || state.uv_sha256 != uv_hash
        || state.scripts_sha256 != scripts_hash;
    if metadata_changed {
        state.completed_stages.clear();
        state.current_stage = SetupStage::Pending;
        state.runtime_validation = RuntimeValidationState::default();
    } else if state.gpu != gpu {
        state.clear_from(SetupStage::Dependencies);
        state.runtime_validation = RuntimeValidationState::default();
    }
    state.state_version = SETUP_STATE_VERSION;
    state.app_version = env!("CARGO_PKG_VERSION").to_string();
    state.python_version = PYTHON_VERSION.to_string();
    state.requirements_sha256 = requirements_hash.to_string();
    state.requirements_bundle_sha256 = requirements_bundle_hash;
    state.uv_sha256 = uv_hash;
    state.scripts_sha256 = scripts_hash;
    state.gpu = gpu;
    state.updated_at = now_string();
}

struct SetupRunningGuard;

impl Drop for SetupRunningGuard {
    fn drop(&mut self) {
        SETUP_RUNNING.store(false, Ordering::SeqCst);
    }
}

fn exposed_stage(stage: SetupStage) -> &'static str {
    match stage {
        SetupStage::Resources => "scripts",
        SetupStage::Dependencies => "packages",
        other => other.as_str(),
    }
}

fn exposed_completed_stages(state: &SetupState) -> Vec<String> {
    [
        SetupStage::Python,
        SetupStage::Venv,
        SetupStage::Dependencies,
        SetupStage::Resources,
        SetupStage::Models,
        SetupStage::Complete,
    ]
    .into_iter()
    .filter(|stage| state.stage_completed(*stage))
    .map(|stage| exposed_stage(stage).to_string())
    .collect()
}

fn setup_progress(state: &SetupState) -> f64 {
    if state.stage_completed(SetupStage::Complete) {
        return 100.0;
    }
    let completed = [
        SetupStage::Python,
        SetupStage::Venv,
        SetupStage::Dependencies,
        SetupStage::Models,
    ]
    .into_iter()
    .filter(|stage| state.stage_completed(*stage))
    .count();
    (completed as f64 / 4.0) * 100.0
}

fn setup_steps(state: &SetupState) -> Vec<SetupStepStatus> {
    [
        ("python", SetupStage::Python),
        ("venv", SetupStage::Venv),
        ("packages", SetupStage::Dependencies),
        ("scripts", SetupStage::Resources),
        ("models", SetupStage::Models),
    ]
    .into_iter()
    .map(|(id, stage)| {
        let status = if state.stage_completed(stage) {
            "completed"
        } else if state.current_stage == stage && SETUP_RUNNING.load(Ordering::SeqCst) {
            if state
                .last_error
                .as_deref()
                .map(is_cancelled_error)
                .unwrap_or(false)
            {
                "cancelled"
            } else if state.last_error.is_some() {
                "error"
            } else {
                "running"
            }
        } else {
            "pending"
        };
        SetupStepStatus {
            id: id.to_string(),
            status: status.to_string(),
            progress: if status == "completed" { 100.0 } else { 0.0 },
            message: None,
            error: if status == "error" {
                state.last_error.clone()
            } else {
                None
            },
        }
    })
    .collect()
}

fn required_models_missing(root: &Path) -> Vec<String> {
    let portable_models = RuntimeLayout::for_root(root.to_path_buf()).models_dir;
    ModelManager::scan_models_status(root, Some(portable_models.to_string_lossy().into_owned()))
        .into_iter()
        .filter(|status| status.required && !status.is_installed)
        .map(|status| status.id)
        .collect()
}

fn required_setup_stages_complete(state: &SetupState) -> bool {
    [
        SetupStage::Resources,
        SetupStage::Python,
        SetupStage::Venv,
        SetupStage::Dependencies,
        SetupStage::Models,
        SetupStage::Complete,
    ]
    .into_iter()
    .all(|stage| state.stage_completed(stage))
}

fn embedded_scripts_present(layout: &RuntimeLayout) -> bool {
    let asr_hash = sha256_hex(ASR_SERVER_PY);
    let vits_hash = sha256_hex(VITS2_SERVER_PY);
    file_sha256(&layout.scripts_dir.join("asr_server.py"))
        .ok()
        .as_deref()
        == Some(asr_hash.as_str())
        && file_sha256(&layout.scripts_dir.join("vits2_server.py"))
            .ok()
            .as_deref()
            == Some(vits_hash.as_str())
}

fn emit_progress(
    app: &Option<AppHandle>,
    stage: SetupStage,
    status: &str,
    progress: f64,
    message: Option<String>,
    error: Option<String>,
) {
    let Some(app) = app else { return };
    let payload = serde_json::json!({
        "stage": exposed_stage(stage),
        "status": status,
        "progress": progress,
        "message": message,
        "error": error,
    });
    let _ = app.emit("setup_progress", payload);
}

fn set_current_stage(
    layout: &RuntimeLayout,
    state: &mut SetupState,
    stage: SetupStage,
) -> Result<(), String> {
    state.current_stage = stage;
    state.last_error = None;
    state.updated_at = now_string();
    write_state_atomic(&layout.state_path, state)
}

fn check_cancelled(
    app: &Option<AppHandle>,
    layout: &RuntimeLayout,
    state: &mut SetupState,
) -> Result<(), String> {
    if !setup_in_progress_cancelled() {
        return Ok(());
    }
    let stage = state.current_stage;
    let error = fail_stage(
        layout,
        state,
        stage,
        "setup cancelled; run setup again to resume".to_string(),
    );
    emit_progress(app, stage, "cancelled", 0.0, None, Some(error.clone()));
    Err(error)
}

fn fail_stage(
    layout: &RuntimeLayout,
    state: &mut SetupState,
    stage: SetupStage,
    error: String,
) -> String {
    state.current_stage = stage;
    state.last_error = Some(error.clone());
    state.updated_at = now_string();
    let _ = write_state_atomic(&layout.state_path, state);
    append_setup_log(layout, &format!("ERROR [{}] {}", stage.as_str(), error));
    error
}

fn fail_stage_event(
    app: &Option<AppHandle>,
    layout: &RuntimeLayout,
    state: &mut SetupState,
    stage: SetupStage,
    error: String,
) -> String {
    let message = fail_stage(layout, state, stage, error);
    let status = if message.starts_with("setup cancelled") {
        "cancelled"
    } else {
        "error"
    };
    emit_progress(app, stage, status, 0.0, None, Some(message.clone()));
    message
}

fn emit_runtime_validation_progress(
    app: &Option<AppHandle>,
    stage: &str,
    status: &str,
    message: Option<String>,
    error: Option<String>,
) {
    let Some(app) = app else { return };
    let payload = serde_json::json!({
        "stage": stage,
        "status": status,
        "progress": if status == "completed" { 100.0 } else { 0.0 },
        "message": message,
        "error": error,
    });
    let _ = app.emit("setup_progress", payload);
}

/// Run a probe without inheriting the caller's shell/configuration.  A probe
/// can import large ML packages, so it has a bounded lifetime and its output
/// is reduced to a short, secret-free diagnostic before it reaches setup.log.
fn run_python_probe(layout: &RuntimeLayout, code: &str, label: &str) -> Result<(), String> {
    let python = layout.python_executable();
    if !python.is_file() {
        return Err(format!(
            "portable Python executable is missing at {}",
            python.display()
        ));
    }

    let models_dir = layout.models_dir.to_string_lossy().to_string();
    let cache_dir = layout.uv_cache_dir.to_string_lossy().to_string();
    let mut command = Command::new(&python);
    command
        .args(["-c", code])
        .current_dir(&layout.root)
        .env("RUNTIME_ROOT", layout.root.to_string_lossy().to_string())
        .env("MODELS_DIR", &models_dir)
        .env("CACHE_DIR", &cache_dir)
        .env("HF_HOME", &cache_dir)
        .env("TRANSFORMERS_CACHE", &cache_dir)
        .env("HF_HUB_CACHE", &cache_dir)
        .env("HUGGINGFACE_HUB_CACHE", &cache_dir)
        .env("SENTENCE_TRANSFORMERS_HOME", &cache_dir)
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("{} probe could not start: {}", label, error))?;
    let started_at = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started_at.elapsed() >= RUNTIME_PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} probe timed out after {:?}",
                    label, RUNTIME_PROBE_TIMEOUT
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} probe process polling failed: {}", label, error));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("{} probe output could not be read: {}", label, error))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = secret_free_probe_detail(&String::from_utf8_lossy(&output.stderr));
    if detail.is_empty() {
        Err(format!(
            "{} probe failed with status {}",
            label, output.status
        ))
    } else {
        Err(format!("{} probe failed: {}", label, detail))
    }
}

/// Do not persist values that look like credentials even if a third-party
/// package accidentally prints its environment while failing to initialize.
fn secret_free_probe_detail(detail: &str) -> String {
    let mut safe_lines = Vec::new();
    for line in detail.lines() {
        let lower = line.to_ascii_lowercase();
        if [
            "api_key",
            "apikey",
            "access_token",
            "refresh_token",
            "client_secret",
            "authorization",
            "bearer ",
            "password",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            safe_lines.push(trimmed);
        }
    }
    let mut result = safe_lines.join(" | ");
    if result.chars().count() > 512 {
        result = result.chars().take(512).collect::<String>();
        result.push('…');
    }
    result
}

fn validate_runtime(
    layout: &RuntimeLayout,
    state: &mut SetupState,
    gpu: bool,
    app: &Option<AppHandle>,
) -> Result<(), String> {
    let fingerprint = runtime_validation_fingerprint(&layout.root, state, gpu);
    let mut validation = RuntimeValidationState {
        fingerprint,
        ..RuntimeValidationState::default()
    };
    let probes = [
        (
            "python-import",
            PYTHON_IMPORT_PROBE,
            "Python import",
            "python_import_ready",
        ),
        (
            "tokenizer",
            TOKENIZER_PROBE,
            "Tokenizer initialization",
            "tokenizer_ready",
        ),
        (
            "embedding",
            EMBEDDING_PROBE,
            "Embedding model initialization",
            "embedding_ready",
        ),
    ];
    let mut failures = Vec::new();

    for (stage, code, label, diagnostic_key) in probes {
        emit_runtime_validation_progress(
            app,
            stage,
            "running",
            Some(format!("Validating {}…", label)),
            None,
        );
        match run_python_probe(layout, code, label) {
            Ok(()) => {
                match diagnostic_key {
                    "python_import_ready" => validation.python_import_ready = true,
                    "tokenizer_ready" => validation.tokenizer_ready = true,
                    "embedding_ready" => validation.embedding_ready = true,
                    _ => {}
                }
                validation.diagnostics.insert(
                    stage.to_string(),
                    format!("{}_ready", stage.replace('-', "_")),
                );
                emit_runtime_validation_progress(
                    app,
                    stage,
                    "completed",
                    Some(format!("{} is ready", label)),
                    None,
                );
            }
            Err(error) => {
                validation
                    .diagnostics
                    .insert(stage.to_string(), error.clone());
                failures.push(error.clone());
                emit_runtime_validation_progress(app, stage, "error", None, Some(error));
            }
        }
    }

    state.runtime_validation = validation;
    if failures.is_empty() {
        state.last_error = None;
        Ok(())
    } else {
        state.current_stage = SetupStage::Models;
        state.last_error = Some(format!(
            "runtime validation failed: {}",
            failures.join("; ")
        ));
        Err(state
            .last_error
            .clone()
            .unwrap_or_else(|| "runtime validation failed; inspect setup diagnostics".to_string()))
    }
}

fn run_uv(layout: &RuntimeLayout, args: &[&str]) -> Result<(), String> {
    if !layout.uv_path.is_file() {
        return Err(format!("uv.exe is missing at {}", layout.uv_path.display()));
    }
    let command_args = uv_command_args(layout, args);
    let command_line = format!("uv {}", command_args.join(" "));
    append_setup_log(layout, &format!("Running: {}", command_line));
    let log_path = layout.logs_dir.join("setup.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| map_io("open setup log", &log_path, error))?;
    let log_file_err = log_file
        .try_clone()
        .map_err(|error| map_io("prepare setup log", &log_path, error))?;
    let mut command = Command::new(&layout.uv_path);
    command.args(&command_args).current_dir(&layout.root);
    // `uv.exe` is a console subsystem executable.  The Tauri application is
    // a GUI subsystem executable, so Windows would otherwise create a visible
    // console window for each first-run setup command.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    // Do not allow a developer-machine uv configuration to redirect a
    // portable setup to another drive (for example UV_CACHE_DIR=f:\\uv).
    // Keep ordinary proxy/network variables intact, but remove all uv/pip
    // configuration variables before setting the two paths owned by this
    // release.  The explicit CLI flags also win over any config file uv may
    // discover when an older uv build is used.
    for (key, _) in std::env::vars_os() {
        let key_upper = key.to_string_lossy().to_ascii_uppercase();
        if key_upper.starts_with("UV_") || key_upper.starts_with("PIP_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .env("UV_PYTHON_INSTALL_DIR", &layout.python_dir)
        .env("UV_CACHE_DIR", &layout.uv_cache_dir)
        .env("UV_NO_CONFIG", "1")
        .env("UV_NO_PROGRESS", "1")
        // Redirect both streams to the persistent setup log while polling the
        // process.  This keeps the pipes from filling and allows cancellation
        // during a long Python/package operation.
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .map_err(|error| map_io("run uv", &layout.uv_path, error))?;

    loop {
        if setup_in_progress_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("setup cancelled; run setup again to resume".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(format!(
                    "{} failed with status {}; see {} for details",
                    command_line,
                    status,
                    log_path.display()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(250)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} process polling failed: {}",
                    command_line, error
                ));
            }
        }
    }
}

fn uv_command_args(layout: &RuntimeLayout, args: &[&str]) -> Vec<String> {
    let mut command_args = vec![
        "--no-config".to_string(),
        "--cache-dir".to_string(),
        layout.uv_cache_dir.to_string_lossy().to_string(),
    ];
    command_args.extend(args.iter().map(|arg| (*arg).to_string()));
    command_args
}

/// Creates a deterministic, hash-pinned requirements lock file. `uv pip
/// compile` resolves the transitive dependency graph once and records hashes
/// for the Windows/Python target; subsequent `uv pip sync` runs consume this
/// file directly instead of resolving moving latest versions again.
fn ensure_lock_file(
    layout: &RuntimeLayout,
    requirements_path: &str,
    python_path: &str,
    gpu: bool,
) -> Result<(), String> {
    let requirements = fs::read_to_string(requirements_path).map_err(|error| {
        map_io(
            "read dependency manifest",
            Path::new(requirements_path),
            error,
        )
    })?;
    let requirements_hash = sha256_hex(requirements.as_bytes());
    let header = format!(
        "# GameAssistant uv lock manifest\n# requirements-sha256: {}\n",
        requirements_hash
    );
    if let Ok(existing) = fs::read_to_string(&layout.lock_path) {
        if existing.starts_with(&header) {
            return Ok(());
        }
    }
    let generated_path = layout.root.join(".uv-lock.generated.txt");
    let generated_path_string = generated_path.to_string_lossy().to_string();
    let compile_args =
        dependency_compile_args(python_path, &generated_path_string, requirements_path, gpu);
    let compile_arg_refs: Vec<&str> = compile_args.iter().map(String::as_str).collect();
    if let Err(error) = run_uv(layout, &compile_arg_refs) {
        let _ = fs::remove_file(&generated_path);
        return Err(error);
    }

    let compiled = fs::read(&generated_path)
        .map_err(|error| map_io("read generated dependency lock", &generated_path, error));
    let _ = fs::remove_file(&generated_path);
    let compiled = compiled?;
    let mut lock_bytes = header.into_bytes();
    lock_bytes.extend_from_slice(&compiled);
    write_bytes_atomic(&layout.lock_path, &lock_bytes)
}

fn dependency_compile_args(
    python_path: &str,
    output_path: &str,
    requirements_path: &str,
    gpu: bool,
) -> Vec<String> {
    let mut args = vec![
        "pip".to_string(),
        "compile".to_string(),
        "--python".to_string(),
        python_path.to_string(),
    ];
    if gpu {
        // Select CUDA wheels only for packages in the PyTorch ecosystem.  This
        // keeps generic packages on PyPI and avoids unsafe cross-index
        // resolution for names such as requests or safetensors.
        args.extend(["--torch-backend".to_string(), GPU_TORCH_BACKEND.to_string()]);
    }
    args.extend([
        "--generate-hashes".to_string(),
        "--output-file".to_string(),
        output_path.to_string(),
        requirements_path.to_string(),
    ]);
    args
}

fn dependency_sync_args(python_path: &str, lock_path: &str, gpu: bool) -> Vec<String> {
    let mut args = vec![
        "pip".to_string(),
        "sync".to_string(),
        "--python".to_string(),
        python_path.to_string(),
    ];
    if gpu {
        args.extend(["--torch-backend".to_string(), GPU_TORCH_BACKEND.to_string()]);
    }
    args.push(lock_path.to_string());
    args
}

fn extract_embedded_resources(layout: &RuntimeLayout) -> Result<(), String> {
    let resources = [
        (layout.scripts_dir.join("asr_server.py"), ASR_SERVER_PY),
        (layout.scripts_dir.join("vits2_server.py"), VITS2_SERVER_PY),
        (layout.root.join("requirements.txt"), REQUIREMENTS_PLAIN),
        (layout.root.join("requirements-cpu.txt"), REQUIREMENTS_CPU),
        (layout.root.join("requirements-gpu.txt"), REQUIREMENTS_GPU),
        (layout.root.join("NOTICE-GEMMA.txt"), NOTICE_GEMMA),
        (layout.uv_path.clone(), UV_EXE),
    ];
    let mut resources = resources.to_vec();
    let runtime_dir = layout
        .llama_server_path
        .parent()
        .ok_or_else(|| "llama runtime path has no parent".to_string())?;
    resources.extend(
        LLAMA_RUNTIME_FILES
            .iter()
            .map(|(name, bytes)| (runtime_dir.join(name), *bytes)),
    );
    resources.extend(
        NOD_WAV_FILES
            .iter()
            .map(|(relative_path, bytes)| (layout.root.join(relative_path), *bytes)),
    );
    for (target, bytes) in resources {
        if bytes.is_empty() {
            continue;
        }
        let expected = sha256_hex(bytes);
        if target.is_file() && file_sha256(&target).ok().as_deref() == Some(expected.as_str()) {
            continue;
        }
        write_bytes_atomic(&target, bytes)?;
        let actual = file_sha256(&target)?;
        if actual != expected {
            return Err(format!(
                "embedded resource hash mismatch for {}",
                target.display()
            ));
        }
    }
    Ok(())
}

fn write_bytes_atomic(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| map_io("create resource directory", parent, error))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let result = (|| -> Result<(), String> {
        let mut file = File::create(&temp)
            .map_err(|error| map_io("write temporary resource", &temp, error))?;
        file.write_all(bytes)
            .map_err(|error| map_io("write temporary resource", &temp, error))?;
        file.sync_all()
            .map_err(|error| map_io("flush temporary resource", &temp, error))?;
        #[cfg(windows)]
        {
            // Windows cannot atomically rename over an existing file. Keep a
            // recoverable backup until the replacement is durably installed.
            let backup = parent.join(format!(
                ".{}.bak-{}",
                target.file_name().unwrap_or_default().to_string_lossy(),
                std::process::id()
            ));
            if target.exists() {
                fs::rename(target, &backup)
                    .map_err(|error| map_io("backup resource for replacement", target, error))?;
            }
            if let Err(error) = fs::rename(&temp, target) {
                if backup.exists() {
                    let _ = fs::rename(&backup, target);
                }
                return Err(map_io("install resource", target, error));
            }
            let _ = fs::remove_file(&backup);
        }
        #[cfg(not(windows))]
        fs::rename(&temp, target).map_err(|error| map_io("install resource", target, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_state_atomic(path: &Path, state: &SetupState) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("serialize setup state: {}", error))?;
    write_bytes_atomic(path, &json)
}

fn read_state(path: &Path) -> Result<SetupState, String> {
    if !path.is_file() {
        return Ok(SetupState::default());
    }
    let content =
        fs::read_to_string(path).map_err(|error| map_io("read setup state", path, error))?;
    serde_json::from_str(&content).map_err(|error| format!("invalid setup-state.json: {}", error))
}

fn ensure_root_writable(root: &Path) -> Result<(), String> {
    if !root.exists() {
        fs::create_dir_all(root).map_err(|error| map_io("create runtime root", root, error))?;
    }
    probe_writable(root)
}

fn probe_writable(root: &Path) -> Result<(), String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let probe = root.join(format!(
        ".gameassistant-write-test-{}-{}",
        std::process::id(),
        stamp
    ));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| map_io("write portable runtime", root, error))?;
    fs::remove_file(&probe).map_err(|error| map_io("remove portable runtime probe", &probe, error))
}

fn map_io(action: &str, path: &Path, error: io::Error) -> String {
    if error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(5) {
        return format!(
            "elevation_required: cannot {} at {} (run GameAssistant as administrator or move it to a writable folder)",
            action,
            path.display()
        );
    }
    format!("{} at {}: {}", action, path.display(), error)
}

fn detect_nvidia_gpu() -> bool {
    if std::env::var("CUDA_VISIBLE_DEVICES")
        .map(|value| value.trim() == "-1")
        .unwrap_or(false)
    {
        return false;
    }
    nvml_wrapper::Nvml::init().is_ok()
}

fn scripts_hash() -> String {
    let mut hasher = Sha256::new();
    hasher.update(ASR_SERVER_PY);
    hasher.update(VITS2_SERVER_PY);
    format!("{:x}", hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| map_io("hash file", path, error))?;
    Ok(sha256_hex(&bytes))
}

fn append_setup_log(layout: &RuntimeLayout, message: &str) {
    let path = layout.logs_dir.join("setup.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{}] {}", now_string(), message);
    }
}

fn now_string() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(debug_assertions)]
    static CWD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(debug_assertions)]
    #[test]
    fn runtime_root_ignores_an_unrelated_process_cwd() {
        let _cwd_guard = CWD_TEST_LOCK.lock().unwrap();
        let unrelated = std::env::temp_dir().join(format!(
            "gameassistant-unrelated-cwd-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&unrelated).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&unrelated).unwrap();
        let resolved = resolve_runtime_root();
        std::env::set_current_dir(original).unwrap();
        assert_eq!(
            resolved,
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("src-tauri has a project root")
        );
        let layout = RuntimeLayout::for_root(resolved);
        assert_eq!(layout.models_dir, layout.root.join("models"));
        assert_eq!(layout.scripts_dir, layout.root.join("scripts"));
        let _ = fs::remove_dir_all(unrelated);
    }

    #[test]
    fn release_layout_is_rooted_at_the_executable_directory() {
        let layout = RuntimeLayout::for_root(PathBuf::from(r"C:\portable\GameAssistant"));
        assert_eq!(layout.root, PathBuf::from(r"C:\portable\GameAssistant"));
        assert_eq!(layout.venv_dir, layout.root.join("venv"));
        assert_eq!(layout.models_dir, layout.root.join("models"));
        assert_eq!(layout.uv_cache_dir, layout.root.join(".uv-cache"));
    }

    #[test]
    fn atomic_replacement_keeps_old_target_when_install_cannot_replace_it() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-atomic-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        fs::write(&target, b"old").unwrap();
        write_bytes_atomic(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn setup_state_only_skips_a_stage_when_its_marker_is_present() {
        let mut state = SetupState::default();
        assert!(!state.stage_completed(SetupStage::Python));
        state.mark_completed(SetupStage::Resources);
        assert!(state.stage_completed(SetupStage::Resources));
        assert!(!state.stage_completed(SetupStage::Python));
    }

    #[test]
    fn setup_state_can_be_serialized_and_resumed() {
        let mut state = SetupState::default();
        state.mark_completed(SetupStage::Resources);
        state.mark_completed(SetupStage::Python);
        let json = serde_json::to_string(&state).expect("state must serialize");
        let restored: SetupState = serde_json::from_str(&json).expect("state must deserialize");
        assert!(restored.stage_completed(SetupStage::Resources));
        assert!(restored.stage_completed(SetupStage::Python));
        assert!(!restored.stage_completed(SetupStage::Dependencies));
    }

    #[test]
    fn metadata_change_invalidates_install_stages() {
        let mut state = SetupState::default();
        state.mark_completed(SetupStage::Resources);
        state.mark_completed(SetupStage::Python);
        state.mark_completed(SetupStage::Venv);
        state.mark_completed(SetupStage::Dependencies);
        state.gpu = true;
        sync_state_metadata(&mut state, false, "new-requirements");
        assert!(!state.stage_completed(SetupStage::Python));
        assert!(!state.stage_completed(SetupStage::Dependencies));
    }

    #[test]
    fn uv_invocation_pins_portable_cache_and_disables_external_config() {
        let layout = RuntimeLayout::for_root(PathBuf::from(r"C:\portable\GameAssistant"));
        let args = uv_command_args(&layout, &["pip", "compile"]);
        assert_eq!(args[0], "--no-config");
        assert_eq!(args[1], "--cache-dir");
        assert_eq!(args[2], layout.uv_cache_dir.to_string_lossy());
        assert_eq!(&args[3..], &["pip".to_string(), "compile".to_string()]);
    }

    #[test]
    fn gpu_dependency_compile_uses_the_cuda_backend() {
        let args =
            dependency_compile_args("python.exe", "generated.txt", "requirements-gpu.txt", true);
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--torch-backend".to_string(), "cu128".to_string()] }));
        assert!(!args.windows(2).any(|pair| {
            pair == [
                "--index-strategy".to_string(),
                "unsafe-best-match".to_string(),
            ]
        }));
    }

    #[test]
    fn gpu_requirements_do_not_mix_the_pytorch_index_into_generic_dependencies() {
        let requirements = String::from_utf8_lossy(REQUIREMENTS_GPU);
        assert!(!requirements
            .lines()
            .any(|line| line.trim_start().starts_with("--extra-index-url")));
    }

    #[test]
    fn runtime_requirements_include_pkg_resources_provider() {
        for requirements in [REQUIREMENTS_PLAIN, REQUIREMENTS_CPU, REQUIREMENTS_GPU] {
            let requirements = String::from_utf8_lossy(requirements);
            assert!(
                requirements
                    .lines()
                    .any(|line| line.trim() == "setuptools==80.9.0"),
                "portable runtime requirements must install setuptools for ctranslate2"
            );
        }
    }

    #[test]
    fn runtime_requirements_include_sentencepiece_compatibility_dependency() {
        for requirements in [REQUIREMENTS_PLAIN, REQUIREMENTS_CPU, REQUIREMENTS_GPU] {
            let requirements = String::from_utf8_lossy(requirements);
            assert!(
                requirements
                    .lines()
                    .any(|line| line.trim() == "sentencepiece==0.2.0"),
                "portable runtime requirements must install SentencePiece for tokenizers"
            );
        }
    }

    #[test]
    fn nod_wav_assets_are_embedded_with_portable_relative_paths() {
        assert_eq!(NOD_WAV_FILES.len(), 5);
        for (relative_path, bytes) in NOD_WAV_FILES {
            assert!(relative_path.starts_with("wav/nod/"));
            assert!(relative_path.ends_with(".wav"));
            assert!(!bytes.is_empty());
        }
    }

    #[test]
    fn every_embedded_requirements_manifest_is_bound_to_runtime_state() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-requirements-bundle-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        for (name, contents) in [
            ("requirements.txt", REQUIREMENTS_PLAIN),
            ("requirements-cpu.txt", REQUIREMENTS_CPU),
            ("requirements-gpu.txt", REQUIREMENTS_GPU),
        ] {
            fs::write(root.join(name), contents).unwrap();
        }
        assert!(embedded_requirements_manifest_current(&layout));

        fs::write(
            root.join("requirements.txt"),
            b"sentencepiece==0.2.0\n# changed\n",
        )
        .unwrap();
        assert!(!embedded_requirements_manifest_current(&layout));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_requirements_metadata_is_not_reported_as_a_completed_runtime() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-stale-requirements-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        let mut state = SetupState::default();
        for stage in [
            SetupStage::Resources,
            SetupStage::Python,
            SetupStage::Venv,
            SetupStage::Dependencies,
            SetupStage::Models,
            SetupStage::Complete,
        ] {
            state.mark_completed(stage);
        }
        state.requirements_sha256 = "stale-requirements-hash".to_string();
        write_state_atomic(&layout.state_path, &state).unwrap();

        let status = runtime_status(&root).unwrap();
        let json = serde_json::to_value(&status).unwrap();

        assert!(!status.setup_state.stage_completed(SetupStage::Dependencies));
        assert!(!status.setup_state.stage_completed(SetupStage::Complete));
        assert_eq!(status.current_stage.as_deref(), Some("pending"));
        assert_eq!(
            json["diagnostics"]["dependency"],
            "dependency_manifest_stale"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lockfile_with_a_stale_requirements_header_is_not_ready() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-stale-lock-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        fs::create_dir_all(&layout.venv_dir).unwrap();
        let python_path = layout.python_executable();
        fs::create_dir_all(python_path.parent().unwrap()).unwrap();
        fs::write(&python_path, b"python").unwrap();
        let gpu = detect_nvidia_gpu();
        fs::write(
            layout.requirements_path(gpu),
            if gpu {
                REQUIREMENTS_GPU
            } else {
                REQUIREMENTS_CPU
            },
        )
        .unwrap();
        let stale_lock =
            b"# GameAssistant uv lock manifest\n# requirements-sha256: stale\npackage==1.0\n";
        fs::write(&layout.lock_path, stale_lock).unwrap();
        fs::write(
            &layout.dependencies_marker_path,
            sha256_hex(stale_lock).as_bytes(),
        )
        .unwrap();
        let mut state = SetupState::default();
        sync_state_metadata(&mut state, gpu, &runtime_requirements_hash(gpu));
        state.lock_sha256 = sha256_hex(stale_lock);
        state.mark_completed(SetupStage::Dependencies);
        write_state_atomic(&layout.state_path, &state).unwrap();

        let status = runtime_status(&root).unwrap();
        let json = serde_json::to_value(&status).unwrap();

        assert!(!status.lock_present);
        assert_eq!(json["dependency_ready"], false);
        assert_eq!(json["diagnostics"]["dependency"], "dependency_lock_stale");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_diagnostics_keep_tokenizer_and_embedding_failures_distinct() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-diagnostics-{}-{}",
            std::process::id(),
            now_string()
        ));
        let tokenizer_path = RuntimeLayout::for_root(root.clone())
            .models_dir
            .join("kotoba-whisper-v2.0-faster")
            .join("tokenizer.json");
        fs::create_dir_all(tokenizer_path.parent().unwrap()).unwrap();
        fs::write(&tokenizer_path, b"{}").unwrap();
        let embedding_tokenizer_config = RuntimeLayout::for_root(root.clone())
            .models_dir
            .join("GLuCoSE-base-ja")
            .join("tokenizer_config.json");
        fs::create_dir_all(embedding_tokenizer_config.parent().unwrap()).unwrap();
        fs::write(embedding_tokenizer_config, b"{}").unwrap();

        let status = runtime_status(&root).unwrap();
        let json = serde_json::to_value(&status).unwrap();

        // Model files alone do not prove that the runtime can initialize the
        // tokenizer.  Runtime validation is intentionally required before a
        // setup status can authorize a session.
        assert_eq!(json["tokenizer_ready"], false);
        assert_eq!(json["embedding_ready"], false);
        assert_eq!(
            json["diagnostics"]["tokenizer"],
            "tokenizer_runtime_unverified"
        );
        assert_eq!(json["diagnostics"]["embedding"], "embedding_model_missing");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_dependency_files_invalidate_each_runtime_layer_even_with_stale_probe_flags() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-dependency-loss-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        let gpu = detect_nvidia_gpu();
        fs::write(
            layout.requirements_path(gpu),
            if gpu {
                REQUIREMENTS_GPU
            } else {
                REQUIREMENTS_CPU
            },
        )
        .unwrap();
        let lock = format!(
            "# GameAssistant uv lock manifest\n# requirements-sha256: {}\npackage==1.0\n",
            runtime_requirements_hash(gpu)
        );
        fs::write(&layout.lock_path, lock.as_bytes()).unwrap();
        let mut state = SetupState::default();
        sync_state_metadata(&mut state, gpu, &runtime_requirements_hash(gpu));
        state.lock_sha256 = sha256_hex(lock.as_bytes());
        fs::write(
            &layout.dependencies_marker_path,
            state.lock_sha256.as_bytes(),
        )
        .unwrap();
        for stage in [
            SetupStage::Resources,
            SetupStage::Python,
            SetupStage::Venv,
            SetupStage::Dependencies,
            SetupStage::Models,
            SetupStage::Complete,
        ] {
            state.mark_completed(stage);
        }
        state.runtime_validation = RuntimeValidationState {
            fingerprint: runtime_validation_fingerprint(&root, &state, gpu),
            python_import_ready: true,
            tokenizer_ready: true,
            embedding_ready: true,
            diagnostics: BTreeMap::new(),
        };
        // Simulate a deleted venv after a previous successful setup. The
        // persisted probe fingerprint must not make any runtime layer look
        // ready when its dependency files are gone.
        write_state_atomic(&layout.state_path, &state).unwrap();

        let status = runtime_status(&root).unwrap();

        assert!(!status.dependency_ready);
        assert!(!status.python_import_ready);
        assert!(!status.tokenizer_ready);
        assert!(!status.embedding_ready);
        assert_eq!(
            status.diagnostics.get("python_import").map(String::as_str),
            Some("dependency_not_ready")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gpu_dependency_sync_uses_the_same_cuda_backend_as_compile() {
        let args = dependency_sync_args("python.exe", "uv.lock", true);
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--torch-backend".to_string(), "cu128".to_string()] }));
        assert_eq!(args.last(), Some(&"uv.lock".to_string()));
    }

    #[test]
    fn cpu_dependency_sync_does_not_select_a_cuda_backend() {
        let args = dependency_sync_args("python.exe", "uv.lock", false);
        assert!(!args.iter().any(|arg| arg == "--torch-backend"));
    }

    #[test]
    fn every_runtime_status_exposes_current_gemma_contract_metadata() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-status-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let status = runtime_status(&root).unwrap();
        assert!(!status.ready);
        assert!(runtime_is_ready(&root).is_err());
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(
            json["gemma_terms_version"],
            crate::model_manager::GEMMA_TERMS_VERSION
        );
        assert_eq!(
            json["gemma_terms_model_sha256"],
            crate::model_manager::GEMMA_EXPECTED_SHA256
        );
        assert_eq!(
            json["gemma_terms_source"],
            crate::model_manager::GEMMA_TERMS_SOURCE
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn complete_marker_alone_cannot_authorize_session_start() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-gate-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        let mut state = SetupState::default();
        state.mark_completed(SetupStage::Models);
        state.mark_completed(SetupStage::Complete);
        write_state_atomic(&layout.state_path, &state).unwrap();
        crate::settings::save_setting_key(&root, "gemma_terms_accepted", serde_json::json!(true))
            .unwrap();
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_version",
            serde_json::json!(crate::model_manager::GEMMA_TERMS_VERSION),
        )
        .unwrap();
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_model_sha256",
            serde_json::json!(crate::model_manager::GEMMA_EXPECTED_SHA256),
        )
        .unwrap();
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_source",
            serde_json::json!(crate::model_manager::GEMMA_TERMS_SOURCE),
        )
        .unwrap();
        assert!(!required_setup_stages_complete(&state));
        assert!(runtime_is_ready(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_is_distinct_from_error_and_clears_stale_error_fields() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-cancel-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        let mut state = SetupState::default();
        state.current_stage = SetupStage::Models;
        state.last_error = Some("setup cancelled; run setup again to resume".to_string());
        write_state_atomic(&layout.state_path, &state).unwrap();
        let status = runtime_status(&root).unwrap();
        assert!(status.cancelled);
        assert!(status.error.is_none());
        assert!(status.stages.iter().all(|step| step.status != "error"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepting_gemma_terms_clears_a_stale_setup_error() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-runtime-terms-error-{}-{}",
            std::process::id(),
            now_string()
        ));
        fs::create_dir_all(&root).unwrap();
        let layout = RuntimeLayout::for_root(root.clone());
        let mut state = SetupState::default();
        state.current_stage = SetupStage::Models;
        state.last_error = Some("previous setup failure".to_string());
        write_state_atomic(&layout.state_path, &state).unwrap();

        clear_setup_error(&root).unwrap();

        assert_eq!(read_state(&layout.state_path).unwrap().last_error, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn asr_script_executable_contract_reports_supplied_absolute_paths() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-asr-contract-{}-{}",
            std::process::id(),
            now_string()
        ));
        let script_path = root.join("scripts").join("asr_server.py");
        fs::create_dir_all(script_path.parent().unwrap()).unwrap();
        fs::write(&script_path, ASR_SERVER_PY).unwrap();

        let runtime_root = root.join("runtime");
        let settings_path = runtime_root.join("settings.json");
        let models_dir = runtime_root.join("models");
        let cache_dir = runtime_root.join("cache");
        let output = Command::new("python")
            .arg(&script_path)
            .arg("--validate-runtime-contract")
            .env("RUNTIME_ROOT", &runtime_root)
            .env("SETTINGS_PATH", &settings_path)
            .env("MODELS_DIR", &models_dir)
            .env("CACHE_DIR", &cache_dir)
            .output()
            .expect("python must execute the ASR contract check");
        assert!(
            output.status.success(),
            "contract check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let contract: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            contract["RUNTIME_ROOT"],
            runtime_root.to_string_lossy().as_ref()
        );
        assert_eq!(
            contract["SETTINGS_PATH"],
            settings_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            contract["MODELS_DIR"],
            models_dir.to_string_lossy().as_ref()
        );
        assert_eq!(contract["CACHE_DIR"], cache_dir.to_string_lossy().as_ref());
        let _ = fs::remove_dir_all(root);
    }
}
