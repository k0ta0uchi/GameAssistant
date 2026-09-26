use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    println!("=== LIVE AUDIO SNIFFER (ALL OUTPUTS & INPUTS) ===");

    let mut streams = Vec::new();

    // 1. 全出力デバイス（WASAPI ループバック）
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            if let Ok(cfg) = d.default_output_config() {
                let stream_cfg: cpal::StreamConfig = cfg.clone().into();
                let peak = Arc::new(std::sync::Mutex::new(0.0f32));
                let p_clone = peak.clone();

                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => d.build_input_stream(
                        &stream_cfg,
                        move |data: &[f32], _| {
                            let mut m = 0.0f32;
                            for &s in data {
                                let a = s.abs();
                                if a > m { m = a; }
                            }
                            let mut p = p_clone.lock().unwrap();
                            if m > *p { *p = m; }
                        },
                        |_| {},
                        None,
                    ).ok(),
                    _ => None,
                };
                if let Some(ref s) = stream {
                    let _ = s.play();
                }
                streams.push((format!("[OUTPUT Loopback] {}", name), stream, peak));
            }
        }
    }

    // 2. 全入力デバイス（録音・マイク・仮想出力）
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            if let Ok(cfg) = d.default_input_config() {
                let stream_cfg: cpal::StreamConfig = cfg.clone().into();
                let peak = Arc::new(std::sync::Mutex::new(0.0f32));
                let p_clone = peak.clone();

                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => d.build_input_stream(
                        &stream_cfg,
                        move |data: &[f32], _| {
                            let mut m = 0.0f32;
                            for &s in data {
                                let a = s.abs();
                                if a > m { m = a; }
                            }
                            let mut p = p_clone.lock().unwrap();
                            if m > *p { *p = m; }
                        },
                        |_| {},
                        None,
                    ).ok(),
                    cpal::SampleFormat::I16 => d.build_input_stream(
                        &stream_cfg,
                        move |data: &[i16], _| {
                            let mut m = 0.0f32;
                            for &s in data {
                                let a = (s as f32 / 32768.0).abs();
                                if a > m { m = a; }
                            }
                            let mut p = p_clone.lock().unwrap();
                            if m > *p { *p = m; }
                        },
                        |_| {},
                        None,
                    ).ok(),
                    _ => None,
                };
                if let Some(ref s) = stream {
                    let _ = s.play();
                }
                streams.push((format!("[INPUT Mic/Line]  {}", name), stream, peak));
            }
        }
    }

    println!("Sniffing {} streams for 10 seconds...", streams.len());
    for sec in 1..=10 {
        std::thread::sleep(Duration::from_secs(1));
        println!("\n--- Second {}/10 ---", sec);
        let mut active_count = 0;
        for (name, s, peak) in &streams {
            let p = {
                let mut guard = peak.lock().unwrap();
                let v = *guard;
                *guard = 0.0;
                v
            };
            if p > 0.005 {
                active_count += 1;
                let bar_len = (p * 50.0).min(50.0) as usize;
                let bar = "#".repeat(bar_len);
                println!("{:<55} | Peak: {:.4} |{}", name, p, bar);
            }
        }
        if active_count == 0 {
            println!("  (No audio signal detected on any device)");
        }
    }
}
