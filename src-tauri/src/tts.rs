use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsSettings {
    pub tts_engine: String,       // "voicevox" | "style_bert_vits2" | "gemini"
    pub speaker_id: i32,          // VOICEVOX speaker id (default: 46)
    pub vits2_speaker_id: i32,    // Style-Bert-VITS2 speaker id (default: 0)
    pub voicevox_url: String,     // default "http://127.0.0.1:50021"
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
}

impl TtsManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AudioCommand>();
        let is_speaking = Arc::new(AtomicBool::new(false));
        let is_speaking_thread = is_speaking.clone();

        // 専用のオーディオ再生ワーカースレッド（スレッド内で OutputStream を所有）
        std::thread::spawn(move || {
            let stream_res = OutputStream::try_default();
            if stream_res.is_err() {
                eprintln!("[TTS] Failed to initialize default output stream in audio thread");
                return;
            }
            let (_stream, stream_handle) = stream_res.unwrap();
            let mut current_sink: Option<Sink> = None;

            while let Some(cmd) = rx.blocking_recv() {
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
                                        is_speaking_thread.store(true, Ordering::SeqCst);
                                        sink.sleep_until_end();
                                        is_speaking_thread.store(false, Ordering::SeqCst);
                                        let _ = reply.send(Ok(()));
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
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            client,
            tx,
            is_speaking,
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

                let synth_url = format!("{}/synthesis?speaker={}", base_url, settings.vits2_speaker_id);
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
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(AudioCommand::PlayWav(wav_bytes, reply_tx))
            .map_err(|e| format!("Failed to send audio command: {}", e))?;

        reply_rx
            .await
            .map_err(|e| format!("Audio thread response error: {}", e))?
    }

    /// テキストの音声合成から再生まで一括実行
    pub async fn speak(&self, text: &str, settings: &TtsSettings) -> Result<(), String> {
        let wav = self.synthesize(text, settings).await?;
        self.play_wav(wav).await
    }

    /// 相槌（wav/nod/0.wav, 1.wav, 2.wav, 4.wav, 5.wav）をランダム再生する
    pub async fn play_random_nod(&self, root_dir: &Path) -> Result<(), String> {
        let nod_indices = [0, 1, 2, 4, 5];
        let idx = nod_indices[rand::random::<usize>() % nod_indices.len()];
        let nod_path = root_dir.join("wav").join("nod").join(format!("{}.wav", idx));

        if nod_path.exists() {
            if let Ok(bytes) = tokio::fs::read(&nod_path).await {
                return self.play_wav(bytes).await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nod_wav_files_exist() {
        let cur_dir = std::env::current_dir().unwrap();
        let root = if cur_dir.ends_with("src-tauri") {
            cur_dir.parent().unwrap().to_path_buf()
        } else {
            cur_dir
        };

        for idx in [0, 1, 2, 4, 5] {
            let p = root.join("wav").join("nod").join(format!("{}.wav", idx));
            assert!(p.exists(), "Nod wav file should exist: {:?}", p);
        }
    }
}
