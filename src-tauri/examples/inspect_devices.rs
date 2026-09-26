use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    if let Some(def) = host.default_output_device() {
        println!(">>> DEFAULT OUTPUT DEVICE: {}", def.name().unwrap_or_default());
    } else {
        println!(">>> NO DEFAULT OUTPUT DEVICE FOUND");
    }
    println!("=== OUTPUT DEVICES (WASAPI) ===");
    if let Ok(devs) = host.output_devices() {
        for (i, d) in devs.enumerate() {
            let name = d.name().unwrap_or_else(|_| "Unknown".to_string());
            println!("\n[{}] {}", i + 1, name);
            if let Ok(cfg) = d.default_output_config() {
                println!("    Default Output: {} Hz, {} ch, {:?}", cfg.sample_rate().0, cfg.channels(), cfg.sample_format());
            }
            if let Ok(cfg) = d.default_input_config() {
                println!("    Default Input (Loopback): {} Hz, {} ch, {:?}", cfg.sample_rate().0, cfg.channels(), cfg.sample_format());
            }
        }
    }
}
