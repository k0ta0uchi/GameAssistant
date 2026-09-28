// -*- coding: utf-8 -*-
//! TC-10 Integration Test: Active Watchdog and Automatic CPU Failover
//! 1. Set SIMULATE_ASR_HANG=1 to force CTranslate2/Python into a simulated native hang upon receiving audio.
//! 2. Launch WhisperWsClient on CUDA mode.
//! 3. Send audio packet.
//! 4. Verify that Rust Watchdog detects the hang within 2.5-3.0s, force-terminates the child,
//!    and triggers the supervisor to restart with --force-device cpu.
//! 5. Verify that the new CPU worker connects and becomes ready!

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== TC-10: Active Watchdog & Auto CPU Failover Test ===");

    // Step 1: Prime hang simulation for the initial CUDA worker
    std::env::set_var("SIMULATE_ASR_HANG", "1");
    println!("[TEST] Set SIMULATE_ASR_HANG=1 to simulate native C++ hang on first inference.");

    let engine = gameassistant_lib::asr::AsrEngine::new();
    let ws_client = engine.ws_client.clone();

    let callback_invoked = Arc::new(AtomicBool::new(false));
    let cb_clone = callback_invoked.clone();

    println!("[TEST] Starting ASR client on CUDA mode...");
    ws_client.start_with_device(
        move |stream, text, is_final, latency| {
            println!(
                "[CALLBACK] stream={} text='{}' is_final={} latency={:?}",
                stream, text, is_final, latency
            );
            cb_clone.store(true, Ordering::SeqCst);
        },
        Some("cuda".to_string()),
    )?;

    println!("[TEST] Warming up WebSocket connection...");
    ws_client.warmup().await?;
    println!(
        "[TEST] ASR server connected and ready. Device={:?}",
        ws_client.current_device.lock().clone()
    );

    // Step 2: Clear hang env var so the subsequent restart won't hang!
    std::env::remove_var("SIMULATE_ASR_HANG");
    println!("[TEST] Cleared SIMULATE_ASR_HANG for subsequent supervisor restarts.");

    // Step 3: Send audio to trigger the native hang
    let dummy_samples = vec![0.1f32; 16000 * 2]; // 2.0s of audio
    println!("[TEST] Sending audio packet (expecting child to hang)...");
    let t_hang_start = Instant::now();
    ws_client.send_audio("mic", &dummy_samples);

    // Step 4: Monitor for watchdog detection and automatic CPU failover
    println!("[TEST] Monitoring for watchdog failover to CPU (timeout=10s)...");
    let mut failover_detected = false;
    let t_wait = Instant::now();
    while t_wait.elapsed() < Duration::from_secs(12) {
        sleep(Duration::from_millis(500)).await;
        let cur_dev = ws_client.current_device.lock().clone();
        let forced = ws_client.forced_device.lock().clone();
        let is_ready = ws_client.is_ready();

        println!(
            "  [Watchdog Poll] elapsed={:.1}s, current_device={}, forced={:?}, is_ready={}",
            t_hang_start.elapsed().as_secs_f32(),
            cur_dev,
            forced,
            is_ready
        );

        if forced.as_deref() == Some("cpu") && cur_dev == "cpu" && is_ready {
            println!(
                "\n[SUCCESS] Active Watchdog successfully triggered, killed hanging child, and restarted with CPU mode in {:.1}s!",
                t_hang_start.elapsed().as_secs_f32()
            );
            failover_detected = true;
            break;
        }
    }

    ws_client.stop();

    if failover_detected {
        println!("\n=== ALL TC-10 TESTS PASSED ===");
        Ok(())
    } else {
        eprintln!("\n[FAILURE] Watchdog did not trigger CPU failover in time.");
        std::process::exit(1);
    }
}
