//! General application logging for ovim
//!
//! This module provides safe logging that NEVER prints to stdout/stderr in TUI mode.
//! All logs go to ~/.cache/ovim/ovim.log (or platform equivalent).
//!
//! For LSP-specific logging, use the LSP logger in lsp::logger.
//!
//! Usage:
//!   log_info!("session", "Started session {}", name);
//!   log_warn!("config", "Config file not found");
//!   log_error!("buffer", "Failed to parse: {}", error);

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
}

/// Initialize the log file (creates the directory if needed)
pub fn init() -> std::io::Result<()> {
    let log_path = get_log_path();

    // Create parent directory if needed
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // Handle mutex poisoning gracefully by recovering the guard
    let mut log_file = match LOG_FILE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *log_file = Some(file);

    Ok(())
}

/// Publicly visible log file location, for user-facing "see the log" hints.
pub fn log_file_path() -> PathBuf {
    get_log_path()
}

/// Get the log file path
fn get_log_path() -> PathBuf {
    let mut path = if let Some(cache_dir) = dirs::cache_dir() {
        cache_dir
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".cache")
    } else {
        std::path::PathBuf::from("/tmp")
    };

    path.push("ovim");
    path.push("ovim.log");
    path
}

/// Write a log message (internal function, but must be pub for macros)
#[doc(hidden)]
pub fn write_log(level: &str, context: &str, message: &str) {
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let log_line = format!("[{}] [{}] [{}] {}\n", timestamp, level, context, message);

    // Try to use the persistent file handle first
    let mut should_use_fallback = true;
    if let Ok(mut log_file) = LOG_FILE.lock() {
        if let Some(ref mut file) = *log_file {
            if file.write_all(log_line.as_bytes()).is_ok() {
                let _ = file.flush();
                should_use_fallback = false;
            }
        }
    }

    // Fallback: open file on-demand if handle is not available or write failed
    if should_use_fallback {
        let log_path = get_log_path();
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = file.write_all(log_line.as_bytes());
            let _ = file.flush();
        }
    }
}

/// Log an info message
#[macro_export]
macro_rules! log_info {
    ($context:expr, $($arg:tt)*) => {
        $crate::log::write_log("INFO", $context, &format!($($arg)*))
    };
}

/// Log a warning message
#[macro_export]
macro_rules! log_warn {
    ($context:expr, $($arg:tt)*) => {
        $crate::log::write_log("WARN", $context, &format!($($arg)*))
    };
}

/// Log an error message
#[macro_export]
macro_rules! log_error {
    ($context:expr, $($arg:tt)*) => {
        $crate::log::write_log("ERROR", $context, &format!($($arg)*))
    };
}

/// Log a debug message (only when OVIM_DEBUG env var is set)
#[macro_export]
macro_rules! log_debug {
    ($context:expr, $($arg:tt)*) => {
        if std::env::var("OVIM_DEBUG").is_ok() {
            $crate::log::write_log("DEBUG", $context, &format!($($arg)*))
        }
    };
}
