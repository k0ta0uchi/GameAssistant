use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let host = cpal::default_host();
    let default_output = host.default_output_device().expect("No default output device");
    let dev_name = default_output.name().unwrap_or_default();
    println!("Testing Default Output Device: {}", dev_name);

    let default_cfg = default_output.default_output_config().expect("Failed default_output_config");
    println!("Format: sample_rate={}, channels={}", default_cfg.sample_rate().0, default_cfg.channels());

    // 1. ループバック受信ストリームを開始
    let peak_bits = Arc::new(AtomicU32::new(0));
    let peak_clone = peak_bits.clone();
    let stream_cfg: cpal::StreamConfig = default_cfg.clone().into();
    
    let capture_stream = default_output.build_input_stream(
        &stream_cfg,
        move |data: &[f32], _| {
            let mut m = 0.0f32;
            for &s in data {
                let a = s.abs();
                if a > m { m = a; }
            }
            let current = f32::from_bits(peak_clone.load(Ordering::Relaxed));
            if m > current {
                peak_clone.store(m.to_bits(), Ordering::Relaxed);
            }
        },
        |e| eprintln!("Capture err: {}", e),
        None,
    ).expect("Failed to build capture stream");
    capture_stream.play().expect("Failed to play capture stream");

    // 2. 同じデバイスにテスト音（440Hz サイン波）を再生する
    let sample_rate = default_cfg.sample_rate().0 as f32;
    let channels = default_cfg.channels() as usize;
    let mut phase = 0.0f32;

    let render_stream = default_output.build_output_stream(
        &stream_cfg,
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(channels) {
                let sample = (phase * 2.0 * std::f32::consts::PI).sin() * 0.5;
                phase = (phase + 440.0 / sample_rate) % 1.0;
                for ch in frame.iter_mut() {
                    *ch = sample;
                }
            }
        },
        |e| eprintln!("Render err: {}", e),
        None,
    ).expect("Failed to build render stream");
    render_stream.play().expect("Failed to play render stream");

    println!("Playing 440Hz sine wave and capturing via loopback for 2 seconds...");
    for i in 1..=4 {
        std::thread::sleep(Duration::from_millis(500));
        let p = f32::from_bits(peak_bits.swap(0, Ordering::Relaxed));
        println!("  Time {}/4: Peak captured = {:.4}", i, p);
    }
}
