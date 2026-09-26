use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct DeviceMonitor {
    name: String,
    stream: Option<cpal::Stream>,
    max_peak_raw: Arc<AtomicU32>,
}

fn main() {
    let host = cpal::default_host();
    println!("=== AUDIO OUTPUT LOOPBACK MONITOR (10 SECONDS) ===");
    println!("Please play sound in Discord NOW!");

    let mut monitors = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                if let Ok(cfg) = d.default_output_config() {
                    let peak_raw = Arc::new(AtomicU32::new(0));
                    let peak_clone = peak_raw.clone();
                    let stream_cfg: cpal::StreamConfig = cfg.clone().into();
                    
                    let stream = match cfg.sample_format() {
                        cpal::SampleFormat::F32 => {
                            d.build_input_stream(
                                &stream_cfg,
                                move |data: &[f32], _| {
                                    let mut m = 0.0f32;
                                    for &s in data {
                                        let a = s.abs();
                                        if a > m { m = a; }
                                    }
                                    let current_bits = peak_clone.load(Ordering::Relaxed);
                                    let current = f32::from_bits(current_bits);
                                    if m > current {
                                        peak_clone.store(m.to_bits(), Ordering::Relaxed);
                                    }
                                },
                                |_| {},
                                None,
                            ).ok()
                        }
                        _ => None,
                    };

                    if let Some(ref s) = stream {
                        let _ = s.play();
                    }

                    monitors.push(DeviceMonitor {
                        name,
                        stream,
                        max_peak_raw: peak_raw,
                    });
                }
            }
        }
    }

    println!("\nMonitoring {} output devices...", monitors.len());
    for tick in 1..=10 {
        std::thread::sleep(Duration::from_secs(1));
        println!("\n--- Second {}/10 ---", tick);
        for m in &monitors {
            let peak_bits = m.max_peak_raw.swap(0, Ordering::Relaxed);
            let peak = f32::from_bits(peak_bits);
            let active = if m.stream.is_some() { "ACTIVE" } else { "FAILED" };
            let bar_len = (peak * 50.0).min(50.0) as usize;
            let bar: String = "#".repeat(bar_len);
            println!("{:<45} [{}] Peak: {:.4} |{}", m.name, active, peak, bar);
        }
    }
}
