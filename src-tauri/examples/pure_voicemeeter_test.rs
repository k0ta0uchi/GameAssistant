use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    println!("=== PURE VOICEMEETER INPUT LOOPBACK (NO KEEP-ALIVE) ===");

    if let Ok(devs) = host.output_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            if name.contains("Voicemeeter Input (VB-Audio Voicemeeter VAIO)") {
                println!("Target found: {}", name);
                let cfg = d.default_output_config().expect("default config");
                println!(
                    "Config: {} Hz, {} ch, {:?}",
                    cfg.sample_rate().0,
                    cfg.channels(),
                    cfg.sample_format()
                );

                let stream_cfg: cpal::StreamConfig = cfg.into();
                let count = Arc::new(AtomicUsize::new(0));
                let peak = Arc::new(std::sync::Mutex::new(0.0f32));
                let c_clone = count.clone();
                let p_clone = peak.clone();

                let stream = d
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
                        |err| eprintln!("Stream error: {}", err),
                        None,
                    )
                    .expect("build input stream");

                stream.play().expect("play stream");
                println!("Stream playing. Listening for 10 seconds...");

                for sec in 1..=10 {
                    std::thread::sleep(Duration::from_secs(1));
                    let samples = count.swap(0, Ordering::Relaxed);
                    let val = {
                        let mut guard = peak.lock().unwrap();
                        let v = *guard;
                        *guard = 0.0;
                        v
                    };
                    let bar_len = (val * 50.0).min(50.0) as usize;
                    let bar = "#".repeat(bar_len);
                    println!(
                        "Sec {:>2}/10 | Samples: {:>6} | Peak: {:.4} |{}",
                        sec, samples, val, bar
                    );
                }
                return;
            }
        }
    }
}
