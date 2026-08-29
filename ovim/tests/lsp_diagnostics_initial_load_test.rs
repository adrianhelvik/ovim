//! LSP diagnostics initial-load integration test
//!
//! Requires:
//! - `cargo build --release`
//! - rust-analyzer installed
//!
//! Run with: `cargo test --test lsp_diagnostics_initial_load_test -- --ignored`

use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::{Child, Command, Stdio};

#[derive(Debug, Deserialize)]
struct DiagnosticCounts {
    errors: usize,
}

#[derive(Debug, Deserialize)]
struct DiagnosticsInfo {
    counts: DiagnosticCounts,
}

struct DiagnosticSession {
    port: u16,
    capability: String,
    session_name: String,
    process: Child,
}

impl DiagnosticSession {
    async fn start(file_path: &str) -> Result<Self> {
        let session_name = format!("diag_test_{}", rand::random::<u32>());
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

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let session_path = if cfg!(target_os = "macos") {
            format!(
                "{}/Library/Caches/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                session_name
            )
        } else {
            format!(
                "{}/.cache/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                session_name
            )
        };

        #[derive(Debug, Deserialize)]
        struct SessionInfo {
            port: u16,
            capability: String,
        }

        let session_json =
            std::fs::read_to_string(&session_path).context("Failed to read session file")?;
        let info: SessionInfo =
            serde_json::from_str(&session_json).context("Failed to parse session info")?;

        Ok(Self {
            port: info.port,
            capability: info.capability,
            session_name,
            process,
        })
    }

    async fn cleanup(mut self) -> Result<()> {
        let _ = self.process.kill();
        let session_path = if cfg!(target_os = "macos") {
            format!(
                "{}/Library/Caches/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                self.session_name
            )
        } else {
            format!(
                "{}/.cache/ovim/sessions/{}.json",
                std::env::var("HOME").unwrap(),
                self.session_name
            )
        };
        let _ = std::fs::remove_file(session_path);
        Ok(())
    }
}

#[tokio::test]
#[ignore = "Requires release binary and rust-analyzer"]
async fn diagnostics_appear_without_edits_on_initial_load() -> Result<()> {
    let dir = tempfile::tempdir().context("tempdir")?;
    std::fs::create_dir_all(dir.path().join("src")).context("create src/")?;

    std::fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "ovim_diag_test"
version = "0.1.0"
edition = "2024"
"#,
    )
    .context("write Cargo.toml")?;

    // Intentional type error.
    std::fs::write(
        dir.path().join("src").join("main.rs"),
        r#"fn main() {
    let x: i32 = "oops";
    println!("{x}");
}
"#,
    )
    .context("write main.rs")?;

    let file_path = dir
        .path()
        .join("src")
        .join("main.rs")
        .to_string_lossy()
        .to_string();

    let session = DiagnosticSession::start(&file_path).await?;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/diagnostics", session.port);

    let mut last_counts: Option<DiagnosticCounts> = None;
    for _ in 0..60 {
        let response = client
            .get(&url)
            .bearer_auth(&session.capability)
            .send()
            .await
            .context("GET /diagnostics")?
            .json::<DiagnosticsInfo>()
            .await
            .context("parse /diagnostics")?;

        last_counts = Some(response.counts);
        if last_counts.as_ref().is_some_and(|c| c.errors > 0) {
            session.cleanup().await?;
            return Ok(());
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    session.cleanup().await?;

    anyhow::bail!(
        "Expected diagnostics to appear without edits; last_counts={:?}",
        last_counts
    );
}
