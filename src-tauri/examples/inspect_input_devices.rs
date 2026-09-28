use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    println!("=== ALL INPUT / RECORDING DEVICES TEST ===");

    let mut streams = Vec::new();

    if let Ok(devs) = host.input_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            if let Ok(cfg) = d.default_input_config() {
                let stream_cfg: cpal::StreamConfig = cfg.clone().into();
                let peak = Arc::new(std::sync::Mutex::new(0.0f32));
                let count = Arc::new(AtomicUsize::new(0));

                let p_clone = peak.clone();
                let c_clone = count.clone();

                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => d
                        .build_input_stream(
                            &stream_cfg,
                            move |data: &[f32], _| {
                                c_clone.fetch_add(data.len(), Ordering::Relaxed);
                                let mut m = 0.0f32;
                                for &s in data {
                                    let a = s.abs();
                                    if a > m {
                                        m = a;
                                    }
                                }
                                let mut p = p_clone.lock().unwrap();
                                if m > *p {
                                    *p = m;
                                }
                            },
                            |_| {},
                            None,
                        )
                        .ok(),
                    cpal::SampleFormat::I16 => d
                        .build_input_stream(
                            &stream_cfg,
                            move |data: &[i16], _| {
                                c_clone.fetch_add(data.len(), Ordering::Relaxed);
                                let mut m = 0.0f32;
                                for &s in data {
                                    let a = (s as f32 / 32768.0).abs();
                                    if a > m {
                                        m = a;
                                    }
                                }
                                let mut p = p_clone.lock().unwrap();
                                if m > *p {
                                    *p = m;
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

                streams.push((name, stream, count, peak));
            }
        }
    }

    println!(
        "Monitoring {} input devices for 5 seconds...",
        streams.len()
    );
    for sec in 1..=5 {
        std::thread::sleep(Duration::from_secs(1));
        println!("\n--- Second {}/5 ---", sec);
        for (name, s, count, peak) in &streams {
            let samples = count.swap(0, Ordering::Relaxed);
            let p = {
                let mut guard = peak.lock().unwrap();
                let v = *guard;
                *guard = 0.0;
                v
            };
            if p > 0.0001 || samples > 0 {
                let active = if s.is_some() { "OK" } else { "FAIL" };
                let bar_len = (p * 50.0).min(50.0) as usize;
                let bar = "#".repeat(bar_len);
                println!(
                    "{:<45} [{}] Samples: {:>6} | Peak: {:.4} |{}",
                    name, active, samples, p, bar
                );
            }
        }
    }
}
