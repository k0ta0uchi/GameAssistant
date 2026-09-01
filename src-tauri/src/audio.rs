use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevicesResponse {
    pub input_devices: Vec<String>,
    pub default_device: Option<String>,
}

pub fn list_input_devices() -> AudioDevicesResponse {
    let host = cpal::default_host();
    let mut input_devices = vec!["Default (System Default)".to_string()];
    let default_device = Some("Default (System Default)".to_string());

    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if !name.trim().is_empty() && !input_devices.contains(&name) {
                    input_devices.push(name);
                }
            }
        }
    }

    AudioDevicesResponse {
        input_devices,
        default_device,
    }
}
