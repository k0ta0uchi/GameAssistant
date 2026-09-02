use std::io::{BufRead, BufReader, Cursor};
use std::process::{Command, Stdio, Child};
use std::sync::Arc;
use std::thread;
use parking_lot::Mutex;
use hound::{WavSpec, WavWriter};
use candle_core::{Device, Tensor, IndexOp};
use candle_transformers::models::whisper::{Config, model::Whisper, audio};
use tokenizers::Tokenizer;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use futures::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use crate::session::normalize_kana;

use tokio::sync::oneshot;
use std::collections::HashMap;

type AsrCallback = Arc<dyn Fn(String, String, bool, Option<f64>) + Send + Sync>;

pub struct WhisperWsClient {
    audio_tx: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    cmd_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    child: Mutex<Option<Child>>,
    callback: Arc<Mutex<Option<AsrCallback>>>,
    pending_embeds: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<Vec<f32>>>>>>,
    is_started: AtomicBool,
}

impl WhisperWsClient {
    pub fn new() -> Self {
        Self {
            audio_tx: Mutex::new(None),
            cmd_tx: Mutex::new(None),
            child: Mutex::new(None),
            callback: Arc::new(Mutex::new(None)),
            pending_embeds: Arc::new(Mutex::new(HashMap::new())),
            is_started: AtomicBool::new(false),
        }
    }

    pub fn set_callback<F>(&self, on_result: F)
    where
        F: Fn(String, String, bool, Option<f64>) + Send + Sync + 'static,
    {
        *self.callback.lock() = Some(Arc::new(on_result));
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
        }).to_string();

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
        }).to_string();

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
        self.set_callback(on_result);

        if self.is_started.swap(true, Ordering::SeqCst) {
            // 既に起動済みの場合はコールバックの更新のみで即時有効化
            return Ok(());
        }

        self.stop_process_only();

        let root_dir = crate::resolve_project_root();

        // scripts/asr_server.py の探索
        let script_path = if root_dir.join("scripts").join("asr_server.py").exists() {
            root_dir.join("scripts").join("asr_server.py")
        } else if std::path::Path::new("C:/Workspace/GameAssistant/scripts/asr_server.py").exists() {
            std::path::PathBuf::from("C:/Workspace/GameAssistant/scripts/asr_server.py")
        } else {
            root_dir.join("scripts").join("asr_server.py")
        };

        // python.exe の探索
        let python_path = if root_dir.join("venv").join("Scripts").join("python.exe").exists() {
            root_dir.join("venv").join("Scripts").join("python.exe")
        } else if std::path::Path::new("C:/Workspace/GameAssistant/venv/Scripts/python.exe").exists() {
            std::path::PathBuf::from("C:/Workspace/GameAssistant/venv/Scripts/python.exe")
        } else {
            std::path::PathBuf::from("python")
        };

        if !script_path.exists() {
            eprintln!("[ERROR] [ASR] ASR server script not found at: {:?}", script_path);
            return Err(format!("ASR server script not found at: {:?}", script_path));
        }

        let models_dir = crate::model_manager::ModelManager::get_effective_models_dir(&root_dir, None);

        let mut cmd = Command::new(&python_path);
        cmd.arg(&script_path)
            .current_dir(&root_dir)
            .env("MODELS_DIR", models_dir.to_string_lossy().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn ASR server: {}", e))?;

        if let Some(stdout) = child.stdout.take() {
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().filter_map(|l| l.ok()) {
                    println!("[INFO] [ASR-Server] {}", line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().filter_map(|l| l.ok()) {
                    eprintln!("[STDERR] [ASR-Server] {}", line);
                }
            });
        }

        *self.child.lock() = Some(child);

        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();
        *self.audio_tx.lock() = Some(audio_tx);
        *self.cmd_tx.lock() = Some(cmd_tx);

        let callback_arc = self.callback.clone();
        let pending_embeds_arc = self.pending_embeds.clone();

        tauri::async_runtime::spawn(async move {
            let ws_url = "ws://127.0.0.1:18088/asr";
            let mut ws_stream = None;

            for attempt in 1..=40 {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                match connect_async(ws_url).await {
                    Ok((stream, _)) => {
                        println!("[INFO] [ASR-WS] Successfully connected to Faster-Whisper CUDA INT8 WebSocket server!");
                        ws_stream = Some(stream);
                        break;
                    }
                    Err(_) => {
                        if attempt % 8 == 0 {
                            println!("[INFO] [ASR-WS] Waiting for ASR WebSocket server to be ready (attempt {}/40)...", attempt);
                        }
                    }
                }
            }

            let ws_stream = match ws_stream {
                Some(s) => s,
                None => {
                    eprintln!("[ERROR] [ASR-WS] Failed to connect to Faster-Whisper ASR WebSocket server after 40 attempts (10s)");
                    return;
                }
            };

            let (mut ws_write, mut ws_read) = ws_stream.split();

            let send_task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(audio_data) = audio_rx.recv() => {
                            if ws_write.send(Message::Binary(audio_data)).await.is_err() {
                                break;
                            }
                        }
                        Some(cmd_str) = cmd_rx.recv() => {
                            if ws_write.send(Message::Text(cmd_str)).await.is_err() {
                                break;
                            }
                        }
                        else => break,
                    }
                }
            });

            while let Some(msg) = ws_read.next().await {
                match msg {
                    Ok(Message::Text(txt)) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) {
                            if let Some(msg_type) = val.get("type").and_then(|v| v.as_str()) {
                                if msg_type == "embed_res" {
                                    if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                                        if let Some(resp_tx) = pending_embeds_arc.lock().remove(id) {
                                            let mut vecs = Vec::new();
                                            if let Some(raw_arr) = val.get("vectors").and_then(|v| v.as_array()) {
                                                for row in raw_arr {
                                                    if let Some(arr) = row.as_array() {
                                                        let f_row: Vec<f32> = arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
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
                                let stream_name = val.get("stream").and_then(|v| v.as_str()).unwrap_or("mic").to_string();
                                let is_final = val.get("is_final").and_then(|v| v.as_bool()).unwrap_or(false);
                                let latency_ms = val.get("latency_ms").and_then(|v| v.as_f64());
                                if let Some(ref cb) = *callback_arc.lock() {
                                    cb(stream_name, text.to_string(), is_final, latency_ms);
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Err(e) => {
                        eprintln!("[ASR-WS] Read error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }

            send_task.abort();
        });

        Ok(())
    }

    /// WebSocket 接続が成功するまで非同期で待機して完了を返す
    pub async fn warmup(&self) -> Result<(), String> {
        if self.child.lock().is_none() {
            let _ = self.start(|_, _, _, _| {});
        }

        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Ok((_stream, _)) = connect_async("ws://127.0.0.1:18088/asr").await {
                return Ok(());
            }
        }
        Err("ASR WebSocket server warmup timed out".to_string())
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
    pub fn send_audio(&self, _stream: &str, samples: &[f32]) {
        if let Some(ref tx) = *self.audio_tx.lock() {
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let _ = tx.send(bytes);
        }
    }

    pub fn stop_process_only(&self) {
        *self.audio_tx.lock() = None;
        *self.cmd_tx.lock() = None;
        if let Some(mut child) = self.child.lock().take() {
            let pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(&["/F", "/T", "/PID", &pid.to_string()])
                    .creation_flags(0x08000000)
                    .output();
            }
        }
        kill_process_on_port(18088);
    }

    pub fn stop(&self) {
        self.is_started.store(false, Ordering::SeqCst);
        self.stop_process_only();
    }
}

/// 指定ポートをリッスンしている外部プロセス（ゾンビ Python 等）を強制終了
pub fn kill_process_on_port(port: u16) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(out) = Command::new("cmd")
            .args(&["/C", &format!("netstat -ano -p tcp | findstr :{}", port)])
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
                                let _ = Command::new("taskkill")
                                    .args(&["/F", "/T", "/PID", &pid.to_string()])
                                    .creation_flags(0x08000000)
                                    .output();
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
}

impl AsrEngine {
    pub fn new() -> Self {
        Self {
            cached_model: Arc::new(Mutex::new(None)),
            ws_client: Arc::new(WhisperWsClient::new()),
        }
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
            writer.finalize().map_err(|e| format!("WavWriter finalize error: {}", e))?;
        }

        Ok(cursor.into_inner())
    }

    /// Pure Rust Native Whisper モデルのロード（Kotoba-Whisper-v2.0-faster）
    fn load_native_model() -> Result<CandleWhisperModel, String> {
        let device = Device::Cpu;
        let local_candidates = [
            std::path::PathBuf::from("models/kotoba-whisper-v2.0-faster"),
            std::path::PathBuf::from("../models/kotoba-whisper-v2.0-faster"),
        ];

        let (config_path, tokenizer_path, weights_path) = if let Some(local_dir) = local_candidates.iter().find(|p| p.join("model.safetensors").exists()) {
            eprintln!("[ASR-Native] Found local Kotoba-Whisper-v2.0-faster model directory at: {:?}", local_dir);
            (
                local_dir.join("config.json"),
                local_dir.join("tokenizer.json"),
                local_dir.join("model.safetensors"),
            )
        } else {
            let repo_id = "kotoba-tech/kotoba-whisper-v2.0";
            eprintln!("[ASR-Native] Initializing Kotoba-Whisper-v2.0-faster from HuggingFace cache...");

            let api = hf_hub::api::sync::Api::new().map_err(|e| format!("HF Hub API init error: {}", e))?;
            let repo = api.model(repo_id.to_string());

            eprintln!("[ASR-Native] Fetching config, tokenizer, and safetensors weights for '{}'...", repo_id);
            let c_path = repo.get("config.json").map_err(|e| format!("Failed to get config.json for {}: {}", repo_id, e))?;
            let t_path = repo.get("tokenizer.json").map_err(|e| format!("Failed to get tokenizer.json for {}: {}", repo_id, e))?;
            let w_path = repo.get("model.safetensors").map_err(|e| format!("Failed to get model.safetensors for {}: {}", repo_id, e))?;
            (c_path, t_path, w_path)
        };

        let config_str = std::fs::read_to_string(&config_path).map_err(|e| format!("Failed to read config: {}", e))?;
        let config: Config = serde_json::from_str(&config_str).map_err(|e| format!("Failed to parse config: {}", e))?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        eprintln!("[ASR-Native] Loading weights from '{:?}' into memory...", weights_path);
        let vb = unsafe {
            candle_nn::VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)
                .map_err(|e| format!("Failed to load safetensors: {}", e))?
        };

        let model = Whisper::load(&vb, config.clone()).map_err(|e| format!("Failed to build whisper model: {}", e))?;
        
        let mel_bytes: &[u8] = if config.num_mel_bins == 128 {
            include_bytes!("../resources/melfilters128.bytes")
        } else {
            include_bytes!("../resources/melfilters.bytes")
        };

        let mut mel_filters = vec![0f32; mel_bytes.len() / 4];
        <byteorder::LittleEndian as byteorder::ByteOrder>::read_f32_into(mel_bytes, &mut mel_filters);

        eprintln!("[ASR-Native] Mel filters loaded from embedded binary ({} bins, {} floats)", config.num_mel_bins, mel_filters.len());
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
        if n_frames % 2 != 0 {
            padded.resize(padded.len() + hop_size, 0.0f32);
        }

        let mel = audio::pcm_to_mel(&m_ref.config, &padded, &m_ref.mel_filters);
        let mel_len = mel.len();
        let mel_frames = mel_len / m_ref.config.num_mel_bins;
        let mel_tensor = Tensor::from_vec(mel, (1, m_ref.config.num_mel_bins, mel_frames), &m_ref.device)
            .map_err(|e| format!("Tensor conversion error: {}", e))?;

        let mel_segment = if mel_frames > 3000 {
            mel_tensor.narrow(2, 0, 3000).map_err(|e| format!("Mel narrow error: {}", e))?
        } else {
            mel_tensor
        };

        let enc = m_ref.model.encoder.forward(&mel_segment, true)
            .map_err(|e| format!("Encoder forward error: {}", e))?;

        m_ref.model.reset_kv_cache();

        // 日本語言語指定トークン (<|ja|>: 50266) を確実に含める
        let sot_token = m_ref.tokenizer.token_to_id("<|startoftranscript|>").unwrap_or(50258);
        let ja_token = m_ref.tokenizer.token_to_id("<|ja|>").unwrap_or(50266);
        let transcribe_token = m_ref.tokenizer.token_to_id("<|transcribe|>").unwrap_or(50360);
        let notimestamps_token = m_ref.tokenizer.token_to_id("<|notimestamps|>").unwrap_or(50364);
        let eot_token = m_ref.tokenizer.token_to_id("<|endoftext|>").unwrap_or(50257);

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

            let ys = m_ref.model.decoder.forward(&token_tensor, &enc, true)
                .map_err(|e| format!("Decoder forward error: {}", e))?;
            let logits = m_ref.model.decoder.final_linear(&ys)
                .map_err(|e| format!("Decoder final linear error: {}", e))?;

            let (_, seq_len, _) = logits.dims3().map_err(|e| format!("Logits shape error: {}", e))?;
            let next_token_logits = logits.i((0, seq_len - 1, ..)).map_err(|e| format!("Logits index error: {}", e))?;
            let next_token = next_token_logits.argmax(0).map_err(|e| format!("Argmax error: {}", e))?
                .to_scalar::<u32>().map_err(|e| format!("Scalar read error: {}", e))?;

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
            m_ref.tokenizer.decode(&generated_tokens, true)
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
            eprintln!("[ASR-Native] Transcribed ({} tokens): '{}'", generated_tokens.len(), clean);
        }

        Ok(clean)
    }

    /// ウェイクワードが含まれているか判定
    pub fn check_wake_word(text: &str, custom_wake_words: &[String]) -> (bool, String) {
        let norm_input = normalize_kana(text);

        for ww in custom_wake_words {
            let norm_ww = normalize_kana(ww);
            if !norm_ww.is_empty() && norm_input.contains(&norm_ww) {
                // ウェイクワード部分を除去した残りの発話テキストを抽出
                let after_ww = text.replace(ww, "");
                return (true, after_ww.trim().to_string());
            }
        }

        // デフォルトウェイクワード
        let default_wws = ["ねえぐり", "ねぐり", "ネグリ", "アシスタント", "ヘイぐり"];
        for dww in default_wws {
            let norm_dww = normalize_kana(dww);
            if norm_input.contains(&norm_dww) {
                let after = text.replace(dww, "");
                return (true, after.trim().to_string());
            }
        }

        (false, text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        println!("WAV spec: sample_rate={}, channels={}, bits_per_sample={}", spec.sample_rate, spec.channels, spec.bits_per_sample);

        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
                reader.samples::<i32>().map(|s| s.unwrap() as f32 / max_val).collect()
            }
            hound::SampleFormat::Float => {
                reader.samples::<f32>().map(|s| s.unwrap()).collect()
            }
        };

        // モノラル 16kHz にリサンプリング
        let mono_samples = if spec.channels > 1 {
            samples.chunks(spec.channels as usize).map(|ch| ch[0]).collect()
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
            println!("[TEST-CALLBACK] Stream: {}, Text: '{}', Final: {}", stream, text, is_final);
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
                    reader.samples::<i32>().map(|s| s.unwrap() as f32 / max_val).collect()
                }
                hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
            };
            let mono_samples = if spec.channels > 1 {
                samples.chunks(spec.channels as usize).map(|ch| ch[0]).collect()
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
                if let Ok((_stream, text, is_final)) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
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
}
