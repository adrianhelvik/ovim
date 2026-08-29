//! LSP Test Framework
//!
//! Provides utilities and macros for writing LSP integration tests.
//!
//! These tests require external infrastructure:
//! - Release binary built (`cargo build --release`)
//! - rust-analyzer installed and working
//! - Tests spawn actual headless ovim processes

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

/// Editor snapshot from API
#[derive(Debug, Clone, Deserialize)]
pub struct EditorSnapshot {
    pub buffer: BufferInfo,
    pub cursor: CursorPosition,
    pub mode: String,
    pub hover_info: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BufferInfo {
    pub content: String,
    pub line_count: usize,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CursorPosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LspServer {
    pub language: String,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LspStatus {
    pub servers: Vec<LspServer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenderInfo {
    pub width: u16,
    pub height: u16,
    pub ansi: String,
}

/// Test session for ovim headless instance
pub struct OvimTestSession {
    pub port: u16,
    pub session_name: String,
    capability: String,
    process: Child,
}

impl OvimTestSession {
    /// Start a new ovim session with a file
    pub async fn start(file_path: &str) -> Result<Self> {
        let session_name = format!("test_{}", rand::random::<u32>());

        // Start ovim headless
        let process = Command::new("./target/release/ovim")
            .arg(file_path)
            .arg("--headless")
            .arg("--session")
            .arg(&session_name)
            .env("OVIM_LSP_DEBUG", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start ovim")?;

        // Wait for session file to be created
        sleep(Duration::from_millis(500)).await;

        // Read port from session file
        let session_path = Self::get_session_path(&session_name);
        let session_json =
            std::fs::read_to_string(&session_path).context("Failed to read session file")?;

        #[derive(Deserialize)]
        struct SessionInfo {
            port: u16,
            capability: String,
        }
        let info: SessionInfo =
            serde_json::from_str(&session_json).context("Failed to parse session info")?;

        let session = Self {
            port: info.port,
            session_name,
            capability: info.capability,
            process,
        };

        // Wait for LSP to be ready
        session.wait_for_lsp_ready().await?;

        Ok(session)
    }

    /// Get session file path
    fn get_session_path(name: &str) -> String {
        if cfg!(target_os = "macos") {
            format!(
                "{}/Library/Caches/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                name
            )
        } else {
            format!(
                "{}/.cache/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                name
            )
        }
    }

    /// Wait for LSP to be ready
    async fn wait_for_lsp_ready(&self) -> Result<()> {
        for _ in 0..60 {
            if let Ok(status) = self.get_lsp_status().await {
                if !status.servers.is_empty()
                    && status.servers.iter().any(|s| s.state.contains("Ready"))
                {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(500)).await;
        }
        anyhow::bail!("LSP did not become ready within 30 seconds")
    }

    /// Send keystrokes to the session
    pub async fn send_keys(&self, keys: &str) -> Result<()> {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/keys", self.port);

        #[derive(Serialize)]
        struct SendKeysRequest {
            keys: String,
        }

        let response = client
            .post(&url)
            .bearer_auth(&self.capability)
            .json(&SendKeysRequest {
                keys: keys.to_string(),
            })
            .send()
            .await
            .context("Failed to send keys")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to send keys: {}", response.status());
        }

        Ok(())
    }

    /// Get editor snapshot
    pub async fn get_snapshot(&self) -> Result<EditorSnapshot> {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/snapshot", self.port);

        let snapshot = client
            .get(&url)
            .bearer_auth(&self.capability)
            .send()
            .await
            .context("Failed to get snapshot")?
            .json::<EditorSnapshot>()
            .await
            .context("Failed to parse snapshot")?;

        Ok(snapshot)
    }

    /// Get rendered ANSI output of the editor
    pub async fn get_render(&self) -> Result<RenderInfo> {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/render", self.port);

        let render = client
            .get(&url)
            .bearer_auth(&self.capability)
            .send()
            .await
            .context("Failed to get render")?
            .json::<RenderInfo>()
            .await
            .context("Failed to parse render")?;

        Ok(render)
    }

    /// Get LSP status
    pub async fn get_lsp_status(&self) -> Result<LspStatus> {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/lsp/status", self.port);

        let status = client
            .get(&url)
            .bearer_auth(&self.capability)
            .send()
            .await
            .context("Failed to get LSP status")?
            .json::<LspStatus>()
            .await
            .context("Failed to parse LSP status")?;

        Ok(status)
    }

    /// Get hover info from current cursor position
    pub async fn get_hover_info(&self) -> Result<Option<String>> {
        let snapshot = self.get_snapshot().await?;
        Ok(snapshot.hover_info)
    }

    /// Get cursor position
    pub async fn get_cursor(&self) -> Result<CursorPosition> {
        let snapshot = self.get_snapshot().await?;
        Ok(snapshot.cursor)
    }

    /// Cleanup session
    pub async fn cleanup(mut self) -> Result<()> {
        // Kill process
        let _ = self.process.kill();

        // Delete session file
        let session_path = Self::get_session_path(&self.session_name);
        let _ = std::fs::remove_file(session_path);

        Ok(())
    }
}

/// Macro to start an ovim test session
#[macro_export]
macro_rules! ovim_session {
    ($file:expr) => {
        $crate::lsp_test_utils::OvimTestSession::start($file).await?
    };
}

/// Macro to send keys
#[macro_export]
macro_rules! send {
    ($session:expr, $keys:expr) => {
        $session.send_keys($keys).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    };
}

/// Macro to assert hover info
#[macro_export]
macro_rules! assert_hover {
    ($session:expr, contains $expected:expr) => {
        let hover = $session.get_hover_info().await?;
        assert!(
            hover
                .as_ref()
                .map(|h| h.contains($expected))
                .unwrap_or(false),
            "Expected hover to contain {:?}, got {:?}",
            $expected,
            hover
        );
    };
    ($session:expr, is_some) => {
        let hover = $session.get_hover_info().await?;
        assert!(hover.is_some(), "Expected hover info to be Some, got None");
    };
    ($session:expr, is_none) => {
        let hover = $session.get_hover_info().await?;
        assert!(
            hover.is_none(),
            "Expected hover info to be None, got {:?}",
            hover
        );
    };
}

/// Macro to assert cursor position
#[macro_export]
macro_rules! assert_cursor {
    ($session:expr, line: $line:expr, col: $col:expr) => {
        let cursor = $session.get_cursor().await?;
        assert_eq!(
            (cursor.line, cursor.column),
            ($line, $col),
            "Expected cursor at ({}, {}), got ({}, {})",
            $line,
            $col,
            cursor.line,
            cursor.column
        );
    };
}

/// Macro to wait briefly
#[macro_export]
macro_rules! wait {
    ($ms:expr) => {
        tokio::time::sleep(tokio::time::Duration::from_millis($ms)).await;
    };
}
