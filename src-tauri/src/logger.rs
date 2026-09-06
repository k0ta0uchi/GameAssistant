use chrono::Local;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub r#type: String,
    pub timestamp: String,
    pub level: String, // "DEBUG" | "INFO" | "WARNING" | "ERROR" | "CRITICAL"
    pub logger: String,
    pub message: String,
}

pub struct LogManager {
    logs: Arc<Mutex<Vec<LogEntry>>>,
    app_handle: Arc<Mutex<Option<AppHandle>>>,
    runtime_root: std::path::PathBuf,
}

impl LogManager {
    pub fn new(runtime_root: std::path::PathBuf) -> Self {
        Self {
            logs: Arc::new(Mutex::new(Vec::with_capacity(1000))),
            app_handle: Arc::new(Mutex::new(None)),
            runtime_root,
        }
    }

    pub fn set_app_handle(&self, handle: AppHandle) {
        *self.app_handle.lock() = Some(handle);
    }

    pub fn log(&self, level: &str, logger_name: &str, message: &str) {
        let entry = LogEntry {
            r#type: "log".to_string(),
            timestamp: Local::now().format("%H:%M:%S.%.3f").to_string(),
            level: level.to_string(),
            logger: logger_name.to_string(),
            message: message.to_string(),
        };

        // 内部ログ履歴（最新500件）に保持
        {
            let mut lock = self.logs.lock();
            lock.push(entry.clone());
            if lock.len() > 500 {
                lock.remove(0);
            }
        }

        // 標準出力にも出力
        println!(
            "[{}] [{}] [{}] {}",
            entry.timestamp, entry.level, entry.logger, entry.message
        );

        // Always write beneath the injected portable runtime root.  In
        // particular, never resolve this path from the process CWD or a user
        // profile directory: portable installs may be launched from either.
        let log_path = self.runtime_root.join("data").join("app.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
        {
            use std::io::Write;
            let _ = writeln!(
                file,
                "[{}] [{}] [{}] {}",
                entry.timestamp, entry.level, entry.logger, entry.message
            );
        }

        // フロントエンドにリアルタイム送信 (単一の app_log イベントに一本化)
        if let Some(ref handle) = *self.app_handle.lock() {
            let _ = handle.emit("app_log", &entry);
        }
    }

    pub fn info(&self, logger_name: &str, message: &str) {
        self.log("INFO", logger_name, message);
    }

    pub fn warn(&self, logger_name: &str, message: &str) {
        self.log("WARNING", logger_name, message);
    }

    pub fn error(&self, logger_name: &str, message: &str) {
        self.log("ERROR", logger_name, message);
    }

    pub fn debug(&self, logger_name: &str, message: &str) {
        self.log("DEBUG", logger_name, message);
    }

    pub fn get_logs(&self) -> Vec<LogEntry> {
        self.logs.lock().clone()
    }

    pub fn clear(&self) {
        self.logs.lock().clear();
    }
}

use std::sync::OnceLock;

static GLOBAL_LOGGER: OnceLock<Arc<LogManager>> = OnceLock::new();

pub fn set_global_logger(mgr: Arc<LogManager>) {
    let _ = GLOBAL_LOGGER.set(mgr);
}

pub fn global_log(level: &str, logger_name: &str, message: &str) {
    if let Some(mgr) = GLOBAL_LOGGER.get() {
        mgr.log(level, logger_name, message);
    } else {
        println!("[{}] [{}] {}", level, logger_name, message);
    }
}

pub fn global_info(logger_name: &str, message: &str) {
    global_log("INFO", logger_name, message);
}

pub fn global_warn(logger_name: &str, message: &str) {
    global_log("WARNING", logger_name, message);
}

pub fn global_error(logger_name: &str, message: &str) {
    global_log("ERROR", logger_name, message);
}

pub fn global_debug(logger_name: &str, message: &str) {
    global_log("DEBUG", logger_name, message);
}

#[cfg(test)]
mod tests {
    use super::LogManager;
    use std::fs;

    #[test]
    fn writes_app_log_under_the_injected_runtime_root() {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-logger-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let unrelated = root.join("unrelated-cwd");
        fs::create_dir_all(&unrelated).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&unrelated).unwrap();

        let logger = LogManager::new(root.clone());
        logger.info("Test", "rooted log");

        std::env::set_current_dir(original).unwrap();
        assert!(root.join("data").join("app.log").is_file());
        assert!(!unrelated.join("data").join("app.log").exists());
        let _ = fs::remove_dir_all(root);
    }
}
