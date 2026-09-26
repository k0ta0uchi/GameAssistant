use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    println!("Host: {:?}", host.id());

    println!("\n=== OUTPUT DEVICES ===");
    let mut outputs = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                println!("- Output: {}", name);
                outputs.push((name, d));
            }
        }
    }

    let default_output = host.default_output_device();
    println!("\nDefault Output Device: {:?}", default_output.as_ref().and_then(|d| d.name().ok()));

    for (name, dev) in &outputs {
        println!("\n--- Testing device: {} ---", name);
        let default_cfg = match dev.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                println!("  Failed default_output_config: {}", e);
                continue;
            }
        };
        println!("  Config: sample_rate={}, channels={}, format={:?}",
            default_cfg.sample_rate().0, default_cfg.channels(), default_cfg.sample_format());

        let stream_cfg: cpal::StreamConfig = default_cfg.clone().into();
        let packet_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_val = std::sync::Arc::new(parking_lot::Mutex::new(0.0f32));

        let pc = packet_count.clone();
        let mv = max_val.clone();
        let err_fn = |err| eprintln!("  Stream error: {}", err);

        let stream_res = match default_cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                dev.build_input_stream(
                    &stream_cfg,
                    move |data: &[f32], _| {
                        pc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let mut m = 0.0f32;
                        for &s in data {
                            let a = s.abs();
                            if a > m { m = a; }
                        }
                        let mut guard = mv.lock();
                        if m > *guard { *guard = m; }
                    },
                    err_fn,
                    None,
                )
            }
            _ => {
                println!("  Unsupported format: {:?}", default_cfg.sample_format());
                continue;
            }
        };

        match stream_res {
            Ok(stream) => {
                if let Err(e) = stream.play() {
                    println!("  Failed to play stream: {}", e);
                } else {
                    println!("  Stream active, listening for 1.5 seconds...");
                    std::thread::sleep(Duration::from_millis(1500));
                    let count = packet_count.load(std::sync::atomic::Ordering::Relaxed);
                    let max_v = *max_val.lock();
                    println!("  Result: {} packets received, max amplitude: {:.6}", count, max_v);
                }
            }
            Err(e) => {
                println!("  Failed to build input stream: {}", e);
            }
        }
    }
}
