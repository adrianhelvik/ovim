//! Session management for headless mode
//!
//! Manages session files in ~/.cache/ovim/sessions/ that allow:
//! - Port discovery for running instances
//! - Session naming for multiple concurrent instances
//! - Automatic cleanup on exit
//! - Status checking (LSP ready, etc.)
//!
//! Client-side messages use stderr for user feedback.
#![allow(clippy::print_stderr)]

use anyhow::{Context, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn ensure_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("Failed to secure session directory")?;
    Ok(())
}

/// Information about a running headless session
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SessionCapability(String);

impl SessionCapability {
    /// Generate a 256-bit capability from the operating system CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use fmt::Write as _;
            write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        Self(encoded)
    }

    /// Expose the capability only at an authenticated transport boundary.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn is_configured(&self) -> bool {
        !self.0.is_empty()
    }
}

impl fmt::Debug for SessionCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionCapability(<redacted>)")
    }
}

/// Information about a running headless session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Process ID of the ovim instance
    pub pid: u32,

    /// Port the API server is listening on
    pub port: u16,

    /// File being edited
    pub file: Option<String>,

    /// When the session was started (Unix timestamp)
    pub started_at: u64,

    /// Session name (default: "default")
    pub session_name: String,

    /// Bearer capability for this session's loopback automation API.
    ///
    /// The descriptor is owner-private. Debug output is always redacted.
    #[serde(default)]
    pub capability: SessionCapability,

    /// Whether LSP is ready
    pub lsp_ready: bool,

    /// Process start time (system boot time + ticks) for PID verification
    /// This prevents PID reuse race conditions
    pub start_time: Option<u64>,

    /// Logical viewport used by headless rendering and motion commands.
    #[serde(default)]
    pub viewport_width: Option<u16>,
    #[serde(default)]
    pub viewport_height: Option<u16>,
}

impl SessionInfo {
    /// Create a new session info
    pub fn new(port: u16, file: Option<String>, session_name: String) -> Self {
        let started_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();

        let pid = process::id();
        let start_time = get_process_start_time(pid);

        Self {
            pid,
            port,
            file,
            started_at,
            session_name,
            capability: SessionCapability::generate(),
            lsp_ready: false,
            start_time,
            viewport_width: None,
            viewport_height: None,
        }
    }

    pub fn with_dimensions(mut self, width: u16, height: u16) -> Self {
        self.viewport_width = Some(width);
        self.viewport_height = Some(height);
        self
    }

    pub fn with_capability(mut self, capability: SessionCapability) -> Self {
        self.capability = capability;
        self
    }

    pub fn dimensions(&self) -> Option<(u16, u16)> {
        Some((self.viewport_width?, self.viewport_height?))
    }

    pub fn set_dimensions(&mut self, width: u16, height: u16) -> Result<()> {
        self.viewport_width = Some(width);
        self.viewport_height = Some(height);
        self.write()
    }

    /// Get the session directory path
    pub fn session_dir() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("OVIM_SESSION_DIR") {
            let session_dir = PathBuf::from(dir);
            ensure_private_directory(&session_dir)?;
            return Ok(session_dir);
        }

        let cache_dir = dirs::cache_dir().context("Failed to get cache directory")?;

        let session_dir = cache_dir.join("ovim").join("sessions");
        ensure_private_directory(&session_dir)?;

        Ok(session_dir)
    }

    /// Get the path for this session's file
    pub fn session_file_path(&self) -> Result<PathBuf> {
        let session_dir = Self::session_dir()?;
        Ok(self.session_file_path_in(&session_dir))
    }

    /// Get this session's file path in an explicit session directory.
    pub fn session_file_path_in(&self, session_dir: &Path) -> PathBuf {
        session_dir.join(format!("{}.json", self.session_name))
    }

    /// Write this session info to disk atomically
    pub fn write(&self) -> Result<()> {
        let session_dir = Self::session_dir()?;
        self.write_to_dir(&session_dir)
    }

    /// Write this session info atomically to an explicit session directory.
    ///
    /// Rejects names that [`Self::validate_session_name`] would refuse on
    /// read, so a session can never be created that is impossible to read
    /// back or target later.
    pub fn write_to_dir(&self, session_dir: &Path) -> Result<()> {
        use std::io::Write;

        Self::validate_session_name(&self.session_name)?;
        ensure_private_directory(session_dir)?;
        let path = self.session_file_path_in(session_dir);
        let json = serde_json::to_string_pretty(self)?;

        let (temp_path, mut temp_file) = (0..8)
            .find_map(|_| {
                let nonce = OsRng.next_u64();
                let temp_path = session_dir.join(format!(".{}.{}.tmp", self.session_name, nonce));
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                options.mode(0o600);
                match options.open(&temp_path) {
                    Ok(file) => Some(Ok((temp_path, file))),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()
            .context("Failed to create private session staging file")?
            .context("Failed to allocate a unique session staging file")?;

        let staged = (|| -> Result<()> {
            temp_file
                .write_all(json.as_bytes())
                .context("Failed to write to temporary session file")?;
            temp_file
                .flush()
                .context("Failed to flush temporary session file")?;
            temp_file
                .sync_all()
                .context("Failed to sync session file to disk")?;
            Ok(())
        })();
        if let Err(error) = staged {
            drop(temp_file);
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        drop(temp_file);

        if let Err(error) = fs::rename(&temp_path, &path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error).context("Failed to rename session file");
        }

        Ok(())
    }

    /// Update LSP ready status
    pub fn set_lsp_ready(&mut self, ready: bool) -> Result<()> {
        self.lsp_ready = ready;
        self.write()
    }

    /// Delete this session file
    pub fn delete(&self) -> Result<()> {
        let session_dir = Self::session_dir()?;
        self.delete_from_dir(&session_dir)
    }

    /// Delete this session's file from an explicit session directory.
    pub fn delete_from_dir(&self, session_dir: &Path) -> Result<()> {
        let path = self.session_file_path_in(session_dir);

        // Remove file directly without checking exists() first to avoid TOCTOU race.
        // An attacker could replace the file with a symlink between the check and removal.
        // remove_file is idempotent - if the file doesn't exist, we treat that as success.
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Validate a session name before it is used as a file stem.
    ///
    /// Names must be non-empty, at most 64 characters, and contain only
    /// alphanumerics, underscores, and hyphens. This is the single source
    /// of truth for session names: creation paths (headless `--session`,
    /// `:session start`, and [`Self::write_to_dir`]) and read paths all
    /// enforce it, so any session that can be created can also be read,
    /// and traversal-capable names like `../../evil` are rejected.
    pub fn validate_session_name(session_name: &str) -> Result<()> {
        let valid = !session_name.is_empty()
            && session_name.chars().count() <= 64
            && session_name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if valid {
            Ok(())
        } else {
            anyhow::bail!(
                "Invalid session name '{}': session names may only contain \
                 alphanumeric characters, underscores, and hyphens (max 64 characters)",
                session_name
            )
        }
    }

    /// Read a session by name with helpful error messages
    pub fn read(session_name: &str) -> Result<Self> {
        let session_dir = Self::session_dir()?;
        Self::read_from_dir(session_name, &session_dir)
    }

    /// Read a session by name from an explicit session directory.
    pub fn read_from_dir(session_name: &str, session_dir: &Path) -> Result<Self> {
        Self::validate_session_name(session_name)?;
        let path = session_dir.join(format!("{}.json", session_name));

        // Try to read the file
        let json = fs::read_to_string(&path).context(format!(
            "Session '{}' not found.\n\nAvailable sessions:\n{}",
            session_name,
            Self::format_available_sessions_from_dir(session_dir)
        ))?;

        // Try to parse JSON
        let info: SessionInfo = serde_json::from_str(&json).context(format!(
            "Session file for '{}' is corrupted. Run 'ovim cleanup' to remove invalid sessions.",
            session_name
        ))?;

        // Check if process is still running and matches start time
        if !is_process_alive_with_start_time(info.pid, info.start_time) {
            // Clean up stale session file
            let _ = fs::remove_file(&path);
            anyhow::bail!(
                "Session '{}' is not running (process {} terminated).\n\
                 Stale session file has been cleaned up.\n\
                 \n\
                 Active sessions:\n{}",
                session_name,
                info.pid,
                Self::format_available_sessions_from_dir(session_dir)
            );
        }

        Ok(info)
    }

    /// Format sessions from a specific directory as a helpful error message.
    fn format_available_sessions_from_dir(session_dir: &Path) -> String {
        match Self::list_all_from_dir(session_dir) {
            Ok(sessions) if !sessions.is_empty() => {
                let mut output = String::new();
                for session in sessions {
                    output.push_str(&format!(
                        "  - {} (PID {}, port {})\n",
                        session.session_name, session.pid, session.port
                    ));
                }
                output
            }
            _ => "  (none - start a session with: ovim <file> --headless --session <name>)\n"
                .to_string(),
        }
    }

    /// List all active sessions
    pub fn list_all() -> Result<Vec<SessionInfo>> {
        let session_dir = Self::session_dir()?;
        Self::list_all_from_dir(&session_dir)
    }

    fn list_all_from_dir(session_dir: &Path) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();

        for entry in fs::read_dir(session_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            if let Ok(json) = fs::read_to_string(&path) {
                if let Ok(info) = serde_json::from_str::<SessionInfo>(&json) {
                    // Only include if process is still alive and matches start time
                    if is_process_alive_with_start_time(info.pid, info.start_time) {
                        sessions.push(info);
                    } else {
                        // Clean up stale file
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }

        sessions.sort_by_key(|s| s.started_at);
        Ok(sessions)
    }

    /// Check if session is expired (older than max_age)
    ///
    /// # Arguments
    /// * `max_age` - Maximum session age before considered expired
    ///
    /// # Educational Note
    /// Session expiry is useful for cleaning up long-running sessions that may have been
    /// forgotten. However, be careful with automatic expiry - some users may intentionally
    /// keep sessions running for days/weeks (e.g., remote development, long-running tasks).
    /// Default expiry should be conservative (7+ days).
    pub fn is_expired(&self, max_age: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let age = now.saturating_sub(self.started_at);
        age > max_age.as_secs()
    }

    /// Health check: verify session is actually responding
    ///
    /// This goes beyond PID checking - it verifies the API endpoint is accessible.
    /// This is crucial because:
    /// 1. Process might exist but be hung/deadlocked
    /// 2. Process might exist but API server failed to start
    /// 3. Port might be bound by a different process (PID reuse + port reuse race)
    ///
    /// # Returns
    /// * `Ok(true)` - Session is healthy and responding
    /// * `Ok(false)` - Session exists but not responding
    /// * `Err(_)` - Network/connection error
    ///
    /// # Educational Note
    /// Health checks are essential for distributed systems. A process existing (PID check)
    /// doesn't mean it's functional. Always verify the actual service endpoint when possible.
    /// This is why Kubernetes has liveness probes, not just PID checks.
    pub fn check_health(&self) -> Result<bool> {
        // First check if process is alive (fast check)
        if !is_process_alive_with_start_time(self.pid, self.start_time) {
            return Ok(false);
        }

        // Then verify API endpoint is responding (comprehensive check)
        // Use a short timeout to avoid blocking cleanup operations
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()?;

        let health_url = format!("http://127.0.0.1:{}/health", self.port);

        match client.get(&health_url).send() {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false), // Network error = unhealthy
        }
    }

    /// Get human-readable session age
    pub fn age(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        Duration::from_secs(now.saturating_sub(self.started_at))
    }
}

/// Get the process start time for a given PID
/// Returns the start time in a platform-specific format that can be compared for equality
#[cfg(target_os = "linux")]
fn get_process_start_time(pid: u32) -> Option<u64> {
    use std::fs;

    // Read /proc/[pid]/stat
    let stat_path = format!("/proc/{}/stat", pid);
    let stat_content = fs::read_to_string(stat_path).ok()?;

    // The stat file format is: pid (comm) state ppid ... starttime ...
    // We need to handle the comm field which can contain spaces and parentheses
    // Find the last ')' to skip the comm field
    let start_pos = stat_content.rfind(')')?;
    let fields: Vec<&str> = stat_content[start_pos + 1..].split_whitespace().collect();

    // starttime is the 22nd field overall, but we've skipped pid and comm
    // After ')' we have: state ppid pgrp session tty_nr tpgid flags ...
    // starttime is at index 19 (0-indexed) after the ')'
    if fields.len() > 19 {
        fields[19].parse::<u64>().ok()
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_process_start_time(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let bytes_written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };

    if bytes_written as usize != std::mem::size_of::<libc::proc_bsdinfo>() {
        return None;
    }

    Some(info.pbi_start_tvsec)
}

#[cfg(target_os = "windows")]
fn get_process_start_time(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, System};

    let mut system = System::new();
    system.refresh_processes();

    let process = system.process(Pid::from_u32(pid))?;
    Some(process.start_time())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn get_process_start_time(_pid: u32) -> Option<u64> {
    // On unsupported platforms, return None
    // This will fall back to PID-only checking (less safe but better than nothing)
    None
}

/// Check if a process is still alive and matches the expected start time
/// This prevents PID reuse race conditions
fn is_process_alive_with_start_time(pid: u32, expected_start_time: Option<u64>) -> bool {
    // First check if process exists
    if !is_process_exists(pid) {
        return false;
    }

    // If we have an expected start time, verify it matches
    if let Some(expected) = expected_start_time {
        if let Some(actual) = get_process_start_time(pid) {
            // Allow small variance (1-2 seconds) due to timing differences
            // between when we captured the start time and when the process actually started
            let diff = actual.abs_diff(expected);

            // If difference is more than 2 seconds, it's likely a different process
            if diff > 2 {
                return false;
            }
        } else {
            // If we can't get the start time but expected one, be conservative
            // and assume it might be a different process
            return false;
        }
    }

    true
}

/// Check if a process exists (without start time verification)
#[cfg(unix)]
fn is_process_exists(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    // Signal 0 doesn't send a signal but checks if process exists
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(windows)]
fn is_process_exists(pid: u32) -> bool {
    use sysinfo::{Pid, System};

    let mut system = System::new();
    system.refresh_processes();
    system.process(Pid::from_u32(pid)).is_some()
}

#[cfg(not(any(unix, windows)))]
fn is_process_exists(_pid: u32) -> bool {
    // On other systems, conservatively assume process is alive
    true
}

/// Result of cleanup operation
#[derive(Debug, Clone)]
pub struct CleanupResult {
    /// Number of stale sessions removed (dead processes)
    pub stale_removed: usize,
    /// Number of expired sessions removed (too old)
    pub expired_removed: usize,
    /// Number of corrupted session files removed
    pub corrupted_removed: usize,
    /// Number of orphaned temp files removed
    pub temp_files_removed: usize,
    /// Details of removed sessions for logging
    pub removed_sessions: Vec<String>,
}

impl CleanupResult {
    pub fn total_removed(&self) -> usize {
        self.stale_removed + self.expired_removed + self.corrupted_removed + self.temp_files_removed
    }
}

/// Clean up all stale session files with detailed reporting
///
/// # Arguments
/// * `max_age` - Optional maximum session age. Sessions older than this are removed. None = no age limit.
/// * `dry_run` - If true, report what would be removed without actually removing
///
/// # Educational Notes
/// **Why PID checking isn't enough:**
/// PIDs can be reused by the OS. If ovim crashes and a new process gets the same PID,
/// we'd incorrectly think the session is still alive. That's why we also check:
/// 1. Process start time (from /proc or ps) - ensures it's the SAME process
/// 2. API health endpoint - ensures the process is actually functional
///
/// **Why we clean up temp files:**
/// During atomic writes, we create a unique hidden `.tmp` file and rename it
/// to the session descriptor.
/// If the process crashes between these steps, temp files are orphaned.
/// We only clean up old temp files (>1 hour) to avoid racing with active writers.
///
/// **Session expiry considerations:**
/// Some users run ovim sessions for days/weeks (remote dev, long tasks).
/// Default expiry should be conservative (7+ days). This is opt-in via --max-age.
pub fn cleanup_stale_sessions(max_age: Option<Duration>, dry_run: bool) -> Result<CleanupResult> {
    let session_dir = SessionInfo::session_dir()?;
    cleanup_stale_sessions_in(&session_dir, max_age, dry_run)
}

/// Clean stale session files in an explicit directory.
///
/// This is also useful for isolated callers and tests that must not mutate the
/// process environment to redirect session storage.
pub fn cleanup_stale_sessions_in(
    session_dir: &Path,
    max_age: Option<Duration>,
    dry_run: bool,
) -> Result<CleanupResult> {
    let mut result = CleanupResult {
        stale_removed: 0,
        expired_removed: 0,
        corrupted_removed: 0,
        temp_files_removed: 0,
        removed_sessions: Vec::new(),
    };

    // Clean up session files
    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let session_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        // Try to parse session file
        match fs::read_to_string(&path) {
            Ok(json) => {
                match serde_json::from_str::<SessionInfo>(&json) {
                    Ok(info) => {
                        let mut should_remove = false;
                        let mut reason = String::new();

                        // Check if process is dead
                        if !is_process_alive_with_start_time(info.pid, info.start_time) {
                            should_remove = true;
                            reason = format!("process {} not running", info.pid);
                            result.stale_removed += 1;
                        }
                        // Check if expired (if max_age specified)
                        else if let Some(max_age) = max_age {
                            if info.is_expired(max_age) {
                                should_remove = true;
                                let age_days = info.age().as_secs() / 86400;
                                reason = format!("expired ({} days old)", age_days);
                                result.expired_removed += 1;
                            }
                        }

                        if should_remove {
                            let detail = format!("{} ({})", session_name, reason);
                            result.removed_sessions.push(detail.clone());

                            if !dry_run {
                                // Best-effort removal - file might already be gone
                                match fs::remove_file(&path) {
                                    Ok(()) => {}
                                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                    Err(e) => {
                                        return Err(e).context(format!(
                                            "Failed to remove session file: {}",
                                            path.display()
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Corrupted session file
                        let detail = format!("{} (corrupted: {})", session_name, e);
                        result.removed_sessions.push(detail);
                        result.corrupted_removed += 1;

                        if !dry_run {
                            // Best-effort removal - file might already be gone
                            match fs::remove_file(&path) {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => {
                                    return Err(e).context(format!(
                                        "Failed to remove corrupted session file: {}",
                                        path.display()
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // Can't read file
                let detail = format!("{} (unreadable: {})", session_name, e);
                result.removed_sessions.push(detail);
                result.corrupted_removed += 1;

                if !dry_run {
                    // Best-effort removal - file might already be gone
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(e).context(format!(
                                "Failed to remove unreadable session file: {}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }
    }

    // Clean up orphaned .tmp files older than 1 hour
    // These are created during atomic writes (write + rename) but can be left behind
    // if the process crashes or the write fails between creating the temp file and renaming it
    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
            // Only delete old temp files (>1 hour old) to avoid races with concurrent writers
            if let Ok(metadata) = fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed > Duration::from_secs(3600) {
                            result.temp_files_removed += 1;

                            if !dry_run {
                                let _ = fs::remove_file(&path); // Best-effort cleanup
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Guard that ensures session cleanup on drop (even during panic)
///
/// This struct wraps SessionInfo and implements Drop to guarantee that
/// the session file is deleted when the guard goes out of scope, whether
/// due to normal exit, Ctrl+C, or panic unwinding.
///
/// # Example
///
/// ```no_run
/// use ovim_core::session::{SessionGuard, SessionInfo};
///
/// let session_info = SessionInfo::new(8080, Some("file.txt".to_string()), "dev".to_string());
/// let _guard = SessionGuard::new(session_info.clone());
/// // Session file will be cleaned up automatically when _guard is dropped
/// ```
pub struct SessionGuard {
    session_info: SessionInfo,
    session_dir: Option<PathBuf>,
}

impl SessionGuard {
    /// Create a new session guard
    pub fn new(session_info: SessionInfo) -> Self {
        Self {
            session_info,
            session_dir: None,
        }
    }

    /// Create a session guard that cleans up in an explicit session directory.
    pub fn new_in(session_info: SessionInfo, session_dir: PathBuf) -> Self {
        Self {
            session_info,
            session_dir: Some(session_dir),
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // Best-effort cleanup on panic/normal exit
        // We ignore errors since:
        // 1. We're already panicking or exiting
        // 2. The file might already be cleaned up by signal handler
        // 3. Logging during drop/panic can cause issues
        if let Some(session_dir) = &self.session_dir {
            let _ = self.session_info.delete_from_dir(session_dir);
        } else {
            let _ = self.session_info.delete();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;

    #[test]
    fn test_session_guard_cleans_up_on_drop() {
        let session_dir = tempfile::tempdir().expect("tempdir");
        let session_info =
            SessionInfo::new(9999, Some("test.txt".to_string()), "guard_test".to_string());
        session_info.write_to_dir(session_dir.path()).unwrap();

        let session_file_path = session_info.session_file_path_in(session_dir.path());
        assert!(
            session_file_path.exists(),
            "Session file should exist before drop"
        );

        // Create and immediately drop the guard
        {
            let _guard = SessionGuard::new_in(session_info.clone(), session_dir.path().into());
        }

        // Give filesystem a moment
        std::thread::sleep(std::time::Duration::from_millis(10));

        assert!(
            !session_file_path.exists(),
            "Session file should be deleted after guard drops"
        );
    }

    #[test]
    fn test_session_guard_cleans_up_on_panic() {
        let session_dir = tempfile::tempdir().expect("tempdir");
        let session_info = SessionInfo::new(
            9998,
            Some("panic_test.txt".to_string()),
            "panic_guard_test".to_string(),
        );
        session_info.write_to_dir(session_dir.path()).unwrap();

        let session_file_path = session_info.session_file_path_in(session_dir.path());
        assert!(
            session_file_path.exists(),
            "Session file should exist before panic"
        );

        // Trigger panic with guard in scope
        let result = panic::catch_unwind(|| {
            let _guard = SessionGuard::new_in(session_info.clone(), session_dir.path().into());
            panic!("Test panic!");
        });

        assert!(result.is_err(), "Panic should have occurred");

        // Give filesystem a moment
        std::thread::sleep(std::time::Duration::from_millis(10));

        assert!(
            !session_file_path.exists(),
            "Session file should be deleted after panic"
        );
    }

    #[test]
    fn test_read_rejects_path_traversal_names() {
        let session_dir = tempfile::tempdir().expect("tempdir");

        for name in ["../../x", "..", "a/b", "evil\\name", "", "dev.json"] {
            let error = SessionInfo::read_from_dir(name, session_dir.path())
                .expect_err("traversal-capable name must be rejected");
            assert!(
                error.to_string().contains("Invalid session name"),
                "unexpected error for {name:?}: {error}"
            );
        }
    }

    #[test]
    fn test_validate_session_name_accepts_written_names() {
        for name in ["default", "dev", "my_session-2", "TUI", "a"] {
            SessionInfo::validate_session_name(name).expect("valid name rejected");
        }

        let too_long = "x".repeat(65);
        assert!(SessionInfo::validate_session_name(&too_long).is_err());
    }

    #[test]
    fn test_write_rejects_names_that_reads_would_refuse() {
        let session_dir = tempfile::tempdir().expect("tempdir");

        // Overlong (65 chars), punctuation-only, empty, traversal-capable:
        // all must fail at creation, not sanitize into a different name.
        let too_long = "x".repeat(65);
        for name in [too_long.as_str(), "!!!...", "", "../../evil"] {
            let info = SessionInfo::new(9990, None, name.to_string());
            let error = info
                .write_to_dir(session_dir.path())
                .expect_err("unreadable name must be rejected at write");
            assert!(
                error.to_string().contains("Invalid session name"),
                "unexpected error for {name:?}: {error}"
            );
            assert!(
                !info.session_file_path_in(session_dir.path()).exists(),
                "no session file should be written for {name:?}"
            );
        }
    }

    #[test]
    fn test_max_length_name_round_trips() {
        let session_dir = tempfile::tempdir().expect("tempdir");
        let name = "y".repeat(64);

        let info = SessionInfo::new(9991, Some("max.txt".into()), name.clone());
        info.write_to_dir(session_dir.path())
            .expect("64-char name must be accepted at write");

        let read = SessionInfo::read_from_dir(&name, session_dir.path())
            .expect("64-char name must be readable after creation");
        assert_eq!(read.session_name, name);
        assert_eq!(read.file.as_deref(), Some("max.txt"));
        assert_eq!(read.capability, info.capability);
        assert!(read.capability.is_configured());

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(session_dir.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(info.session_file_path_in(session_dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn generated_capability_is_random_256_bit_hex_and_debug_redacted() {
        let first = SessionCapability::generate();
        let second = SessionCapability::generate();

        assert_eq!(first.expose_secret().len(), 64);
        assert!(first
            .expose_secret()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert_eq!(format!("{first:?}"), "SessionCapability(<redacted>)");

        let mut session = SessionInfo::new(1, None, "test".into());
        session.capability = first.clone();
        assert!(!format!("{session:?}").contains(first.expose_secret()));
    }

    #[test]
    fn legacy_descriptor_without_capability_is_readable_but_unconfigured() {
        let descriptor = r#"{
            "pid": 1,
            "port": 1234,
            "file": null,
            "started_at": 0,
            "session_name": "legacy",
            "lsp_ready": false,
            "start_time": null
        }"#;

        let session: SessionInfo = serde_json::from_str(descriptor).unwrap();
        assert!(!session.capability.is_configured());
    }

    #[test]
    fn test_read_from_explicit_directory() {
        let session_dir = tempfile::tempdir().expect("tempdir");
        let session_info =
            SessionInfo::new(9997, Some("explicit.txt".into()), "explicit-dir".into());
        session_info.write_to_dir(session_dir.path()).unwrap();

        let read = SessionInfo::read_from_dir("explicit-dir", session_dir.path()).unwrap();

        assert_eq!(read.pid, session_info.pid);
        assert_eq!(read.session_name, "explicit-dir");
        assert_eq!(read.file.as_deref(), Some("explicit.txt"));
    }
}
