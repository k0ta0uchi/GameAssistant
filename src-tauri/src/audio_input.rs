use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use crate::logger::LogManager;

enum AudioCommand {
    StartMic {
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: Arc<dyn Fn(Vec<f32>) + Send + Sync>,
    },
    StartDiscord {
        device_name: Option<String>,
        on_pcm_data: Arc<dyn Fn(Vec<f32>) + Send + Sync>,
    },
    Stop,
}

pub struct AudioInputManager {
    is_running: Arc<AtomicBool>,
    cmd_tx: Mutex<Option<Sender<AudioCommand>>>,
    _worker_thread: Mutex<Option<JoinHandle<()>>>,
    _log_mgr: Arc<LogManager>,
}

unsafe impl Send for AudioInputManager {}
unsafe impl Sync for AudioInputManager {}

impl AudioInputManager {
    pub fn new(log_mgr: Arc<LogManager>) -> Self {
        let is_running = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<AudioCommand>();

        let is_running_clone = is_running.clone();
        let log_mgr_clone = log_mgr.clone();

        let handle = thread::spawn(move || {
            let mut mic_stream: Option<cpal::Stream> = None;
            let mut discord_stream: Option<cpal::Stream> = None;

            while let Ok(cmd) = rx.recv() {
                match cmd {
                    AudioCommand::StartMic {
                        device_name,
                        app_handle,
                        on_pcm_data,
                    } => {
                        if let Some(s) = mic_stream.take() {
                            let _ = s.pause();
                        }

                        let host = cpal::default_host();
                        let device = if let Some(name) = device_name {
                            if name.is_empty() || name == "Default (System Default)" || name == "Default" {
                                host.default_input_device()
                            } else {
                                host.input_devices()
                                    .ok()
                                    .and_then(|mut devs| devs.find(|d| d.name().map(|n| n == name).unwrap_or(false)))
                                    .or_else(|| host.default_input_device())
                            }
                        } else {
                            host.default_input_device()
                        };

                        let dev = match device {
                            Some(d) => d,
                            None => {
                                log_mgr_clone.error("Audio", "No audio input device found");
                                continue;
                            }
                        };

                        let default_cfg = match dev.default_input_config() {
                            Ok(c) => c,
                            Err(e) => {
                                log_mgr_clone.error("Audio", &format!("Failed to get default config: {}", e));
                                continue;
                            }
                        };

                        let dev_name = dev.name().unwrap_or_else(|_| "Unknown".to_string());
                        log_mgr_clone.info("Audio", &format!("Connecting to microphone: '{}' (sample_rate: {}, channels: {})", dev_name, default_cfg.sample_rate().0, default_cfg.channels()));

                        let stream_config: StreamConfig = default_cfg.into();
                        let in_sample_rate = stream_config.sample_rate.0;
                        let in_channels = stream_config.channels as usize;
                        let target_sample_rate = 16000u32;

                        let last_meter_emit = Arc::new(Mutex::new(Instant::now()));
                        let is_running_cb = is_running_clone.clone();
                        let app_handle_meter = app_handle.clone();
                        let on_pcm_cb = on_pcm_data.clone();

                        let err_fn = move |err| {
                            eprintln!("Mic audio stream error: {}", err);
                        };

                        let stream_res = dev.build_input_stream(
                            &stream_config,
                            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                if !is_running_cb.load(Ordering::SeqCst) {
                                    return;
                                }

                                // 1. レベルメーター計算 (RMS)
                                let mut sum_sq = 0.0f32;
                                for i in (0..data.len()).step_by(in_channels) {
                                    sum_sq += data[i] * data[i];
                                }
                                let rms = (sum_sq / (data.len() / in_channels).max(1) as f32).sqrt();

                                if last_meter_emit.lock().elapsed() >= Duration::from_millis(50) {
                                    *last_meter_emit.lock() = Instant::now();
                                    if let Some(ref handle) = app_handle_meter {
                                        let meter_val = (rms * 250.0).clamp(0.0, 100.0);
                                        let _ = handle.emit("level_meter", meter_val);
                                    }
                                }

                                // 2. 16kHz モノラルへリサンプリングし、ストリーミングキューへ直接送信
                                let resampled = resample_linear(data, in_sample_rate, target_sample_rate, in_channels);
                                if !resampled.is_empty() {
                                    on_pcm_cb(resampled);
                                }
                            },
                            err_fn,
                            None,
                        );

                        match stream_res {
                            Ok(stream) => {
                                if let Ok(_) = stream.play() {
                                    mic_stream = Some(stream);
                                    is_running_clone.store(true, Ordering::SeqCst);
                                    log_mgr_clone.info("Audio", "Microphone stream active");
                                }
                            }
                            Err(e) => {
                                log_mgr_clone.error("Audio", &format!("Failed to build mic stream: {}", e));
                            }
                        }
                    }
                    AudioCommand::StartDiscord {
                        device_name,
                        on_pcm_data,
                    } => {
                        if let Some(s) = discord_stream.take() {
                            let _ = s.pause();
                        }

                        let host = cpal::default_host();
                        let device = if let Some(name) = device_name {
                            if name.is_empty() || name == "Default (System Default)" || name == "Default" {
                                host.default_input_device()
                            } else {
                                host.input_devices()
                                    .ok()
                                    .and_then(|mut devs| devs.find(|d| d.name().map(|n| n == name).unwrap_or(false)))
                                    .or_else(|| host.default_input_device())
                            }
                        } else {
                            host.default_input_device()
                        };

                        if let Some(dev) = device {
                            let default_cfg = match dev.default_input_config() {
                                Ok(c) => c,
                                Err(e) => {
                                    log_mgr_clone.error("Discord", &format!("Failed to get default config: {}", e));
                                    continue;
                                }
                            };

                            let dev_name = dev.name().unwrap_or_else(|_| "Unknown".to_string());
                            log_mgr_clone.info("Discord", &format!("Starting Discord Audio Capture stream on '{}' (sample_rate: {}, channels: {})", dev_name, default_cfg.sample_rate().0, default_cfg.channels()));

                            let stream_config: StreamConfig = default_cfg.into();
                            let in_sample_rate = stream_config.sample_rate.0;
                            let in_channels = stream_config.channels as usize;
                            let target_sample_rate = 16000u32;

                            let is_running_cb = is_running_clone.clone();
                            let on_pcm_cb = on_pcm_data.clone();

                            let err_fn = move |err| {
                                eprintln!("Discord audio stream error: {}", err);
                            };

                            if let Ok(stream) = dev.build_input_stream(
                                &stream_config,
                                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                    if !is_running_cb.load(Ordering::SeqCst) {
                                        return;
                                    }

                                    let resampled = resample_linear(data, in_sample_rate, target_sample_rate, in_channels);
                                    if !resampled.is_empty() {
                                        on_pcm_cb(resampled);
                                    }
                                },
                                err_fn,
                                None,
                            ) {
                                if let Ok(_) = stream.play() {
                                    discord_stream = Some(stream);
                                    log_mgr_clone.info("Discord", "Discord Audio stream active");
                                }
                            }
                        }
                    }
                    AudioCommand::Stop => {
                        if let Some(s) = mic_stream.take() {
                            let _ = s.pause();
                        }
                        if let Some(s) = discord_stream.take() {
                            let _ = s.pause();
                        }
                        is_running_clone.store(false, Ordering::SeqCst);
                        log_mgr_clone.info("Audio", "All audio streams stopped");
                    }
                }
            }
        });

        Self {
            is_running,
            cmd_tx: Mutex::new(Some(tx)),
            _worker_thread: Mutex::new(Some(handle)),
            _log_mgr: log_mgr,
        }
    }

    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            let _ = tx.send(AudioCommand::Stop);
        }
    }

    pub fn start_mic_stream<F>(
        &self,
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: F,
    ) -> Result<(), String>
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            tx.send(AudioCommand::StartMic {
                device_name,
                app_handle,
                on_pcm_data: Arc::new(on_pcm_data),
            })
            .map_err(|e| format!("Failed to send start mic audio command: {}", e))?;
            Ok(())
        } else {
            Err("Audio worker thread not available".to_string())
        }
    }

    pub fn start_discord_stream<F>(
        &self,
        device_name: Option<String>,
        on_pcm_data: F,
    ) -> Result<(), String>
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            tx.send(AudioCommand::StartDiscord {
                device_name,
                on_pcm_data: Arc::new(on_pcm_data),
            })
            .map_err(|e| format!("Failed to send start discord audio command: {}", e))?;
            Ok(())
        } else {
            Err("Audio worker thread not available".to_string())
        }
    }
}

/// チャンネルダウンミックス & 線形補間リサンプリング (Any Sample Rate -> 16kHz Mono)
pub fn resample_linear(input: &[f32], in_rate: u32, out_rate: u32, channels: usize) -> Vec<f32> {
    if input.is_empty() || in_rate == 0 || out_rate == 0 || channels == 0 {
        return Vec::new();
    }
    // 1. チャンネルダウンミックス (モノラル化)
    let mono_len = input.len() / channels;
    let mut mono = Vec::with_capacity(mono_len);
    for frame in input.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        mono.push(sum / channels as f32);
    }

    if in_rate == out_rate {
        return mono;
    }

    // 2. 線形補間リサンプリング
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_idx = i as f64 * ratio;
        let idx0 = src_idx.floor() as usize;
        let idx1 = (idx0 + 1).min(mono.len() - 1);
        let frac = (src_idx - idx0 as f64) as f32;
        let s0 = mono[idx0];
        let s1 = mono[idx1];
        output.push(s0 + (s1 - s0) * frac);
    }

    output
}
