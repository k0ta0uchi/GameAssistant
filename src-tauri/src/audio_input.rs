use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use crate::logger::LogManager;

pub type PcmCallback = Arc<dyn Fn(Vec<f32>) + Send + Sync>;

#[cfg(windows)]
extern "C" {
    fn start_discord_process_loopback(
        callback: extern "C" fn(*const f32, i32, i32, i32, *mut std::ffi::c_void),
        user_data: *mut std::ffi::c_void,
    ) -> i32;
    fn stop_discord_process_loopback();
    #[allow(dead_code)]
    fn is_discord_process_loopback_running() -> bool;
}

#[cfg(windows)]
struct DiscordCallbackContext {
    app_handle: Option<AppHandle>,
    on_pcm_data: Option<PcmCallback>,
    is_running: Arc<AtomicBool>,
    max_meter_val: Mutex<f64>,
    last_meter_emit: Mutex<Instant>,
    resampler: Mutex<StreamingResampler>,
}

#[cfg(windows)]
extern "C" fn discord_audio_callback(
    samples_ptr: *const f32,
    num_samples: i32,
    sample_rate: i32,
    channels: i32,
    user_data: *mut std::ffi::c_void,
) {
    if samples_ptr.is_null() || num_samples <= 0 || user_data.is_null() {
        return;
    }

    let ctx = unsafe { &*(user_data as *const DiscordCallbackContext) };
    if !ctx.is_running.load(Ordering::SeqCst) {
        return;
    }

    let slice = unsafe { std::slice::from_raw_parts(samples_ptr, num_samples as usize) };

    // 音量レベルメーターの計算
    let meter_val = calculate_meter_level(slice);
    {
        let mut cur = ctx.max_meter_val.lock();
        if meter_val > *cur {
            *cur = meter_val;
        }
    }

    // 50ms ごとに最大ピークを emit
    let should_emit = {
        let mut last = ctx.last_meter_emit.lock();
        if last.elapsed() >= Duration::from_millis(50) {
            *last = Instant::now();
            true
        } else {
            false
        }
    };

    if should_emit {
        let emit_val = {
            let mut cur = ctx.max_meter_val.lock();
            let v = *cur;
            *cur = 0.0;
            v
        };
        if let Some(ref handle) = ctx.app_handle {
            let _ = handle.emit("discord_level_meter", emit_val);
        }
    }

    // 音声データが検出された場合（有音時）、Whisper へ PCM 転送 (16kHz モノラル)
    if meter_val > 0.5 {
        if let Some(ref cb) = ctx.on_pcm_data {
            let mut resampler = ctx.resampler.lock();
            resampler.ensure_config(sample_rate as u32, 16000, channels as usize);
            let resampled = resampler.resample(slice);
            if !resampled.is_empty() {
                cb(resampled);
            }
        }
    }
}

enum AudioCommand {
    StartMic {
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: Option<PcmCallback>,
    },
    StartDiscord {
        #[allow(dead_code)]
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: Option<PcmCallback>,
    },
    StopMic,
    StopDiscord,
    StopAll,
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

        #[allow(unused_assignments)]
        let handle = thread::spawn(move || {
            let mut mic_stream: Option<cpal::Stream> = None;
            #[cfg(windows)]
            let mut _discord_ctx: Option<Box<DiscordCallbackContext>> = None;

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
                        let all_inputs = host
                            .input_devices()
                            .map(|iter| iter.collect::<Vec<_>>())
                            .unwrap_or_default();
                        let device = if let Some(ref name) = device_name {
                            if name.is_empty()
                                || name == "Default (System Default)"
                                || name == "Default"
                            {
                                host.default_input_device()
                            } else {
                                find_device_fuzzy(&all_inputs, name)
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
                                log_mgr_clone.error(
                                    "Audio",
                                    &format!("Failed to get default input config: {}", e),
                                );
                                continue;
                            }
                        };

                        let dev_name = dev.name().unwrap_or_else(|_| "Unknown".to_string());
                        log_mgr_clone.info(
                            "Audio",
                            &format!(
                                "Connecting to microphone: '{}' (sample_rate: {}, channels: {}, format: {:?})",
                                dev_name,
                                default_cfg.sample_rate().0,
                                default_cfg.channels(),
                                default_cfg.sample_format()
                            ),
                        );

                        let in_sample_rate = default_cfg.sample_rate().0;
                        let in_channels = default_cfg.channels() as usize;
                        let target_sample_rate = 16000u32;

                        let last_meter_emit = Arc::new(Mutex::new(Instant::now()));
                        let is_running_cb = is_running_clone.clone();
                        let app_handle_meter = app_handle.clone();
                        let on_pcm_cb = on_pcm_data.clone();
                        let resampler = Mutex::new(StreamingResampler::new(
                            in_sample_rate,
                            target_sample_rate,
                            in_channels,
                        ));

                        let stream_res = build_flexible_input_stream(
                            &dev,
                            &default_cfg,
                            move |data: &[f32]| {
                                if !is_running_cb.load(Ordering::SeqCst) {
                                    return;
                                }

                                // 1. レベルメーター計算 (Peak & RMS ハイブリッド)
                                if last_meter_emit.lock().elapsed() >= Duration::from_millis(50) {
                                    *last_meter_emit.lock() = Instant::now();
                                    if let Some(ref handle) = app_handle_meter {
                                        let meter_val = calculate_meter_level(data);
                                        let _ = handle.emit("level_meter", meter_val);
                                    }
                                }

                                // 2. 16kHz モノラルへステートフルリサンプリングし、コールバックがあれば転送
                                if let Some(ref cb) = on_pcm_cb {
                                    let resampled = resampler.lock().resample(data);
                                    if !resampled.is_empty() {
                                        cb(resampled);
                                    }
                                }
                            },
                        );

                        match stream_res {
                            Ok(stream) => {
                                if stream.play().is_ok() {
                                    mic_stream = Some(stream);
                                    is_running_clone.store(true, Ordering::SeqCst);
                                    log_mgr_clone.info("Audio", "Microphone stream active");
                                }
                            }
                            Err(e) => {
                                log_mgr_clone
                                    .error("Audio", &format!("Failed to build mic stream: {}", e));
                            }
                        }
                    }
                    AudioCommand::StartDiscord {
                        device_name: _,
                        app_handle,
                        on_pcm_data,
                    } => {
                        #[cfg(windows)]
                        {
                            unsafe {
                                stop_discord_process_loopback();
                            }
                            _discord_ctx = None;

                            log_mgr_clone.info("Discord", "Starting Discord process loopback capture (OBS Application Audio Capture)...");

                            let ctx = Box::new(DiscordCallbackContext {
                                app_handle,
                                on_pcm_data,
                                is_running: is_running_clone.clone(),
                                max_meter_val: Mutex::new(0.0),
                                last_meter_emit: Mutex::new(Instant::now()),
                                resampler: Mutex::new(StreamingResampler::new(48000, 16000, 2)),
                            });
                            let ctx_ptr = Box::into_raw(ctx);

                            let res = unsafe {
                                start_discord_process_loopback(
                                    discord_audio_callback,
                                    ctx_ptr as *mut std::ffi::c_void,
                                )
                            };

                            if res == 0 {
                                is_running_clone.store(true, Ordering::SeqCst);
                                _discord_ctx = Some(unsafe { Box::from_raw(ctx_ptr) });
                                log_mgr_clone.info("Discord", "Discord process loopback started successfully (target: Discord.exe)");
                            } else {
                                unsafe {
                                    drop(Box::from_raw(ctx_ptr));
                                }
                                log_mgr_clone.error(
                                    "Discord",
                                    "Failed to start Discord process loopback capture",
                                );
                            }
                        }
                    }
                    AudioCommand::StopMic => {
                        if let Some(s) = mic_stream.take() {
                            let _ = s.pause();
                        }
                        log_mgr_clone.info("Audio", "Microphone stream stopped");
                    }
                    AudioCommand::StopDiscord => {
                        #[cfg(windows)]
                        {
                            unsafe {
                                stop_discord_process_loopback();
                            }
                            _discord_ctx = None;
                        }
                        log_mgr_clone.info("Discord", "Discord process loopback stopped");
                    }
                    AudioCommand::StopAll => {
                        if let Some(s) = mic_stream.take() {
                            let _ = s.pause();
                        }
                        #[cfg(windows)]
                        {
                            unsafe {
                                stop_discord_process_loopback();
                            }
                            _discord_ctx = None;
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
            let _ = tx.send(AudioCommand::StopAll);
        }
    }

    pub fn stop_mic(&self) {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            let _ = tx.send(AudioCommand::StopMic);
        }
    }

    pub fn stop_discord(&self) {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            let _ = tx.send(AudioCommand::StopDiscord);
        }
    }

    pub fn start_mic_stream(
        &self,
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: Option<PcmCallback>,
    ) -> Result<(), String> {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            tx.send(AudioCommand::StartMic {
                device_name,
                app_handle,
                on_pcm_data,
            })
            .map_err(|e| format!("Failed to send start mic audio command: {}", e))?;
            Ok(())
        } else {
            Err("Audio worker thread not available".to_string())
        }
    }

    pub fn start_discord_stream(
        &self,
        device_name: Option<String>,
        app_handle: Option<AppHandle>,
        on_pcm_data: Option<PcmCallback>,
    ) -> Result<(), String> {
        if let Some(ref tx) = *self.cmd_tx.lock() {
            tx.send(AudioCommand::StartDiscord {
                device_name,
                app_handle,
                on_pcm_data,
            })
            .map_err(|e| format!("Failed to send start discord audio command: {}", e))?;
            Ok(())
        } else {
            Err("Audio worker thread not available".to_string())
        }
    }
}

/// サポートされている任意のサンプルフォーマット (F32, I16, U16) で入力ストリームを構築する
fn build_flexible_input_stream<F>(
    dev: &cpal::Device,
    default_cfg: &cpal::SupportedStreamConfig,
    mut on_data: F,
) -> Result<cpal::Stream, String>
where
    F: FnMut(&[f32]) + Send + Sync + 'static,
{
    let stream_config: cpal::StreamConfig = default_cfg.clone().into();
    let err_fn = move |err| {
        eprintln!("[Audio Stream Error]: {}", err);
    };

    match default_cfg.sample_format() {
        cpal::SampleFormat::F32 => dev
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| on_data(data),
                err_fn,
                None,
            )
            .map_err(|e| format!("Failed to build F32 stream: {}", e)),
        cpal::SampleFormat::I16 => {
            let mut buf = Vec::new();
            dev.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    buf.clear();
                    buf.reserve(data.len());
                    for &s in data {
                        buf.push(s as f32 / 32768.0);
                    }
                    on_data(&buf);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Failed to build I16 stream: {}", e))
        }
        cpal::SampleFormat::U16 => {
            let mut buf = Vec::new();
            dev.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    buf.clear();
                    buf.reserve(data.len());
                    for &s in data {
                        buf.push((s as f32 - 32768.0) / 32768.0);
                    }
                    on_data(&buf);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Failed to build U16 stream: {}", e))
        }
        other => Err(format!("Unsupported audio sample format: {:?}", other)),
    }
}

/// ステートフル・ストリーミングリサンプラー (Any Sample Rate / Channels -> 16kHz Mono)
/// コールバックチャンク境界で位相 (phase) と前サンプルの状態を保持し、
/// 44.1kHz / 48kHz 等からのリサンプリング時における境界歪み・位相ジッターを防止する。
#[derive(Debug, Clone)]
pub struct StreamingResampler {
    in_rate: u32,
    out_rate: u32,
    channels: usize,
    ratio: f64,
    phase: f64,
    last_sample: f32,
    has_last_sample: bool,
}

impl StreamingResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let ratio = if out_rate > 0 {
            in_rate as f64 / out_rate as f64
        } else {
            1.0
        };
        Self {
            in_rate,
            out_rate,
            channels,
            ratio,
            phase: 0.0,
            last_sample: 0.0,
            has_last_sample: false,
        }
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.last_sample = 0.0;
        self.has_last_sample = false;
    }

    pub fn ensure_config(&mut self, in_rate: u32, out_rate: u32, channels: usize) {
        let channels = channels.max(1);
        if self.in_rate != in_rate || self.out_rate != out_rate || self.channels != channels {
            self.in_rate = in_rate;
            self.out_rate = out_rate;
            self.channels = channels;
            self.ratio = if out_rate > 0 {
                in_rate as f64 / out_rate as f64
            } else {
                1.0
            };
            self.reset();
        }
    }

    pub fn resample(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() || self.in_rate == 0 || self.out_rate == 0 || self.channels == 0 {
            return Vec::new();
        }

        // 1. チャンネルダウンミックス (モノラル化)
        let mono_len = input.len() / self.channels;
        if mono_len == 0 {
            return Vec::new();
        }
        let mut mono = Vec::with_capacity(mono_len);
        for frame in input.chunks_exact(self.channels) {
            let sum: f32 = frame.iter().sum();
            mono.push(sum / self.channels as f32);
        }

        // 入力レートと出力レートが同一の場合
        if self.in_rate == self.out_rate {
            self.last_sample = *mono.last().unwrap();
            self.has_last_sample = true;
            return mono;
        }

        // 2. ステートフル線形補間
        let mut output = Vec::new();
        let mut t = self.phase;
        let n = mono.len();

        while t < n as f64 - 1.0 {
            if t < 0.0 {
                // 前チャンクの last_sample (t = -1.0) と mono[0] (t = 0.0) の間で補間
                let s0 = if self.has_last_sample {
                    self.last_sample
                } else {
                    mono[0]
                };
                let s1 = mono[0];
                let frac = (t + 1.0).clamp(0.0, 1.0) as f32;
                output.push(s0 + (s1 - s0) * frac);
            } else {
                let idx0 = t.floor() as usize;
                let idx1 = idx0 + 1;
                let frac = (t - idx0 as f64) as f32;
                let s0 = mono[idx0];
                let s1 = mono[idx1];
                output.push(s0 + (s1 - s0) * frac);
            }
            t += self.ratio;
        }

        self.last_sample = *mono.last().unwrap();
        self.has_last_sample = true;
        self.phase = t - n as f64;

        output
    }
}

/// チャンネルダウンミックス & 線形補間リサンプリング (Any Sample Rate -> 16kHz Mono)
/// ※ 後方互換性およびワンショット変換用。連続ストリーミングには StreamingResampler を推奨。
pub fn resample_linear(input: &[f32], in_rate: u32, out_rate: u32, channels: usize) -> Vec<f32> {
    let mut resampler = StreamingResampler::new(in_rate, out_rate, channels);
    resampler.resample(input)
}

/// 全チャンネルの振幅（Peak & RMS ハイブリッド）からパーセンテージ（0.0 - 100.0, 小数点第1位）を算出する
pub fn calculate_meter_level(data: &[f32]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut max_abs = 0.0f32;
    let mut sum_sq = 0.0f32;
    for &s in data {
        let abs = s.abs();
        if abs > max_abs {
            max_abs = abs;
        }
        sum_sq += s * s;
    }
    let rms = (sum_sq / data.len() as f32).sqrt();
    // Peak 60% + RMS 40% のブレンドで素早い反応と持続音を両立
    let level = max_abs * 0.6 + rms * 0.4;
    // 人間の聴覚特性に合わせた自然なカーブ (sqrt)
    let meter = (level.sqrt() * 100.0).clamp(0.0, 100.0);
    (meter * 10.0).round() as f64 / 10.0
}

/// OBS Studio 風の柔軟なデバイス検索（完全一致 -> trim -> 大文字小文字無視 -> 部分一致）
pub fn find_device_fuzzy(devs: &[cpal::Device], target: &str) -> Option<cpal::Device> {
    let target_clean = target.trim();
    if target_clean.is_empty() {
        return None;
    }
    // 1. 完全一致
    if let Some(d) = devs
        .iter()
        .find(|d| d.name().map(|n| n == target).unwrap_or(false))
    {
        return Some(d.clone());
    }
    // 2. trim() 一致
    if let Some(d) = devs
        .iter()
        .find(|d| d.name().map(|n| n.trim() == target_clean).unwrap_or(false))
    {
        return Some(d.clone());
    }
    // 3. 大文字小文字無視
    let target_lower = target_clean.to_lowercase();
    if let Some(d) = devs.iter().find(|d| {
        d.name()
            .map(|n| n.trim().to_lowercase() == target_lower)
            .unwrap_or(false)
    }) {
        return Some(d.clone());
    }
    // 4. 部分一致 (contains)
    if let Some(d) = devs.iter().find(|d| {
        d.name()
            .map(|n| {
                let nl = n.to_lowercase();
                nl.contains(&target_lower) || target_lower.contains(&nl)
            })
            .unwrap_or(false)
    }) {
        return Some(d.clone());
    }
    None
}

/// OBS Silent Loopback Fix: 出力デバイスに対して無音 (0.0) を流すダミーストリームを作成し、
/// WASAPI ループバックがスリープ・停止するのを防止して常時ウェイク状態に保つ
pub fn build_keep_alive_render_stream(
    dev: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
) -> Option<cpal::Stream> {
    let stream_cfg: cpal::StreamConfig = cfg.clone().into();
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev
            .build_output_stream(
                &stream_cfg,
                |data: &mut [f32], _| {
                    for s in data.iter_mut() {
                        *s = 0.0;
                    }
                },
                |_| {},
                None,
            )
            .ok(),
        cpal::SampleFormat::I16 => dev
            .build_output_stream(
                &stream_cfg,
                |data: &mut [i16], _| {
                    for s in data.iter_mut() {
                        *s = 0;
                    }
                },
                |_| {},
                None,
            )
            .ok(),
        cpal::SampleFormat::U16 => dev
            .build_output_stream(
                &stream_cfg,
                |data: &mut [u16], _| {
                    for s in data.iter_mut() {
                        *s = 32768;
                    }
                },
                |_| {},
                None,
            )
            .ok(),
        _ => None,
    };
    if let Some(ref s) = stream {
        let _ = s.play();
    }
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_resampler_preserves_phase_and_length() {
        let in_rate = 48000u32;
        let out_rate = 16000u32;
        let total_samples = 48000; // 1 second
        let mut sine_wave = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            let t = i as f32 / in_rate as f32;
            sine_wave.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
        }

        // 1. One-shot resample
        let mut oneshot_resampler = StreamingResampler::new(in_rate, out_rate, 1);
        let oneshot_output = oneshot_resampler.resample(&sine_wave);

        // 2. Chunked streaming resample (chunk size = 480 samples = 10ms)
        let mut streaming_resampler = StreamingResampler::new(in_rate, out_rate, 1);
        let mut streaming_output = Vec::new();
        for chunk in sine_wave.chunks(480) {
            let out_chunk = streaming_resampler.resample(chunk);
            streaming_output.extend(out_chunk);
        }

        // 出力サイズが一致すること（16000 samples 周辺）
        assert_eq!(oneshot_output.len(), streaming_output.len());
        assert!((oneshot_output.len() as i32 - 16000).abs() <= 2);

        // 各サンプルの誤差が極小であること（位相飛びがないこと）
        for (i, (&s1, &s2)) in oneshot_output
            .iter()
            .zip(streaming_output.iter())
            .enumerate()
        {
            let diff = (s1 - s2).abs();
            assert!(
                diff < 1e-5,
                "Sample {} diverged: oneshot={}, streaming={}, diff={}",
                i,
                s1,
                s2,
                diff
            );
        }
    }

    #[test]
    fn test_streaming_resampler_fractional_rate_44100() {
        let in_rate = 44100u32;
        let out_rate = 16000u32;
        let total_samples = 44100;
        let mut sine_wave = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            let t = i as f32 / in_rate as f32;
            sine_wave.push((2.0 * std::f32::consts::PI * 300.0 * t).sin());
        }

        let mut oneshot_resampler = StreamingResampler::new(in_rate, out_rate, 1);
        let oneshot_output = oneshot_resampler.resample(&sine_wave);

        let mut streaming_resampler = StreamingResampler::new(in_rate, out_rate, 1);
        let mut streaming_output = Vec::new();
        for chunk in sine_wave.chunks(512) {
            let out_chunk = streaming_resampler.resample(chunk);
            streaming_output.extend(out_chunk);
        }

        assert_eq!(oneshot_output.len(), streaming_output.len());
        for (i, (&s1, &s2)) in oneshot_output
            .iter()
            .zip(streaming_output.iter())
            .enumerate()
        {
            let diff = (s1 - s2).abs();
            assert!(
                diff < 1e-4,
                "Sample {} diverged: oneshot={}, streaming={}, diff={}",
                i,
                s1,
                s2,
                diff
            );
        }
    }

    #[test]
    fn test_streaming_resampler_stereo_downmix() {
        let in_rate = 48000u32;
        let out_rate = 16000u32;
        let channels = 2;
        // L = 1.0, R = 0.5 -> Mono = 0.75
        let stereo: Vec<f32> = vec![1.0, 0.5, 1.0, 0.5, 1.0, 0.5, 1.0, 0.5, 1.0, 0.5, 1.0, 0.5];
        let mut resampler = StreamingResampler::new(in_rate, out_rate, channels);
        let out = resampler.resample(&stereo);
        for &s in &out {
            assert!((s - 0.75).abs() < 1e-5);
        }
    }
}
