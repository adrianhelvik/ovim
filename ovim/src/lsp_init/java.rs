use crate::editor::Editor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

// Global channel for Hyperion LSP status updates (used by all JVM languages)
static HYPERION_STATUS_SENDER: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// Helper to send Hyperion LSP status updates, prefixed with the language name
fn send_hyperion_status(language_label: &str, msg: String) {
    if let Some(tx) = HYPERION_STATUS_SENDER.get() {
        let _ = tx.try_send(format!("{}: {}", language_label, msg));
    }
}

/// Initialize the Hyperion status sender (called from main)
pub fn init_java_status_sender(sender: mpsc::Sender<String>) {
    HYPERION_STATUS_SENDER.set(sender).ok();
}

/// Find the root of a JVM project (Maven or Gradle)
/// Searches parent directories for project markers.
/// For Gradle multi-module projects, prefers settings.gradle (root) over build.gradle (subprojects)
pub fn find_jvm_project_root(file_path: &Path) -> &Path {
    // First pass: look for Gradle root markers (settings.gradle only exists at root)
    let mut current = file_path.parent();
    while let Some(dir) = current {
        if dir.join("settings.gradle").exists() || dir.join("settings.gradle.kts").exists() {
            return dir;
        }
        current = dir.parent();
    }

    // Second pass: look for Maven or single-module Gradle
    current = file_path.parent();
    while let Some(dir) = current {
        if dir.join("pom.xml").exists()
            || dir.join("build.gradle").exists()
            || dir.join("build.gradle.kts").exists()
        {
            return dir;
        }
        current = dir.parent();
    }

    // Fall back to file's parent directory if no project root found
    file_path.parent().unwrap_or_else(|| Path::new("/"))
}

/// Find the hyperion-lsp binary
fn find_hyperion_binary() -> Option<PathBuf> {
    // Check common locations in order of preference
    let candidates: Vec<Option<PathBuf>> = vec![
        // Check PATH using `which` command
        std::process::Command::new("which")
            .arg("hyperion-lsp")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())),
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate.exists())
}

/// Handle Hyperion LSP initialization for any JVM language (spawns background task)
pub async fn handle_hyperion_lsp(editor: &mut Editor, abs_path: PathBuf, language_id: &str) {
    let lsp_manager = editor.lsp_manager();
    let lang_id = language_id.to_string();

    // Spawn Hyperion LSP initialization in background
    tokio::spawn(async move {
        initialize_hyperion_lsp_background(lsp_manager, abs_path, &lang_id).await;
    });
}

/// Background Hyperion LSP initialization
pub async fn initialize_hyperion_lsp_background(
    lsp_manager: Option<Arc<crate::lsp::LspManager>>,
    file_path: PathBuf,
    language_id: &str,
) {
    let language_label = capitalize_first(language_id);
    ovim_core::lsp_debug!(&language_label, "Starting Hyperion LSP for {:?}", file_path);

    let Some(lsp_manager) = lsp_manager else {
        send_hyperion_status(&language_label, "No LSP manager available".to_string());
        return;
    };

    // Find project root
    let project_root = find_jvm_project_root(&file_path);
    ovim_core::lsp_debug!(&language_label, "Project root: {:?}", project_root);

    send_hyperion_status(&language_label, "Finding Hyperion LSP...".to_string());

    // Find the hyperion-lsp binary
    let hyperion_bin = match find_hyperion_binary() {
        Some(bin) => {
            ovim_core::lsp_debug!(&language_label, "Found Hyperion at {:?}", bin);
            bin
        }
        None => {
            send_hyperion_status(
                &language_label,
                "Hyperion LSP not found. Install it or build from source.".to_string(),
            );
            return;
        }
    };

    send_hyperion_status(&language_label, "Starting Hyperion LSP...".to_string());

    // Start the LSP server (no args needed - runs in stdio mode)
    let server_command = hyperion_bin.to_string_lossy().to_string();
    let server_args: Vec<String> = vec![];

    let server_id = match lsp_manager
        .start_server(language_id, &server_command, server_args, project_root)
        .await
    {
        Ok(sid) => {
            send_hyperion_status(&language_label, "Server started".to_string());
            sid
        }
        Err(e) => {
            send_hyperion_status(&language_label, format!("Failed to start: {}", e));
            return;
        }
    };

    // Start notification listener (use returned server_id for multi-root support)
    lsp_manager.start_notification_listener(server_id).await;

    send_hyperion_status(&language_label, "Ready".to_string());
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}
