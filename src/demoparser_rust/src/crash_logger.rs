//! Crash logging system for debugging parser failures
//!
//! Provides panic hooks and structured logging to help diagnose crashes

use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Global logger instance
static CRASH_LOGGER: Mutex<Option<CrashLogger>> = Mutex::new(None);

/// Crash logger that writes to timestamped log files
pub struct CrashLogger {
    log_file: Arc<Mutex<std::fs::File>>,
    log_path: PathBuf,
}

impl CrashLogger {
    /// Initialize the crash logger
    pub fn initialize() -> Result<(), Box<dyn std::error::Error>> {
        // Create crash_logs directory
        let log_dir = PathBuf::from("crash_logs");
        fs::create_dir_all(&log_dir)?;

        // Create timestamped log file
        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let log_path = log_dir.join(format!("session_{}.log", timestamp));

        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        let logger = CrashLogger {
            log_file: Arc::new(Mutex::new(log_file)),
            log_path: log_path.clone(),
        };

        // Store logger globally
        *CRASH_LOGGER.lock().unwrap() = Some(logger);

        // Set up panic hook
        let log_file_clone = CRASH_LOGGER
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .log_file
            .clone();
        let crash_log_path = log_path
            .parent()
            .unwrap()
            .join(format!("crash_{}.log", timestamp));

        panic::set_hook(Box::new(move |panic_info| {
            // Write to crash log
            let separator = "=".repeat(80);
            let crash_msg = format!(
                "\n{}\nPANIC at {}\n{}\n{}\n\nBacktrace:\n{:?}\n",
                separator,
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                separator,
                panic_info,
                std::backtrace::Backtrace::force_capture()
            );

            // Write to crash-specific file
            if let Ok(mut crash_file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&crash_log_path)
            {
                let _ = crash_file.write_all(crash_msg.as_bytes());
                let _ = crash_file.flush();
            }

            // Also write to session log
            if let Ok(mut file) = log_file_clone.lock() {
                let _ = file.write_all(crash_msg.as_bytes());
                let _ = file.flush();
            }

            // Print to stderr
            eprintln!("{}", crash_msg);
            eprintln!("\n🔴 CRASH LOG WRITTEN TO: {}", crash_log_path.display());
        }));

        log_info("Crash logger initialized");

        Ok(())
    }
}

/// Log an info message
pub fn log_info(message: &str) {
    log_message("INFO", message);
}

/// Log a warning message
pub fn log_warning(message: &str) {
    log_message("WARN", message);
}

/// Log an error message
pub fn log_error(message: &str) {
    log_message("ERROR", message);
}

/// Log a debug message
pub fn log_debug(message: &str) {
    log_message("DEBUG", message);
}

/// Log batch start
pub fn log_batch_start(batch_num: usize, total_batches: usize, file_count: usize) {
    log_info(&format!(
        ">>> BATCH {}/{} START - {} files",
        batch_num, total_batches, file_count
    ));
}

/// Log batch end
pub fn log_batch_end(batch_num: usize, collections_written: usize, elapsed: f64) {
    log_info(&format!(
        "<<< BATCH {} END - {} collections written in {:.2}s",
        batch_num, collections_written, elapsed
    ));
}

/// Log buffer state
pub fn log_buffer_state(collections_count: usize, tickbytick_completed: usize) {
    log_debug(&format!(
        "Buffer state: {} total collections ({} with tickbytick)",
        collections_count, tickbytick_completed
    ));
}

/// Log DuckDB write start
pub fn log_duckdb_write_start(file_path: &str, collections_count: usize) {
    log_info(&format!(
        "DuckDB write START: {} ({} collections)",
        file_path, collections_count
    ));
}

/// Log DuckDB write end
pub fn log_duckdb_write_end(file_path: &str, success: bool) {
    if success {
        log_info(&format!("DuckDB write SUCCESS: {}", file_path));
    } else {
        log_error(&format!("DuckDB write FAILED: {}", file_path));
    }
}

/// Internal logging function
fn log_message(level: &str, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let log_line = format!("[{}] [{}] {}\n", timestamp, level, message);

    // Write to file
    if let Ok(logger_lock) = CRASH_LOGGER.lock() {
        if let Some(logger) = logger_lock.as_ref() {
            if let Ok(mut file) = logger.log_file.lock() {
                let _ = file.write_all(log_line.as_bytes());
                let _ = file.flush();
            }
        }
    }

    // Also print to stdout for important messages
    if level == "ERROR" || level == "WARN" {
        eprint!("{}", log_line);
    } else if level == "INFO" {
        print!("{}", log_line);
    }
}
