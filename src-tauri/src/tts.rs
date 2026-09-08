use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const NOD_INDICES: [u8; 5] = [0, 1, 2, 4, 5];
const AUDIO_THREAD_STARTING: u8 = 0;
const AUDIO_THREAD_READY: u8 = 1;
const AUDIO_THREAD_STOPPED: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodPlaybackError {
    MissingAsset { index: u8 },
    InvalidAsset { index: u8 },
    AssetReadFailed { index: u8 },
    AudioThreadUnavailable,
    AudioCommandSendFailed,
    AudioThreadResponseFailed,
    PlaybackStopped,
    PlaybackSuperseded,
    AudioPlaybackFailed,
}

impl NodPlaybackError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingAsset { .. } => "nod_asset_missing",
            Self::InvalidAsset { .. } => "nod_asset_invalid",
            Self::AssetReadFailed { .. } => "nod_asset_read_failed",
            Self::AudioThreadUnavailable => "audio_thread_unavailable",
            Self::AudioCommandSendFailed => "audio_command_send_failed",
            Self::AudioThreadResponseFailed => "audio_thread_response_failed",
            Self::PlaybackStopped => "nod_stopped",
            Self::PlaybackSuperseded => "audio_playback_superseded",
            Self::AudioPlaybackFailed => "audio_playback_failed",
        }
    }
}

impl std::fmt::Display for NodPlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAsset { index } => write!(formatter, "nod asset {}.wav is missing", index),
            Self::InvalidAsset { index } => write!(formatter, "nod asset {}.wav is invalid", index),
            Self::AssetReadFailed { index } => {
                write!(formatter, "nod asset {}.wav could not be read", index)
            }
            Self::AudioThreadUnavailable => {
                formatter.write_str("audio output thread is unavailable")
            }
            Self::AudioCommandSendFailed => formatter.write_str("audio command could not be sent"),
            Self::AudioThreadResponseFailed => {
                formatter.write_str("audio output thread did not respond")
            }
            Self::PlaybackStopped => formatter.write_str("audio playback was stopped"),
            Self::PlaybackSuperseded => formatter.write_str("audio playback was superseded"),
            Self::AudioPlaybackFailed => formatter.write_str("audio playback failed"),
        }
    }
}

impl std::error::Error for NodPlaybackError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsSettings {
    pub tts_engine: String,    // "voicevox" | "style_bert_vits2" | "gemini"
    pub speaker_id: i32,       // VOICEVOX speaker id (default: 46)
    pub vits2_speaker_id: i32, // Style-Bert-VITS2 speaker id (default: 0)
    pub voicevox_url: String,  // default "http://127.0.0.1:50021"
}

impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            tts_engine: "voicevox".to_string(),
            speaker_id: 46,
            vits2_speaker_id: 0,
            voicevox_url: "http://127.0.0.1:50021".to_string(),
        }
    }
}

enum AudioCommand {
    PlayWav(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    Stop,
}

pub struct TtsManager {
    client: reqwest::Client,
    tx: mpsc::UnboundedSender<AudioCommand>,
    is_speaking: Arc<AtomicBool>,
    audio_thread_state: Arc<std::sync::atomic::AtomicU8>,
}

impl Default for TtsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AudioCommand>();
        let is_speaking = Arc::new(AtomicBool::new(false));
        let is_speaking_thread = is_speaking.clone();
        let audio_thread_state = Arc::new(std::sync::atomic::AtomicU8::new(AUDIO_THREAD_STARTING));
        let audio_thread_state_worker = audio_thread_state.clone();

        // 専用のオーディオ再生ワーカースレッド（スレッド内で OutputStream を所有）
        std::thread::spawn(move || {
            let stream_res = OutputStream::try_default();
            let (_stream, stream_handle) = match stream_res {
                Ok(stream) => stream,
                Err(_) => {
                    audio_thread_state_worker.store(AUDIO_THREAD_STOPPED, Ordering::SeqCst);
                    eprintln!("[TTS] Failed to initialize default output stream in audio thread");
                    return;
                }
            };
            audio_thread_state_worker.store(AUDIO_THREAD_READY, Ordering::SeqCst);
            let mut current_sink: Option<Sink> = None;
            let mut pending_command: Option<AudioCommand> = None;

            while let Some(cmd) = pending_command.take().or_else(|| rx.blocking_recv()) {
                match cmd {
                    AudioCommand::Stop => {
                        if let Some(sink) = current_sink.take() {
                            sink.stop();
                        }
                        is_speaking_thread.store(false, Ordering::SeqCst);
                    }
                    AudioCommand::PlayWav(wav_bytes, reply) => {
                        // 既存再生の停止
                        if let Some(sink) = current_sink.take() {
                            sink.stop();
                        }

                        match Sink::try_new(&stream_handle) {
                            Ok(sink) => {
                                let cursor = Cursor::new(wav_bytes);
                                match Decoder::new(cursor) {
                                    Ok(source) => {
                                        sink.append(source);
                                        current_sink = Some(sink);
                                        is_speaking_thread.store(true, Ordering::SeqCst);
                                        let mut interrupted = false;
                                        loop {
                                            let Some(active_sink) = current_sink.as_ref() else {
                                                break;
                                            };
                                            if active_sink.empty() {
                                                break;
                                            }
                                            match rx.try_recv() {
                                                Ok(AudioCommand::Stop) => {
                                                    if let Some(active_sink) = current_sink.take() {
                                                        active_sink.stop();
                                                    }
                                                    interrupted = true;
                                                    break;
                                                }
                                                Ok(next @ AudioCommand::PlayWav(_, _)) => {
                                                    if let Some(active_sink) = current_sink.take() {
                                                        active_sink.stop();
                                                    }
                                                    pending_command = Some(next);
                                                    interrupted = true;
                                                    break;
                                                }
                                                Err(mpsc::error::TryRecvError::Empty) => {
                                                    std::thread::sleep(Duration::from_millis(10));
                                                }
                                                Err(mpsc::error::TryRecvError::Disconnected) => {
                                                    if let Some(active_sink) = current_sink.take() {
                                                        active_sink.stop();
                                                    }
                                                    is_speaking_thread
                                                        .store(false, Ordering::SeqCst);
                                                    let _ = reply.send(Err(
                                                        "Audio output thread disconnected"
                                                            .to_string(),
                                                    ));
                                                    return;
                                                }
                                            }
                                        }
                                        current_sink.take();
                                        is_speaking_thread.store(false, Ordering::SeqCst);
                                        if !interrupted {
                                            let _ = reply.send(Ok(()));
                                        } else if pending_command.is_some() {
                                            // A new command superseded this
                                            // playback. Report that boundary
                                            // so nod diagnostics can tell an
                                            // audio conflict from success.
                                            let _ = reply
                                                .send(Err("Audio playback superseded".to_string()));
                                        } else {
                                            // Stop also ends the current
                                            // playback, but callers must be
                                            // able to distinguish cancellation
                                            // from a completed sound.
                                            let _ = reply
                                                .send(Err("Audio playback stopped".to_string()));
                                        }
                                    }
                                    Err(e) => {
                                        is_speaking_thread.store(false, Ordering::SeqCst);
                                        let _ = reply.send(Err(format!("Decode error: {}", e)));
                                    }
                                }
                            }
                            Err(e) => {
                                is_speaking_thread.store(false, Ordering::SeqCst);
                                let _ = reply.send(Err(format!("Sink error: {}", e)));
                            }
                        }
                    }
                }
            }
            audio_thread_state_worker.store(AUDIO_THREAD_STOPPED, Ordering::SeqCst);
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            client,
            tx,
            is_speaking,
            audio_thread_state,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.is_speaking.load(Ordering::SeqCst)
    }

    pub fn stop_playback(&self) {
        let _ = self.tx.send(AudioCommand::Stop);
        self.is_speaking.store(false, Ordering::SeqCst);
    }

    /// 音声合成を行って WAV バイト列を取得する
    pub async fn synthesize(&self, text: &str, settings: &TtsSettings) -> Result<Vec<u8>, String> {
        let clean_text = text.trim();
        if clean_text.is_empty() {
            return Err("Empty text".to_string());
        }

        let base_url = if settings.voicevox_url.is_empty() {
            "http://127.0.0.1:50021"
        } else {
            settings.voicevox_url.trim_end_matches('/')
        };

        match settings.tts_engine.as_str() {
            "style_bert_vits2" => {
                let query_url = format!(
                    "{}/audio_query?text={}&speaker={}",
                    base_url,
                    urlencoding::encode(clean_text),
                    settings.vits2_speaker_id
                );
                let query_res = self
                    .client
                    .post(&query_url)
                    .send()
                    .await
                    .map_err(|e| format!("Style-Bert-VITS2 audio_query error: {}", e))?;
                let query_json: serde_json::Value = query_res
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse query JSON: {}", e))?;

                let synth_url = format!(
                    "{}/synthesis?speaker={}",
                    base_url, settings.vits2_speaker_id
                );
                let synth_res = self
                    .client
                    .post(&synth_url)
                    .json(&query_json)
                    .send()
                    .await
                    .map_err(|e| format!("Style-Bert-VITS2 synthesis error: {}", e))?;
                let wav_bytes = synth_res
                    .bytes()
                    .await
                    .map_err(|e| format!("Failed to read WAV bytes: {}", e))?;
                Ok(wav_bytes.to_vec())
            }
            _ => {
                let query_url = format!(
                    "{}/audio_query?text={}&speaker={}",
                    base_url,
                    urlencoding::encode(clean_text),
                    settings.speaker_id
                );
                let query_res = self
                    .client
                    .post(&query_url)
                    .send()
                    .await
                    .map_err(|e| format!("VOICEVOX audio_query error: {}", e))?;
                let query_json: serde_json::Value = query_res
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse VOICEVOX query: {}", e))?;

                let synth_url = format!("{}/synthesis?speaker={}", base_url, settings.speaker_id);
                let synth_res = self
                    .client
                    .post(&synth_url)
                    .json(&query_json)
                    .send()
                    .await
                    .map_err(|e| format!("VOICEVOX synthesis error: {}", e))?;
                let wav_bytes = synth_res
                    .bytes()
                    .await
                    .map_err(|e| format!("Failed to read WAV bytes: {}", e))?;
                Ok(wav_bytes.to_vec())
            }
        }
    }

    /// WAV バイト列をオーディオ再生スレッドに送信して再生
    pub async fn play_wav(&self, wav_bytes: Vec<u8>) -> Result<(), String> {
        self.play_wav_for_nod(wav_bytes)
            .await
            .map_err(|error| error.to_string())
    }

    async fn play_wav_for_nod(&self, wav_bytes: Vec<u8>) -> Result<(), NodPlaybackError> {
        if self.audio_thread_state.load(Ordering::SeqCst) == AUDIO_THREAD_STOPPED {
            return Err(NodPlaybackError::AudioThreadUnavailable);
        }

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(AudioCommand::PlayWav(wav_bytes, reply_tx))
            .map_err(|_| NodPlaybackError::AudioCommandSendFailed)?;

        let result = reply_rx.await.map_err(|_| {
            if self.audio_thread_state.load(Ordering::SeqCst) == AUDIO_THREAD_STOPPED {
                NodPlaybackError::AudioThreadUnavailable
            } else {
                NodPlaybackError::AudioThreadResponseFailed
            }
        })?;
        result.map_err(|error| match error.as_str() {
            "Audio playback stopped" => NodPlaybackError::PlaybackStopped,
            "Audio playback superseded" => NodPlaybackError::PlaybackSuperseded,
            _ => NodPlaybackError::AudioPlaybackFailed,
        })
    }

    /// テキストの音声合成から再生まで一括実行
    pub async fn speak(&self, text: &str, settings: &TtsSettings) -> Result<(), String> {
        let wav = self.synthesize(text, settings).await?;
        self.play_wav(wav).await
    }

    /// 相槌（wav/nod/0.wav, 1.wav, 2.wav, 4.wav, 5.wav）をランダム再生する
    pub async fn play_random_nod(&self, root_dir: &Path) -> Result<(), NodPlaybackError> {
        validate_nod_assets(root_dir).await?;

        let idx = NOD_INDICES[rand::random::<usize>() % NOD_INDICES.len()];
        let nod_path = root_dir
            .join("wav")
            .join("nod")
            .join(format!("{}.wav", idx));
        let bytes = tokio::fs::read(&nod_path)
            .await
            .map_err(|_| NodPlaybackError::AssetReadFailed { index: idx })?;

        self.play_wav_for_nod(bytes)
            .await
            .map_err(|error| match error {
                NodPlaybackError::AudioCommandSendFailed => {
                    NodPlaybackError::AudioCommandSendFailed
                }
                NodPlaybackError::AudioThreadUnavailable => {
                    NodPlaybackError::AudioThreadUnavailable
                }
                NodPlaybackError::AudioThreadResponseFailed => {
                    NodPlaybackError::AudioThreadResponseFailed
                }
                NodPlaybackError::PlaybackStopped => NodPlaybackError::PlaybackStopped,
                NodPlaybackError::PlaybackSuperseded => NodPlaybackError::PlaybackSuperseded,
                _ => NodPlaybackError::AudioPlaybackFailed,
            })
    }
}

/// Validate the complete nod asset contract before a random asset is chosen.
/// Errors intentionally contain only a stable asset index, never the runtime
/// root path, because portable roots can include usernames or other secrets.
pub async fn validate_nod_assets(root_dir: &Path) -> Result<(), NodPlaybackError> {
    let nod_dir = root_dir.join("wav").join("nod");
    for index in NOD_INDICES {
        let path = nod_dir.join(format!("{}.wav", index));
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| NodPlaybackError::MissingAsset { index })?;
        if !metadata.is_file() {
            return Err(NodPlaybackError::InvalidAsset { index });
        }
        tokio::fs::read(&path)
            .await
            .map_err(|_| NodPlaybackError::AssetReadFailed { index })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nod_wav_files_exist() {
        let Some(root) = std::env::var_os("GAMEASSISTANT_DISTRIBUTION_ROOT") else {
            // Source/unit-test CI may intentionally omit shipped audio. The
            // fixture contract below still exercises the complete file set;
            // package validation opts in with the distribution root.
            return;
        };
        validate_nod_assets(std::path::Path::new(&root))
            .await
            .expect("distributed nod wav assets must be complete");
    }

    #[tokio::test]
    async fn nod_asset_fixture_contains_every_required_index() {
        let root = std::env::temp_dir().join(format!(
            "ga-nod-fixture-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let nod_dir = root.join("wav").join("nod");
        std::fs::create_dir_all(&nod_dir).expect("create nod fixture");
        for index in NOD_INDICES {
            std::fs::write(nod_dir.join(format!("{}.wav", index)), b"fixture")
                .expect("write nod fixture");
        }

        validate_nod_assets(&root)
            .await
            .expect("fixture must satisfy the nod asset contract");
    }

    #[tokio::test]
    async fn missing_nod_assets_are_reported_as_an_error() {
        let root = std::env::temp_dir().join(format!(
            "ga-missing-nod-assets-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let manager = TtsManager::new();

        let result = manager.play_random_nod(&root).await;

        assert!(
            result.is_err(),
            "missing nod assets must not be silent success"
        );
    }
}
