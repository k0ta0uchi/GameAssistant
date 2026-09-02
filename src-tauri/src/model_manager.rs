use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hf_repo: String,
    pub category: String, // "ASR" | "Embedding" | "LLM" | "Other"
    pub required: bool,
    pub estimated_size_bytes: u64,
    pub check_files: Vec<String>, // このファイル群が存在すればインストール済みと判定
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hf_repo: String,
    pub category: String,
    pub required: bool,
    pub estimated_size_bytes: u64,
    pub is_installed: bool,
    pub actual_size_bytes: u64,
    pub local_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgressEvent {
    pub model_id: String,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub speed_mbps: f64,
    pub percent: f64,
    pub status: String, // "downloading" | "completed" | "error" | "cancelled"
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HfTreeItem {
    r#type: String,
    path: String,
    size: Option<u64>,
}

pub fn get_defined_models() -> Vec<ModelDef> {
    vec![
        ModelDef {
            id: "kotoba-whisper-v2.0-faster".to_string(),
            name: "Kotoba-Whisper v2.0 (CUDA INT8)".to_string(),
            description: "日本語特化の超高速・高精度リアルタイム音声認識モデル (Faster-Whisper CTranslate2)".to_string(),
            hf_repo: "kotoba-tech/kotoba-whisper-v2.0-faster".to_string(),
            category: "ASR".to_string(),
            required: true,
            estimated_size_bytes: 1_515_000_000,
            check_files: vec![
                "model.bin".to_string(),
                "config.json".to_string(),
                "tokenizer.json".to_string(),
                "vocabulary.json".to_string(),
            ],
        },
        ModelDef {
            id: "GLuCoSE-base-ja".to_string(),
            name: "GLuCoSE-base-ja (Embedding)".to_string(),
            description: "日本語セマンティック長期記憶・ベクトル検索用の高精度埋め込みモデル (768次元)".to_string(),
            hf_repo: "pkshatech/GLuCoSE-base-ja".to_string(),
            category: "Embedding".to_string(),
            required: true,
            estimated_size_bytes: 535_000_000,
            check_files: vec![
                "pytorch_model.bin".to_string(),
                "config.json".to_string(),
                "tokenizer_config.json".to_string(),
            ],
        },
        ModelDef {
            id: "gemma-3-1b-it-Q4_K_S.gguf".to_string(),
            name: "Gemma 3 1B IT (GGUF Q4_K_S)".to_string(),
            description: "Google製 超軽量・高品質ローカルLLM (llama-cpp-python / GGUF量子化モデル)".to_string(),
            hf_repo: "bartowski/gemma-3-1b-it-GGUF".to_string(),
            category: "LLM".to_string(),
            required: false,
            estimated_size_bytes: 780_000_000,
            check_files: vec![
                "gemma-3-1b-it-Q4_K_S.gguf".to_string(),
            ],
        },
        ModelDef {
            id: "sup-simcse-ja-base".to_string(),
            name: "Sup-SimCSE-ja-base".to_string(),
            description: "日本語テキスト類似度判定・発話クラスタリング用BERTモデル".to_string(),
            hf_repo: "llm-book/bert-base-japanese-v2-sup-simcse-ja".to_string(),
            category: "Embedding".to_string(),
            required: false,
            estimated_size_bytes: 450_000_000,
            check_files: vec![
                "pytorch_model.bin".to_string(),
                "config.json".to_string(),
            ],
        },
    ]
}

pub struct ModelManager {
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl ModelManager {
    pub fn new() -> Self {
        Self {
            cancels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// モデル保存先ディレクトリの取得
    pub fn get_effective_models_dir(root_dir: &Path, custom_dir: Option<String>) -> PathBuf {
        if let Some(dir) = custom_dir {
            if !dir.trim().is_empty() {
                let p = PathBuf::from(dir.trim());
                if p.is_absolute() {
                    return p;
                } else {
                    return root_dir.join(p);
                }
            }
        }
        
        let st = crate::settings::load_settings_file(root_dir);
        if let Some(st_dir) = st.get("models_dir").and_then(|v| v.as_str()) {
            if !st_dir.trim().is_empty() {
                let p = PathBuf::from(st_dir.trim());
                if p.is_absolute() {
                    return p;
                } else {
                    return root_dir.join(p);
                }
            }
        }

        root_dir.join("models")
    }

    /// 全モデルのインストール状態をスキャン
    pub fn scan_models_status(root_dir: &Path, custom_dir: Option<String>) -> Vec<ModelStatus> {
        let models_dir = Self::get_effective_models_dir(root_dir, custom_dir);
        let defs = get_defined_models();
        let mut results = Vec::new();

        for def in defs {
            let model_target_path = if def.id.ends_with(".gguf") {
                models_dir.join(&def.id)
            } else {
                models_dir.join(&def.id)
            };

            let mut is_installed = false;
            let mut actual_size_bytes = 0;

            if def.id.ends_with(".gguf") {
                if model_target_path.exists() && model_target_path.is_file() {
                    if let Ok(meta) = fs::metadata(&model_target_path) {
                        actual_size_bytes = meta.len();
                        if actual_size_bytes >= (def.estimated_size_bytes / 2) {
                            is_installed = true;
                        }
                    }
                }
            } else if model_target_path.exists() && model_target_path.is_dir() {
                let mut found_all_checks = true;
                for check in &def.check_files {
                    let cp = model_target_path.join(check);
                    if !cp.exists() {
                        // model.bin / model.safetensors のどちらかがあればOKとする特別対応
                        if check == "model.bin" && model_target_path.join("model.safetensors").exists() {
                            continue;
                        }
                        if check == "pytorch_model.bin" && model_target_path.join("model.safetensors").exists() {
                            continue;
                        }
                        found_all_checks = false;
                        break;
                    }
                }

                if found_all_checks {
                    is_installed = true;
                }

                // ディレクトリの合計サイズを計算
                if let Ok(entries) = fs::read_dir(&model_target_path) {
                    for entry in entries.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_file() {
                                actual_size_bytes += meta.len();
                            }
                        }
                    }
                }
            }

            results.push(ModelStatus {
                id: def.id,
                name: def.name,
                description: def.description,
                hf_repo: def.hf_repo,
                category: def.category,
                required: def.required,
                estimated_size_bytes: def.estimated_size_bytes,
                is_installed,
                actual_size_bytes,
                local_path: model_target_path.to_string_lossy().to_string(),
            });
        }

        results
    }

    /// ダウンロードのキャンセル
    pub fn cancel_download(&self, model_id: &str) {
        if let Some(flag) = self.cancels.lock().get(model_id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Hugging Face からのモデルダウンロード実行 (マルチエンドポイント・ストリーミング)
    pub async fn download_model(
        &self,
        app: AppHandle,
        root_dir: PathBuf,
        model_id: String,
        custom_dir: Option<String>,
    ) -> Result<(), String> {
        let defs = get_defined_models();
        let def = defs
            .into_iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("Unknown model id: {}", model_id))?;

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancels.lock().insert(model_id.clone(), cancel_flag.clone());

        let models_dir = Self::get_effective_models_dir(&root_dir, custom_dir);
        fs::create_dir_all(&models_dir).map_err(|e| format!("Failed to create models dir: {}", e))?;

        let emit_progress = |status: &str, cur: u64, tot: u64, speed: f64, pct: f64, err: Option<String>| {
            let _ = app.emit(
                "download_progress",
                DownloadProgressEvent {
                    model_id: model_id.clone(),
                    current_bytes: cur,
                    total_bytes: tot,
                    speed_mbps: speed,
                    percent: pct,
                    status: status.to_string(),
                    error_message: err,
                },
            );
        };

        emit_progress("downloading", 0, def.estimated_size_bytes, 0.0, 0.0, None);

        let target_dir = if def.id.ends_with(".gguf") {
            models_dir.clone()
        } else {
            let td = models_dir.join(&def.id);
            fs::create_dir_all(&td).map_err(|e| format!("Failed to create target dir: {}", e))?;
            td
        };

        // ダウンロード対象のファイルリストを取得
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;

        // 1. GGUF 単体ファイルの場合
        if def.id.ends_with(".gguf") {
            let filename = &def.id;
            let target_file = target_dir.join(filename);
            let part_file = target_dir.join(format!("{}.part", filename));

            let urls = vec![
                format!("https://huggingface.co/{}/resolve/main/{}", def.hf_repo, filename),
                format!("https://hf-mirror.com/{}/resolve/main/{}", def.hf_repo, filename),
            ];

            let mut downloaded = false;
            for url in urls {
                if cancel_flag.load(Ordering::SeqCst) {
                    emit_progress("cancelled", 0, def.estimated_size_bytes, 0.0, 0.0, None);
                    return Ok(());
                }

                match download_single_file_streaming(
                    &client,
                    &url,
                    &part_file,
                    &target_file,
                    &cancel_flag,
                    &emit_progress,
                )
                .await
                {
                    Ok(_) => {
                        downloaded = true;
                        break;
                    }
                    Err(e) => {
                        println!("[WARN] Download failed from {}: {}. Trying fallback...", url, e);
                    }
                }
            }

            if !downloaded {
                emit_progress(
                    "error",
                    0,
                    def.estimated_size_bytes,
                    0.0,
                    0.0,
                    Some("All download mirrors failed".to_string()),
                );
                return Err("Failed to download model from all mirrors".to_string());
            }

            emit_progress("completed", def.estimated_size_bytes, def.estimated_size_bytes, 0.0, 100.0, None);
            self.cancels.lock().remove(&model_id);
            return Ok(());
        }

        // 2. ディレクトリモデル (Hugging Face Tree API から取得)
        let tree_urls = vec![
            format!("https://huggingface.co/api/models/{}/tree/main", def.hf_repo),
            format!("https://hf-mirror.com/api/models/{}/tree/main", def.hf_repo),
        ];

        let mut files_to_download: Vec<(String, u64)> = Vec::new();

        for tree_url in tree_urls {
            if let Ok(resp) = client.get(&tree_url).send().await {
                if resp.status().is_success() {
                    if let Ok(items) = resp.json::<Vec<HfTreeItem>>().await {
                        for it in items {
                            if it.r#type == "file" {
                                let sz = it.size.unwrap_or(0);
                                files_to_download.push((it.path, sz));
                            }
                        }
                        if !files_to_download.is_empty() {
                            break;
                        }
                    }
                }
            }
        }

        // Tree API から取れなかった場合は check_files をフォールバック
        if files_to_download.is_empty() {
            for cf in &def.check_files {
                files_to_download.push((cf.clone(), 0));
            }
        }

        let total_est_bytes: u64 = if files_to_download.iter().map(|(_, s)| *s).sum::<u64>() > 0 {
            files_to_download.iter().map(|(_, s)| *s).sum()
        } else {
            def.estimated_size_bytes
        };

        let mut overall_downloaded: u64 = 0;
        let start_time = Instant::now();

        for (rel_path, file_size) in files_to_download {
            if cancel_flag.load(Ordering::SeqCst) {
                emit_progress("cancelled", overall_downloaded, total_est_bytes, 0.0, 0.0, None);
                return Ok(());
            }

            let file_target = target_dir.join(&rel_path);
            let file_part = target_dir.join(format!("{}.part", rel_path));

            if let Some(parent) = file_target.parent() {
                let _ = fs::create_dir_all(parent);
            }

            let mirrors = vec![
                format!("https://huggingface.co/{}/resolve/main/{}", def.hf_repo, rel_path),
                format!("https://hf-mirror.com/{}/resolve/main/{}", def.hf_repo, rel_path),
            ];

            let mut file_ok = false;
            for url in mirrors {
                if cancel_flag.load(Ordering::SeqCst) {
                    emit_progress("cancelled", overall_downloaded, total_est_bytes, 0.0, 0.0, None);
                    return Ok(());
                }

                let cur_base = overall_downloaded;
                let on_chunk = |chunk_len: u64| {
                    let total_cur = cur_base + chunk_len;
                    let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
                    let speed = (total_cur as f64 / 1_048_576.0) / elapsed;
                    let pct = ((total_cur as f64 / total_est_bytes as f64) * 100.0).min(99.9);
                    emit_progress("downloading", total_cur, total_est_bytes, speed, pct, None);
                };

                match download_file_chunked(&client, &url, &file_part, &file_target, &cancel_flag, on_chunk).await {
                    Ok(bytes) => {
                        overall_downloaded += if file_size > 0 { file_size } else { bytes };
                        file_ok = true;
                        break;
                    }
                    Err(e) => {
                        println!("[WARN] Failed to download {} from {}: {}. Trying next mirror...", rel_path, url, e);
                    }
                }
            }

            if !file_ok {
                println!("[WARN] Optional/Required file {} could not be fetched", rel_path);
            }
        }

        emit_progress("completed", total_est_bytes, total_est_bytes, 0.0, 100.0, None);
        self.cancels.lock().remove(&model_id);
        Ok(())
    }
}

async fn download_single_file_streaming<F>(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    target_path: &Path,
    cancel_flag: &AtomicBool,
    emit_progress: &F,
) -> Result<u64, String>
where
    F: Fn(&str, u64, u64, f64, f64, Option<String>),
{
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP error status: {}", resp.status()));
    }

    let total_bytes = resp.content_length().unwrap_or(0);
    let mut file = File::create(part_path).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let start = Instant::now();

    while let Some(chunk_res) = stream.next().await {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = fs::remove_file(part_path);
            return Ok(0);
        }

        let chunk = chunk_res.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        let elapsed = start.elapsed().as_secs_f64().max(0.001);
        let speed = (downloaded as f64 / 1_048_576.0) / elapsed;
        let pct = if total_bytes > 0 {
            (downloaded as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };

        emit_progress("downloading", downloaded, total_bytes, speed, pct, None);
    }

    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    fs::rename(part_path, target_path).map_err(|e| e.to_string())?;
    Ok(downloaded)
}

async fn download_file_chunked<F>(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    target_path: &Path,
    cancel_flag: &AtomicBool,
    on_chunk: F,
) -> Result<u64, String>
where
    F: Fn(u64),
{
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP error: {}", resp.status()));
    }

    let mut file = File::create(part_path).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk_res) = stream.next().await {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = fs::remove_file(part_path);
            return Ok(0);
        }

        let chunk = chunk_res.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        on_chunk(downloaded);
    }

    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    fs::rename(part_path, target_path).map_err(|e| e.to_string())?;
    Ok(downloaded)
}
