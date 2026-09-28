use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevicesResponse {
    pub input_devices: Vec<String>,
    pub default_device: Option<String>,
    #[serde(default)]
    pub output_devices: Vec<String>,
    #[serde(default)]
    pub default_output_device: Option<String>,
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

    let mut output_devices = vec![
        "Auto (Discord App / System Loopback)".to_string(),
        "Default (System Playback Loopback)".to_string(),
    ];
    let default_output_device = Some("Auto (Discord App / System Loopback)".to_string());

    if let Ok(devices) = host.output_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                let trimmed = name.trim().to_string();
                if !trimmed.is_empty()
                    && !output_devices.contains(&trimmed)
                    && !output_devices.contains(&name)
                {
                    output_devices.push(name);
                }
            }
        }
    }

    AudioDevicesResponse {
        input_devices,
        default_device,
        output_devices,
        default_output_device,
    }
}
