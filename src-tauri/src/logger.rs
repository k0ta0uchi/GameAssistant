use chrono::Local;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use tauri::{AppHandle, Emitter};

/// File logs are rotated daily and kept for this many days; older files are
/// deleted by the logging worker. Bounded retention keeps `logs/` from
/// growing without limit (Issue #28).
const LOG_RETENTION_DAYS: i64 = 14;

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
    /// Common logging pipeline fan-out: the background file-logger worker
    /// consumes formatted lines through this channel so call sites (and low
    /// latency paths like ASR/TTS/audio) never block on file I/O.
    file_tx: OnceLock<mpsc::Sender<String>>,
    file_queued: Arc<AtomicU64>,
    file_written: Arc<AtomicU64>,
}

impl LogManager {
    pub fn new(runtime_root: std::path::PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<String>();
        let written = Arc::new(AtomicU64::new(0));
        let logs_dir = runtime_root.join("logs");
        let worker_written = Arc::clone(&written);
        // Dedicated worker thread: owns all file I/O for logging. If spawning
        // fails we simply lose file logging; the app keeps running.
        let _ = std::thread::Builder::new()
            .name("file-logger".to_string())
            .spawn(move || file_logger_worker(logs_dir, rx, worker_written));
        let manager = Self {
            logs: Arc::new(Mutex::new(Vec::with_capacity(1000))),
            app_handle: Arc::new(Mutex::new(None)),
            file_tx: OnceLock::new(),
            file_queued: Arc::new(AtomicU64::new(0)),
            file_written: written,
        };
        let _ = manager.file_tx.set(tx);
        manager
    }

    pub fn set_app_handle(&self, handle: AppHandle) {
        *self.app_handle.lock() = Some(handle);
    }

    pub fn log(&self, level: &str, logger_name: &str, message: &str) {
        // Single redaction point in the common pipeline: GUI, stdout and the
        // file log all receive the redacted message, so credentials never
        // reach any log surface (Issue #28).
        let message = redact_message(message);
        let entry = LogEntry {
            r#type: "log".to_string(),
            timestamp: Local::now().format("%H:%M:%S.%.3f").to_string(),
            level: level.to_string(),
            logger: logger_name.to_string(),
            message: message.clone(),
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

        // Always write beneath the injected portable runtime root (logs/):
        // never resolve this path from the process CWD or a user profile
        // directory. The dedicated worker performs the file I/O; the send is
        // non-blocking and I/O failures are swallowed inside the worker.
        let file_line = format!(
            "{} [{}] [{}] {}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            entry.level,
            entry.logger,
            entry.message
        );
        if let Some(tx) = self.file_tx.get() {
            if tx.send(file_line).is_ok() {
                self.file_queued.fetch_add(1, Ordering::Relaxed);
            }
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

    /// Best-effort drain used by tests: waits until the file-logger worker
    /// has written every queued line (or until a short timeout). Production
    /// code never needs this; the worker flushes each line immediately.
    pub fn wait_until_file_flushed(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.file_written.load(Ordering::Relaxed) < self.file_queued.load(Ordering::Relaxed) {
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Background file-logger worker: owns the daily log file
/// (`logs/GameAssistant-YYYY-MM-DD.log`), flushes per line (on its own
/// thread, so callers never block), rotates at date change and prunes logs
/// older than [`LOG_RETENTION_DAYS`]. Every I/O error is swallowed: file
/// logging must never panic or stop the application.
fn file_logger_worker(
    logs_dir: std::path::PathBuf,
    rx: mpsc::Receiver<String>,
    written: Arc<AtomicU64>,
) {
    cleanup_old_logs(&logs_dir);
    let mut current_day: Option<String> = None;
    let mut file: Option<std::fs::File> = None;
    while let Ok(line) = rx.recv() {
        let today = Local::now().format("%Y-%m-%d").to_string();
        if file.is_none() || current_day.as_deref() != Some(today.as_str()) {
            // Rotate to today's file (also re-attempts directory creation,
            // so a transiently unwritable logs dir recovers on its own).
            if std::fs::create_dir_all(&logs_dir).is_err() {
                written.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let path = logs_dir.join(format!("GameAssistant-{today}.log"));
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(handle) => {
                    file = Some(handle);
                    current_day = Some(today);
                    cleanup_old_logs(&logs_dir);
                }
                Err(_) => {
                    // Drop this line and keep serving; the next line retries
                    // the open. Logging must never take the app down.
                    written.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
        if let Some(handle) = file.as_mut() {
            let _ = writeln!(handle, "{line}");
            // Flush here, on the worker thread: crash windows stay tiny
            // without ever blocking the logging call sites.
            let _ = handle.flush();
        }
        written.fetch_add(1, Ordering::Relaxed);
    }
}

/// Deletes `GameAssistant-YYYY-MM-DD.log` files older than the retention
/// window. Best-effort: every error is ignored.
fn cleanup_old_logs(logs_dir: &std::path::Path) {
    let cutoff = (Local::now().date_naive() - chrono::Duration::days(LOG_RETENTION_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(date) = name
            .strip_prefix("GameAssistant-")
            .and_then(|rest| rest.strip_suffix(".log"))
        else {
            continue;
        };
        // ISO dates compare correctly as plain strings.
        if date < cutoff.as_str() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Redacts credentials from a log message at the single common pipeline
/// entry, before any fan-out (GUI / stdout / file). Covers the credentials
/// listed in Issue #28 plus generic labeled secrets that show up in URLs,
/// JSON bodies and key=value debug output.
fn redact_message(message: &str) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            // Gemini API keys (AIza...) wherever they appear, including URLs.
            (r"AIza[0-9A-Za-z_\-]{10,}", "[REDACTED]"),
            // Authorization headers (with or without the Bearer scheme).
            (
                r"(?i)(authorization\s*:\s*)(bearer\s+)?\S+",
                "${1}[REDACTED]",
            ),
            (r"(?i)bearer\s+[A-Za-z0-9._\-]+", "bearer [REDACTED]"),
            // Labeled credentials in URLs, JSON bodies and key=value output.
            // The value run stops at '&', whitespace, ',' or '}' so URL
            // query strings stay parseable in the redacted log.
            (
                r#"(?i)\b(access_token|refresh_token|id_token|client_secret|api[_-]?key|apikey|secret|password|token)\b(\s*[=:]\s*)[^&\s,}]+"#,
                "${1}${2}[REDACTED]",
            ),
            // Cookie headers.
            (r"(?i)(cookie\s*:\s*).+", "${1}[REDACTED]"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                Regex::new(pattern).expect("valid redaction pattern"),
                replacement.to_string(),
            )
        })
        .collect()
    });
    let mut redacted = message.to_string();
    for (pattern, replacement) in patterns.iter() {
        redacted = pattern
            .replace_all(&redacted, replacement.as_str())
            .into_owned();
    }
    redacted
}

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
    use super::{cleanup_old_logs, LogManager};
    use chrono::Local;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn unique_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "gameassistant-logger-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn today_log_path(root: &std::path::Path) -> std::path::PathBuf {
        let today = Local::now().format("%Y-%m-%d").to_string();
        root.join("logs").join(format!("GameAssistant-{today}.log"))
    }

    #[test]
    fn storage_writes_daily_log_under_the_injected_runtime_root() {
        let root = unique_root("storage");
        let unrelated = root.join("unrelated-cwd");
        fs::create_dir_all(&unrelated).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&unrelated).unwrap();

        let logger = LogManager::new(root.clone());
        logger.info("Test", "rooted log");
        logger.wait_until_file_flushed();

        std::env::set_current_dir(original).unwrap();
        assert!(today_log_path(&root).is_file());
        assert!(!unrelated.join("logs").exists());
        let content = fs::read_to_string(today_log_path(&root)).unwrap();
        // ファイル行は日付入フルタイムスタンプを持つ
        let today = Local::now().format("%Y-%m-%d").to_string();
        assert!(content.contains(&today));
        assert!(content.contains("[INFO] [Test] rooted log"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn redacts_credentials_before_gui_and_file_fanout() {
        let root = unique_root("redaction");
        let logger = LogManager::new(root.clone());
        logger.info(
            "Test",
            "gemini key AIzaSyA1234567890abcdefghij, access_token=supersecret, Authorization: Bearer abc.def.ghi, cookie: SID=xyz; HttpOnly",
        );
        logger.wait_until_file_flushed();

        let entries = logger.get_logs();
        assert_eq!(entries.len(), 1);
        let message = &entries[0].message;
        assert!(!message.contains("AIzaSyA"), "gemini key leaked: {message}");
        assert!(
            !message.contains("supersecret"),
            "access token leaked: {message}"
        );
        assert!(
            !message.contains("abc.def.ghi"),
            "bearer token leaked: {message}"
        );
        assert!(!message.contains("SID=xyz"), "cookie leaked: {message}");
        assert!(message.contains("[REDACTED]"));

        let content = fs::read_to_string(today_log_path(&root)).unwrap();
        assert!(!content.contains("supersecret"));
        assert!(!content.contains("AIzaSyA"));
        assert!(content.contains("[REDACTED]"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_removes_only_logs_older_than_retention() {
        let root = unique_root("retention");
        let logs_dir = root.join("logs");
        fs::create_dir_all(&logs_dir).unwrap();
        fs::write(logs_dir.join("GameAssistant-2020-01-01.log"), "old").unwrap();
        fs::write(logs_dir.join("GameAssistant-unrelated.log"), "keep").unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        fs::write(logs_dir.join(format!("GameAssistant-{today}.log")), "keep").unwrap();

        cleanup_old_logs(&logs_dir);

        assert!(!logs_dir.join("GameAssistant-2020-01-01.log").exists());
        assert!(logs_dir.join("GameAssistant-unrelated.log").exists());
        assert!(logs_dir.join(format!("GameAssistant-{today}.log")).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unwritable_logs_dir_does_not_panic() {
        let root = unique_root("unwritable");
        fs::create_dir_all(&root).unwrap();
        // logs/ をファイルで潰して書き込み不能にする
        fs::write(root.join("logs"), "not a directory").unwrap();
        let logger = LogManager::new(root.clone());
        logger.info("Test", "must not panic");
        logger.wait_until_file_flushed();
        // panic せずここまで到達し、GUI 側のリングには記録されている
        assert_eq!(logger.get_logs().len(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
