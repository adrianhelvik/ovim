//! LSP logging infrastructure
//!
//! Logs LSP messages to a bounded private file instead of stderr/stdout, which
//! would corrupt the TUI.

use std::path::PathBuf;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref LSP_LOG_FILE: Mutex<Option<crate::diagnostic_log::DiagnosticLog>> = Mutex::new(None);
}

const LSP_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Initialize LSP logging to a file
pub fn init_lsp_logging() -> std::io::Result<()> {
    let log_path = get_log_path();

    let file = crate::diagnostic_log::DiagnosticLog::open(&log_path, LSP_LOG_MAX_BYTES)?;

    // Handle mutex poisoning gracefully by recovering the guard
    let mut log_file = match LSP_LOG_FILE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *log_file = Some(file);

    Ok(())
}

/// Get the path to the LSP log file
pub fn get_log_path() -> PathBuf {
    crate::diagnostic_log::log_path("lsp.log")
}

/// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// Log a message to the LSP log file
pub fn log_message(level: LogLevel, context: &str, message: &str) {
    // Only log debug messages if OVIM_LSP_DEBUG is set
    if level == LogLevel::Debug && std::env::var("OVIM_LSP_DEBUG").is_err() {
        return;
    }

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let log_line = format!(
        "[{}] [{}] [{}] {}\n",
        timestamp,
        level.as_str(),
        context,
        message
    );

    if let Ok(mut log_file) = LSP_LOG_FILE.lock() {
        if let Some(ref mut file) = *log_file {
            let _ = file.write_record(&log_line);
        }
    }
}

/// Convenience macros for logging
#[macro_export]
macro_rules! lsp_debug {
    ($context:expr, $($arg:tt)*) => {
        $crate::lsp::logger::log_message(
            $crate::lsp::logger::LogLevel::Debug,
            $context,
            &format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! lsp_info {
    ($context:expr, $($arg:tt)*) => {
        $crate::lsp::logger::log_message(
            $crate::lsp::logger::LogLevel::Info,
            $context,
            &format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! lsp_warn {
    ($context:expr, $($arg:tt)*) => {
        $crate::lsp::logger::log_message(
            $crate::lsp::logger::LogLevel::Warning,
            $context,
            &format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! lsp_error {
    ($context:expr, $($arg:tt)*) => {
        $crate::lsp::logger::log_message(
            $crate::lsp::logger::LogLevel::Error,
            $context,
            &format!($($arg)*)
        )
    };
}
