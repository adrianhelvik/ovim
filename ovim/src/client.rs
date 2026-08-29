//! HTTP client for communicating with ovim sessions
//!
//! Client-side code that outputs to stderr for user feedback.
#![allow(clippy::print_stderr)]

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::{json, Value};

use crate::api::{
    BufferInfo, CursorPosition, EditorSnapshot, HealthInfo, LinesResponse, LspStatusInfo,
    OutlineInfo, SymbolSearchInfo, TraceInfo,
};
use crate::session::SessionInfo;

/// Client for making requests to an ovim session
pub struct OvimClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl OvimClient {
    fn authenticated_client(session: &SessionInfo) -> reqwest::blocking::Client {
        let mut headers = HeaderMap::new();
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", session.capability.expose_secret()))
                .expect("session capabilities contain only HTTP header-safe hex digits");
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        reqwest::blocking::Client::builder()
            .default_headers(headers)
            .build()
            .expect("failed to construct session HTTP client")
    }

    fn ensure_success(response: reqwest::blocking::Response, action: &str) -> Result<Value> {
        let status = response.status();
        let body = response.text().context("Failed to read response")?;
        let value = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
        if !status.is_success() || value.get("error").is_some() {
            let message = value.get("error").and_then(Value::as_str).unwrap_or(&body);
            anyhow::bail!("Failed to {action}: {message}");
        }
        Ok(value)
    }

    /// Create a new client for the given session
    pub fn new(session: &SessionInfo) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", session.port),
            client: Self::authenticated_client(session),
        }
    }

    /// Create a client from explicit session coordinates.
    pub fn from_port(port: u16, capability: &ovim_core::session::SessionCapability) -> Self {
        let session = SessionInfo::new(port, None, "direct-client".into())
            .with_capability(capability.clone());
        Self {
            base_url: format!("http://127.0.0.1:{}", port),
            client: Self::authenticated_client(&session),
        }
    }

    /// Send keys to the session
    pub fn send_keys(&self, keys: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/keys", self.base_url))
            .json(&json!({ "keys": keys }))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let body = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(body);
            anyhow::bail!("Failed to send keys: {}", message);
        }

        Ok(())
    }

    /// Deliver text as a single paste event.
    pub fn paste(&self, text: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/paste", self.base_url))
            .json(&json!({ "text": text }))
            .send()
            .context("Failed to send paste request")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to paste text: {}",
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    /// Resize the session's logical viewport.
    pub fn resize(&self, width: u16, height: u16) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/resize", self.base_url))
            .json(&json!({ "width": width, "height": height }))
            .send()
            .context("Failed to send resize request")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to resize editor: {}",
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    /// Execute an ex command
    pub fn execute_command(&self, command: &str) -> Result<String> {
        let response = self
            .client
            .post(format!("{}/command", self.base_url))
            .json(&json!({ "command": command }))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to execute command: {:?}", error);
        }

        let result: Value = response.json()?;

        // Check for error response first
        if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("{}", error);
        }

        Ok(result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Success")
            .to_string())
    }

    /// Set the editor mode
    pub fn set_mode(&self, mode: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/mode", self.base_url))
            .json(&json!({ "mode": mode }))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to set mode: {:?}", error);
        }

        Ok(())
    }

    /// Get the editor snapshot
    pub fn get_snapshot(&self) -> Result<EditorSnapshot> {
        let response = self
            .client
            .get(format!("{}/snapshot", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get snapshot: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get buffer content
    pub fn get_buffer(&self) -> Result<BufferInfo> {
        let response = self
            .client
            .get(format!("{}/buffer", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get buffer: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Set buffer content
    pub fn set_buffer(&self, content: &str) -> Result<()> {
        let response = self
            .client
            .put(format!("{}/buffer", self.base_url))
            .json(&json!({ "content": content }))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to set buffer: {:?}", error);
        }

        Ok(())
    }

    /// Replace a unique literal match in the live buffer.
    pub fn edit(&self, line: Option<usize>, old: &str, new: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/edit", self.base_url))
            .json(&json!({ "line": line, "old": old, "new": new }))
            .send()
            .context("Failed to send edit request")?;
        Self::ensure_success(response, "edit buffer")?;
        Ok(())
    }

    /// Insert lines in the live buffer.
    pub fn insert(&self, after: Option<usize>, before: Option<usize>, text: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/insert", self.base_url))
            .json(&json!({ "after": after, "before": before, "text": text }))
            .send()
            .context("Failed to send insert request")?;
        Self::ensure_success(response, "insert lines")?;
        Ok(())
    }

    /// Delete an inclusive 1-indexed line range in the live buffer.
    pub fn delete_lines(&self, from: usize, to: usize) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/delete-lines", self.base_url))
            .json(&json!({ "from": from, "to": to }))
            .send()
            .context("Failed to send delete request")?;
        Self::ensure_success(response, "delete lines")?;
        Ok(())
    }

    /// Read an inclusive 1-indexed line range from the live buffer.
    pub fn read_lines(&self, from: usize, to: usize) -> Result<LinesResponse> {
        let response = self
            .client
            .get(format!("{}/lines", self.base_url))
            .query(&[("from", from), ("to", to)])
            .send()
            .context("Failed to send read-lines request")?;
        let value = Self::ensure_success(response, "read lines")?;
        serde_json::from_value(value).context("Invalid read-lines response")
    }

    /// Get cursor position
    pub fn get_cursor(&self) -> Result<CursorPosition> {
        let response = self
            .client
            .get(format!("{}/cursor", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get cursor: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get health status
    pub fn get_health(&self) -> Result<HealthInfo> {
        let response = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get health: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get LSP status
    pub fn get_lsp_status(&self) -> Result<LspStatusInfo> {
        let response = self
            .client
            .get(format!("{}/lsp/status", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get LSP status: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get document outline
    pub fn get_outline(&self) -> Result<OutlineInfo> {
        let response = self
            .client
            .get(format!("{}/v1/outline", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get outline: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Search workspace symbols
    pub fn search_symbols(&self, query: &str) -> Result<SymbolSearchInfo> {
        let response = self
            .client
            .get(format!("{}/v1/symbol", self.base_url))
            .query(&[("q", query)])
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to search symbols: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get call hierarchy trace
    pub fn get_trace(&self) -> Result<TraceInfo> {
        let response = self
            .client
            .get(format!("{}/v1/trace", self.base_url))
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to get trace: {:?}", error);
        }

        Ok(response.json()?)
    }

    /// Get plain-text TUI render
    pub fn get_render_plain(&self, width: u16, height: u16) -> Result<String> {
        let response = self
            .client
            .get(format!("{}/render", self.base_url))
            .query(&[
                ("width", width.to_string()),
                ("height", height.to_string()),
                ("plain", "true".to_string()),
            ])
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get render");
        }

        response.text().context("Failed to read render response")
    }

    /// Send MCP JSON-RPC request
    pub fn send_mcp_request(&self, method: &str, params: Value, id: i64) -> Result<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let response = self
            .client
            .post(format!("{}/mcp", self.base_url))
            .json(&request)
            .send()
            .context("Failed to send MCP request")?;

        if !response.status().is_success() {
            let error: Value = response.json().unwrap_or(json!({"error": "Unknown error"}));
            anyhow::bail!("Failed to send MCP request: {:?}", error);
        }

        let result: Value = response.json()?;

        // Check for JSON-RPC error
        if let Some(error) = result.get("error") {
            anyhow::bail!("MCP error: {}", serde_json::to_string_pretty(error)?);
        }

        Ok(result)
    }

    /// Kill the session (by sending SIGTERM to the process)
    #[cfg(unix)]
    pub fn kill_session(&self, session: &SessionInfo) -> Result<()> {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let pid = Pid::from_raw(session.pid as i32);
        kill(pid, Signal::SIGTERM).context("Failed to kill process")?;

        // Poll for exit instead of unconditionally sleeping
        let mut exited = false;
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Signal 0 checks if process exists without sending a signal
            if kill(pid, None).is_err() {
                exited = true;
                break;
            }
        }

        if !exited {
            // Still running after 500ms, force kill
            let _ = kill(pid, Signal::SIGKILL);
            ovim_core::log_warn!(
                "client",
                "Process {} did not exit gracefully, sent SIGKILL",
                session.pid
            );
        }

        // Clean up session file
        session.delete()?;

        Ok(())
    }

    #[cfg(not(unix))]
    pub fn kill_session(&self, session: &SessionInfo) -> Result<()> {
        anyhow::bail!("Kill not implemented for this platform")
    }
}
