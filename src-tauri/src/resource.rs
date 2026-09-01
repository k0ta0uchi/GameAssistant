use serde::{Deserialize, Serialize};
use sysinfo::System;
use nvml_wrapper::Nvml;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceInfo {
    pub used: f64,    // MB
    pub total: f64,   // MB
    pub percent: f64, // %
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemResources {
    pub ram: ResourceInfo,
    pub vram: ResourceInfo,
}

pub struct ResourceManager {
    sys: Mutex<System>,
    nvml: Option<Nvml>,
}

impl ResourceManager {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_memory();

        let nvml = Nvml::init().ok();
        Self {
            sys: Mutex::new(sys),
            nvml,
        }
    }

    pub fn get_resources(&self) -> SystemResources {
        // RAM 取得
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_memory();

        let total_ram_bytes = sys.total_memory();
        let used_ram_bytes = sys.used_memory();
        let ram_total_mb = (total_ram_bytes as f64) / (1024.0 * 1024.0);
        let ram_used_mb = (used_ram_bytes as f64) / (1024.0 * 1024.0);
        let ram_percent = if total_ram_bytes > 0 {
            (ram_used_mb / ram_total_mb) * 100.0
        } else {
            0.0
        };

        // VRAM 取得
        let (vram_used_mb, vram_total_mb, vram_percent) = if let Some(ref nvml) = self.nvml {
            if let Ok(device) = nvml.device_by_index(0) {
                if let Ok(mem_info) = device.memory_info() {
                    let total = (mem_info.total as f64) / (1024.0 * 1024.0);
                    let used = (mem_info.used as f64) / (1024.0 * 1024.0);
                    let pct = if total > 0.0 { (used / total) * 100.0 } else { 0.0 };
                    (used, total, pct)
                } else {
                    (0.0, 0.0, 0.0)
                }
            } else {
                (0.0, 0.0, 0.0)
            }
        } else {
            (0.0, 0.0, 0.0)
        };

        SystemResources {
            ram: ResourceInfo {
                used: (ram_used_mb * 10.0).round() / 10.0,
                total: (ram_total_mb * 10.0).round() / 10.0,
                percent: (ram_percent * 10.0).round() / 10.0,
            },
            vram: ResourceInfo {
                used: (vram_used_mb * 10.0).round() / 10.0,
                total: (vram_total_mb * 10.0).round() / 10.0,
                percent: (vram_percent * 10.0).round() / 10.0,
            },
        }
    }
}
