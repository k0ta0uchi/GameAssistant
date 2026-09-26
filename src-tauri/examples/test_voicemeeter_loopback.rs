use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    println!("=== VOICEMEETER & LINE LOOPBACK TEST ===");

    let target_names = [
        "Voicemeeter Input",
        "Line (AG06/AG03)",
        "Voicemeeter AUX Input",
        "DELL U3415W",
    ];

    let mut streams = Vec::new();

    if let Ok(devs) = host.output_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            for target in &target_names {
                if name.contains(target) {
                    println!("\nFound target device: {}", name);
                    if let Ok(cfg) = d.default_output_config() {
                        let sample_rate = cfg.sample_rate().0;
                        let channels = cfg.channels();
                        let format = cfg.sample_format();
                        println!("  Format: {} Hz, {} ch, {:?}", sample_rate, channels, format);

                        let stream_cfg: cpal::StreamConfig = cfg.clone().into();
                        let count = Arc::new(AtomicUsize::new(0));
                        let max_peak = Arc::new(std::sync::Mutex::new(0.0f32));

                        let count_c = count.clone();
                        let peak_c = max_peak.clone();

                        // キープアライブ無音出力ストリーム
                        let keep_alive = d.build_output_stream(
                            &stream_cfg,
                            move |data: &mut [f32], _| {
                                for sample in data.iter_mut() {
                                    *sample = 0.0;
                                }
                            },
                            |err| eprintln!("Render keep_alive err: {}", err),
                            None,
                        );
                        if let Ok(ref s) = keep_alive {
                            let _ = s.play();
                            println!("  [OK] Keep-alive render stream playing");
                        } else if let Err(ref e) = keep_alive {
                            println!("  [WARN] Keep-alive render failed: {}", e);
                        }

                        // ループバック入力ストリーム
                        let loopback = d.build_input_stream(
                            &stream_cfg,
                            move |data: &[f32], _| {
                                count_c.fetch_add(data.len(), Ordering::Relaxed);
                                let mut m = 0.0f32;
                                for &s in data {
                                    let a = s.abs();
                                    if a > m { m = a; }
                                }
                                let mut p = peak_c.lock().unwrap();
                                if m > *p { *p = m; }
                            },
                            move |err| eprintln!("Loopback stream err: {}", err),
                            None,
                        );

                        if let Ok(ref s) = loopback {
                            let _ = s.play();
                            println!("  [OK] Loopback input stream playing");
                            streams.push((name.clone(), keep_alive.ok(), loopback.ok(), count, max_peak));
                        } else if let Err(ref e) = loopback {
                            println!("  [ERROR] Loopback stream failed to build: {}", e);
                        }
                    }
                    break;
                }
            }
        }
    }

    println!("\nMonitoring for 10 seconds (samples received and peak amplitude)...");
    for tick in 1..=10 {
        std::thread::sleep(Duration::from_secs(1));
        println!("\n--- Second {}/10 ---", tick);
        for (name, _, _, count, peak) in &streams {
            let samples = count.swap(0, Ordering::Relaxed);
            let p = {
                let mut guard = peak.lock().unwrap();
                let v = *guard;
                *guard = 0.0;
                v
            };
            let bar_len = (p * 50.0).min(50.0) as usize;
            let bar = "#".repeat(bar_len);
            println!("{:<40} | Samples/sec: {:>6} | Peak: {:.4} |{}", name, samples, p, bar);
        }
    }
}
