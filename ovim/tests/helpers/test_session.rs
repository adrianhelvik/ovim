//! Test infrastructure for spawning and managing ovim headless sessions
//!
//! This module provides `TestSession` - a RAII guard that:
//! - Spawns ovim in headless mode
//! - Waits for the API server to be ready
//! - Provides convenience methods for making HTTP requests
//! - Automatically cleans up the process and session file on drop
//!
//! # Why This Matters
//!
//! Integration tests are different from unit tests in several key ways:
//!
//! 1. **Real Process Isolation**: We spawn an actual ovim process, not just
//!    call functions in-process. This tests the full lifecycle including
//!    session setup, API server initialization, and cleanup.
//!
//! 2. **Network Communication**: Tests use HTTP to communicate with the editor,
//!    exactly like real clients would. This validates serialization, HTTP
//!    headers, error codes, and the full request/response cycle.
//!
//! 3. **Concurrency**: The editor runs in a separate process, handling requests
//!    concurrently via its event loop. This tests real-world async behavior.
//!
//! 4. **Resource Cleanup**: The Drop implementation ensures we don't leak
//!    processes or session files, even if tests panic. This is critical for
//!    CI environments and prevents test pollution.
//!
//! # Test Isolation Pattern
//!
//! Each TestSession gets a unique session name to avoid conflicts:
//! ```rust
//! let session = TestSession::start("test_foo").await?;
//! // session.port is unique (OS-assigned)
//! // session.name is unique to this test
//! // Drop handler cleans up automatically
//! ```

use anyhow::{bail, Context, Result};
use ovim::session::SessionInfo;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

/// RAII guard for a test session
///
/// Automatically kills the process and removes the session file when dropped.
pub struct TestSession {
    pub name: String,
    pub port: u16,
    capability: String,
    process: Child,
    _temp_file: NamedTempFile,
}

impl TestSession {
    /// Start ovim in headless mode for testing
    ///
    /// Creates a temporary file and spawns ovim with a unique session name.
    /// Waits up to 5 seconds for the API server to become ready.
    ///
    /// # Arguments
    /// * `test_name` - Unique name for this test (used as session name)
    ///
    /// # Returns
    /// TestSession with process handle and port number
    pub async fn start(test_name: &str) -> Result<Self> {
        // Create unique temp file for this test
        let mut temp_file = tempfile::Builder::new()
            .prefix(&format!("ovim_test_{}_", test_name))
            .suffix(".txt")
            .tempfile()
            .context("Failed to create temp file")?;
        std::io::Write::write_all(&mut temp_file, b"").context("Failed to seed temp file")?;
        let temp_file_path = temp_file.path().to_string_lossy().to_string();

        // Start ovim headless with unique session name
        let session_name = format!("integration_test_{}", test_name);

        let mut process = Command::new("target/debug/ovim")
            .args(&["--headless", "--session", &session_name, &temp_file_path])
            .spawn()
            .context("Failed to spawn ovim process - did you run 'cargo build'?")?;

        // Wait for session file to appear and server to be ready
        match wait_for_session(&session_name, Duration::from_secs(5)).await {
            Ok(session_info) => Ok(Self {
                name: session_name,
                port: session_info.port,
                capability: session_info.capability.expose_secret().to_string(),
                process,
                _temp_file: temp_file,
            }),
            Err(e) => {
                // Kill the process if startup failed
                let _ = process.kill();
                let _ = process.wait();
                Err(e)
            }
        }
    }

    /// Get full URL for an API endpoint
    ///
    /// # Example
    /// ```rust
    /// let url = session.url("/v1/health");
    /// // Returns: "http://127.0.0.1:54321/v1/health"
    /// ```
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// Helper: GET request returning JSON value
    pub async fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        let resp = reqwest::Client::new()
            .get(self.url(path))
            .bearer_auth(&self.capability)
            .send()
            .await
            .context("GET request failed")?;

        if !resp.status().is_success() {
            bail!("GET {} failed with status {}", path, resp.status());
        }

        resp.json().await.context("Failed to parse JSON")
    }

    /// Helper: POST request with JSON body
    pub async fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let resp = reqwest::Client::new()
            .post(&self.url(path))
            .bearer_auth(&self.capability)
            .json(&body)
            .send()
            .await
            .context("POST request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_else(|_| "".to_string());
            bail!("POST {} failed with status {}: {}", path, status, text);
        }

        resp.json().await.context("Failed to parse JSON")
    }

    /// Helper: PUT request with JSON body
    pub async fn put_json(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = reqwest::Client::new()
            .put(&self.url(path))
            .bearer_auth(&self.capability)
            .json(&body)
            .send()
            .await
            .context("PUT request failed")?;

        if !resp.status().is_success() {
            bail!("PUT {} failed with status {}", path, resp.status());
        }

        resp.json().await.context("Failed to parse JSON")
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        // Kill the process
        let _ = self.process.kill();
        let _ = self.process.wait();

        // Clean up session file
        if let Ok(session_dir) = SessionInfo::session_dir() {
            let session_file = session_dir.join(format!("{}.json", self.name));
            let _ = std::fs::remove_file(session_file);
        }
    }
}

/// Wait for session file to appear and server to become ready
///
/// This polls the session directory for the JSON file, then verifies
/// the HTTP server responds to /v1/health requests.
async fn wait_for_session(name: &str, timeout: Duration) -> Result<SessionInfo> {
    let start = Instant::now();

    loop {
        if let Ok(session_dir) = SessionInfo::session_dir() {
            let session_file = session_dir.join(format!("{}.json", name));

            if session_file.exists() {
                // Try to read and parse JSON manually (SessionInfo::read checks if process is alive)
                if let Ok(json_str) = std::fs::read_to_string(&session_file) {
                    if let Ok(info) = serde_json::from_str::<SessionInfo>(&json_str) {
                        // Verify server is responding
                        let health_url = format!("http://127.0.0.1:{}/v1/health", info.port);

                        if let Ok(resp) = reqwest::Client::new()
                            .get(&health_url)
                            .bearer_auth(info.capability.expose_secret())
                            .send()
                            .await
                        {
                            if resp.status().is_success() {
                                return Ok(info);
                            }
                        }
                    }
                }
            }
        }

        if start.elapsed() > timeout {
            bail!("Session {} did not start within {:?}", name, timeout);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
