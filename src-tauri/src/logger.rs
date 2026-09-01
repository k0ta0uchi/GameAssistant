use std::sync::Arc;
use chrono::Local;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
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
}

impl LogManager {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(Mutex::new(Vec::with_capacity(1000))),
            app_handle: Arc::new(Mutex::new(None)),
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

        // data/app.log に追記出力 (AIアシスタントやデバッグ用)
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open("data/app.log") {
            use std::io::Write;
            let _ = writeln!(file, "[{}] [{}] [{}] {}", entry.timestamp, entry.level, entry.logger, entry.message);
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

