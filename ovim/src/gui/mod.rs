//! Native GUI bridge.
//!
//! The editor remains the source of truth. A dedicated runtime thread owns it
//! and exposes compact, serializable view models to desktop frontends. This is
//! intentionally not an HTML text editor pretending to be Vim: keyboard,
//! paste, picker, tab, and file-tree actions all flow back through Ovim's real
//! input/state machinery.

#[cfg(feature = "gui")]
pub mod app;

use crate::cli::FileArg;
use crate::color::Color;
use crate::editor::{Editor, InputHandler};
use crate::frontend::{
    handle_viewport_resize, process_editor_tick, process_external_file_change,
    process_picker_results, refresh_after_input, FrontendChannels,
};
use crate::git::LineStatus;
use crate::mode::Mode;
use crate::syntax::{HighlightGroup, UiGroup};
use crate::unicode::GraphemeCol;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};
use unicode_segmentation::UnicodeSegmentation;

const TICK_RATE: Duration = Duration::from_millis(50);
const EXTERNAL_FILE_RATE: Duration = Duration::from_millis(500);
const SNAPSHOT_OVERSCAN: usize = 4;
const HORIZONTAL_OVERSCAN: usize = 96;
const MAX_FILE_TREE_ITEMS: usize = 300;
const MAX_PICKER_ITEMS: usize = 24;
const MAX_COMPLETION_ITEMS: usize = 12;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiKeyInput {
    pub key: String,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub meta: bool,
}

fn chat_setup(editor: &Editor) -> Option<GuiChatSetup> {
    if let Some(summary) = editor.codex_auth_dialog_summary() {
        use ovim_core::editor::CodexAuthDialogPhase;
        let (title, detail, actions) = match summary.phase {
            CodexAuthDialogPhase::Offer => (
                "Sign in to Codex",
                summary.detail.unwrap_or_else(|| {
                    "Use your ChatGPT plan for Codex inference inside Ovim.".to_string()
                }),
                vec![("Sign in in browser", "Enter"), ("Not now", "Escape")],
            ),
            CodexAuthDialogPhase::Refreshing => (
                "Refreshing Codex sign-in",
                "Your draft and selection are preserved while Ovim refreshes its credentials."
                    .to_string(),
                vec![("Cancel", "Escape")],
            ),
            CodexAuthDialogPhase::WaitingForBrowser => (
                "Finish sign-in in your browser",
                "Ovim will continue automatically when OpenAI sign-in succeeds.".to_string(),
                vec![("Reopen browser", "o"), ("Cancel", "Escape")],
            ),
            CodexAuthDialogPhase::Error => (
                "Codex sign-in needs attention",
                summary.detail.unwrap_or_else(|| {
                    "Codex sign-in failed; your draft is preserved.".to_string()
                }),
                vec![("Try again", "Enter"), ("Cancel", "Escape")],
            ),
        };
        return Some(GuiChatSetup {
            kind: "codexAuth".to_string(),
            title: title.to_string(),
            detail,
            masked_input: None,
            input_cursor: None,
            error: None,
            actions: actions
                .into_iter()
                .map(|(label, key)| GuiChatSetupAction {
                    label: label.to_string(),
                    key: key.to_string(),
                })
                .collect(),
        });
    }

    let (input, cursor, error, environment_override) = editor.ai_chat_exa_setup_summary()?;
    let cursor = input[..cursor.min(input.len())].chars().count();
    Some(GuiChatSetup {
        kind: "exaKey".to_string(),
        title: "Enable web search".to_string(),
        detail: if environment_override {
            "EXA_API_KEY is set in the environment. Update or unset it there to replace it."
                .to_string()
        } else {
            "Paste an Exa API key to enable live web search, or skip this optional setup."
                .to_string()
        },
        masked_input: Some("•".repeat(input.chars().count())),
        input_cursor: Some(cursor),
        error,
        actions: vec![
            GuiChatSetupAction {
                label: "Save key".to_string(),
                key: "Enter".to_string(),
            },
            GuiChatSetupAction {
                label: "Not now".to_string(),
                key: "Escape".to_string(),
            },
        ],
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatSetup {
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub masked_input: Option<String>,
    pub input_cursor: Option<usize>,
    pub error: Option<String>,
    pub actions: Vec<GuiChatSetupAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatSetupAction {
    pub label: String,
    pub key: String,
}

fn attach_chat_images(editor: &mut Editor, paths: &[std::path::PathBuf]) -> Result<()> {
    if editor.mode() != Mode::AiChat {
        anyhow::bail!("Open AI chat before dropping an image");
    }

    if paths.is_empty() {
        return Ok(());
    }
    for path in paths {
        let supported = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp"
                )
            });
        if !supported {
            anyhow::bail!("Unsupported image format: {}", path.display());
        }
        let path = path
            .to_str()
            .with_context(|| format!("Image path is not valid UTF-8: {}", path.display()))?;
        editor.handle_paste_event(path)?;
    }
    Ok(())
}

impl GuiKeyInput {
    fn into_core(self) -> Result<ovim_core::key::KeyEvent> {
        use ovim_core::key::{KeyCode, KeyEvent, Modifiers};

        let code = match self.key.as_str() {
            "Enter" => KeyCode::Enter,
            "Escape" => KeyCode::Esc,
            "Tab" if self.shift => KeyCode::BackTab,
            "Tab" => KeyCode::Tab,
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "ArrowLeft" => KeyCode::Left,
            "ArrowRight" => KeyCode::Right,
            "ArrowUp" => KeyCode::Up,
            "ArrowDown" => KeyCode::Down,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            key if key.len() > 1 && key.starts_with('F') => key[1..]
                .parse::<u8>()
                .ok()
                .filter(|number| (1..=24).contains(number))
                .map(KeyCode::F)
                .unwrap_or(KeyCode::Null),
            key => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => KeyCode::Char(ch),
                    _ => anyhow::bail!("Unsupported GUI key: {key}"),
                }
            }
        };

        let mut modifiers = Modifiers::NONE;
        if self.shift {
            modifiers |= Modifiers::SHIFT;
        }
        if self.control {
            modifiers |= Modifiers::CONTROL;
        }
        if self.alt {
            modifiers |= Modifiers::ALT;
        }
        if self.meta {
            modifiers |= Modifiers::SUPER;
        }
        Ok(KeyEvent::new(code, modifiers))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiSnapshot {
    pub revision: u64,
    pub mode: String,
    pub dashboard: bool,
    pub file_path: Option<String>,
    pub file_name: String,
    pub project_name: String,
    pub language: String,
    pub encoding: String,
    pub line_ending: String,
    pub modified: bool,
    pub read_only: bool,
    pub selection_text: Option<String>,
    pub cursor: GuiCursor,
    pub horizontal_offset: usize,
    pub wrap: bool,
    pub tab_width: usize,
    pub first_line: usize,
    pub total_lines: usize,
    pub lines: Vec<GuiLine>,
    pub layout: GuiLayoutNode,
    pub panes: Vec<GuiPane>,
    pub tabs: Vec<GuiTab>,
    pub active_tab: usize,
    pub git_branch: Option<String>,
    pub git_changes: GuiGitChanges,
    pub diagnostics: GuiDiagnostics,
    pub lsp_status: String,
    pub status_message: String,
    pub prompt: Option<GuiPrompt>,
    pub picker: Option<GuiPicker>,
    pub completion: Option<GuiCompletion>,
    pub hover: Option<GuiHover>,
    pub file_tree: Option<GuiFileTree>,
    pub ai_chat: Option<GuiAiChat>,
    pub test_panel: Option<GuiTestPanel>,
    pub problems: Option<GuiProblemList>,
    pub lsp_manager: Option<GuiLspManager>,
    pub debug: Option<GuiDebugPanel>,
    pub theme: GuiTheme,
    pub should_quit: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCursor {
    pub line: usize,
    pub column: usize,
    pub display_column: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLine {
    pub number: usize,
    pub continuation: bool,
    pub display_start: usize,
    pub current: bool,
    pub segments: Vec<GuiSegment>,
    pub git: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GuiLayoutNode {
    Pane {
        pane: usize,
    },
    Split {
        direction: String,
        ratio: f32,
        first: Box<GuiLayoutNode>,
        second: Box<GuiLayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPane {
    pub index: usize,
    pub focused: bool,
    pub file_name: String,
    pub modified: bool,
    pub cursor: GuiCursor,
    pub first_line: usize,
    pub scroll_subrow: usize,
    pub horizontal_offset: usize,
    pub total_lines: usize,
    pub lines: Vec<GuiLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiSegment {
    pub text: String,
    pub cells: usize,
    pub token: Option<String>,
    pub cursor: bool,
    pub selected: bool,
    pub search_match: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTab {
    pub index: usize,
    pub title: String,
    pub active: bool,
    pub modified: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiGitChanges {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDiagnostics {
    pub errors: usize,
    pub warnings: usize,
    pub information: usize,
    pub hints: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPrompt {
    pub prefix: String,
    pub text: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPicker {
    pub title: String,
    pub query: String,
    pub file_filter: Option<String>,
    pub selected: usize,
    pub total: usize,
    pub items: Vec<GuiPickerItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPickerItem {
    pub index: usize,
    pub display: String,
    pub location: String,
    pub detail: Option<String>,
    pub matched: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCompletion {
    pub selected: usize,
    pub items: Vec<GuiCompletionItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCompletionItem {
    pub index: usize,
    pub label: String,
    pub detail: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiHover {
    pub content: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiFileTree {
    pub root: String,
    pub selected: usize,
    pub items: Vec<GuiFileTreeItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiFileTreeItem {
    pub index: usize,
    pub name: String,
    pub path: String,
    pub depth: usize,
    pub directory: bool,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiAiChat {
    pub profile: String,
    pub reasoning_effort: String,
    pub activity: String,
    pub waiting: bool,
    pub input: String,
    pub input_cursor: usize,
    pub pending_images: Vec<String>,
    pub setup: Option<GuiChatSetup>,
    pub messages: Vec<GuiChatMessage>,
    pub streaming: Option<String>,
    pub approval: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatMessage {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTestPanel {
    pub scope: String,
    pub command: String,
    pub directory: String,
    pub status: String,
    pub elapsed_ms: u64,
    pub summary: Option<String>,
    pub truncated: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiProblemList {
    pub kind: String,
    pub title: String,
    pub selected: usize,
    pub total: usize,
    pub items: Vec<GuiProblem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiProblem {
    pub index: usize,
    pub severity: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLspManager {
    pub filter: String,
    pub selected: usize,
    pub show_detail: bool,
    pub items: Vec<GuiLspEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLspEntry {
    pub index: usize,
    pub language: String,
    pub section: String,
    pub command: Option<String>,
    pub state: Option<String>,
    pub installing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDebugPanel {
    pub running: bool,
    pub reason: Option<String>,
    pub execution_line: Option<u64>,
    pub stack: Vec<GuiDebugFrame>,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDebugFrame {
    pub name: String,
    pub file: String,
    pub line: u64,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTheme {
    pub name: String,
    pub background: String,
    pub foreground: String,
    pub surface: String,
    pub surface_selected: String,
    pub border: String,
    pub accent: String,
    pub accent_foreground: String,
    pub muted: String,
    pub cursor_line: String,
    pub selection: String,
    pub search: String,
    pub error: String,
    pub warning: String,
    pub info: String,
    pub success: String,
    pub syntax: BTreeMap<String, String>,
}

enum GuiRequest {
    Snapshot {
        columns: u16,
        rows: u16,
        reply: oneshot::Sender<Result<GuiSnapshot, String>>,
    },
    Key {
        input: GuiKeyInput,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Paste {
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AttachImages {
        paths: Vec<std::path::PathBuf>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetCursor {
        pane: usize,
        line: usize,
        display_column: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectTab {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    FocusPane {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectPicker {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectFileTree {
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectProblem {
        kind: String,
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectLsp {
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

/// Send-side handle stored as Tauri application state.
#[derive(Clone)]
pub struct GuiBridge {
    requests: mpsc::UnboundedSender<GuiRequest>,
    updates: watch::Sender<Option<GuiSnapshot>>,
}

impl GuiBridge {
    pub fn spawn(file: Option<FileArg>, resume: bool) -> Result<Self> {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let editor_updates = update_tx.clone();

        std::thread::Builder::new()
            .name("ovim-gui-editor".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = ready_tx.send(Err("Failed to create GUI runtime".to_string()));
                    return;
                };
                runtime.block_on(run_editor(
                    file,
                    resume,
                    request_rx,
                    editor_updates,
                    ready_tx,
                ));
            })
            .context("Failed to start the GUI editor thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self {
                requests: request_tx,
                updates: update_tx,
            }),
            Ok(Err(error)) => anyhow::bail!(error),
            Err(error) => anyhow::bail!("GUI editor initialization timed out: {error}"),
        }
    }

    /// Subscribe to coalesced editor-state changes.
    ///
    /// Tauri turns this watch stream into an IPC channel. Slow webviews only
    /// retain the newest snapshot instead of building an unbounded queue.
    pub fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>> {
        self.updates.subscribe()
    }

    pub async fn snapshot(&self, columns: u16, rows: u16) -> Result<GuiSnapshot, String> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(GuiRequest::Snapshot {
                columns,
                rows,
                reply,
            })
            .map_err(|_| "The Ovim editor thread has stopped".to_string())?;
        response
            .await
            .map_err(|_| "The Ovim editor thread closed the response".to_string())?
    }

    pub async fn key(&self, input: GuiKeyInput) -> Result<(), String> {
        self.request(|reply| GuiRequest::Key { input, reply }).await
    }

    pub async fn paste(&self, text: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::Paste { text, reply })
            .await
    }

    pub async fn attach_images(&self, paths: Vec<std::path::PathBuf>) -> Result<(), String> {
        self.request(|reply| GuiRequest::AttachImages { paths, reply })
            .await
    }

    pub async fn set_cursor(
        &self,
        pane: usize,
        line: usize,
        display_column: usize,
    ) -> Result<(), String> {
        self.request(|reply| GuiRequest::SetCursor {
            pane,
            line,
            display_column,
            reply,
        })
        .await
    }

    pub async fn select_tab(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectTab { index, reply })
            .await
    }

    pub async fn focus_pane(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::FocusPane { index, reply })
            .await
    }

    pub async fn select_picker(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectPicker { index, reply })
            .await
    }

    pub async fn select_file_tree(&self, index: usize, activate: bool) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectFileTree {
            index,
            activate,
            reply,
        })
        .await
    }

    pub async fn select_problem(
        &self,
        kind: String,
        index: usize,
        activate: bool,
    ) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectProblem {
            kind,
            index,
            activate,
            reply,
        })
        .await
    }

    pub async fn select_lsp(&self, index: usize, activate: bool) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectLsp {
            index,
            activate,
            reply,
        })
        .await
    }

    pub fn shutdown(&self) {
        let _ = self.requests.send(GuiRequest::Shutdown);
    }

    async fn request(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<(), String>>) -> GuiRequest,
    ) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.requests
            .send(request(reply_tx))
            .map_err(|_| "The Ovim editor thread has stopped".to_string())?;
        reply_rx
            .await
            .map_err(|_| "The Ovim editor thread closed the response".to_string())?
    }
}

async fn run_editor(
    file: Option<FileArg>,
    resume: bool,
    mut requests: mpsc::UnboundedReceiver<GuiRequest>,
    updates: watch::Sender<Option<GuiSnapshot>>,
    ready: std_mpsc::SyncSender<Result<(), String>>,
) {
    let mut editor = Editor::new();
    if let Err(error) = editor.enable_lua() {
        editor.set_status_message(format!("Lua configuration: {error}"));
    }
    editor.language_catalog().install_as_process_catalog();

    if let Some(file) = file {
        let path = Path::new(&file.path);
        let result = if path.is_dir() {
            editor.open_directory(path)
        } else {
            editor.load_file_async(path).await
        };
        if let Err(error) = result {
            editor.set_file_path(file.path.clone());
            editor.set_status_message(format!("Could not open {}: {error}", file.path));
        } else if !path.is_dir() {
            if let Some(line) = file.line {
                editor.buffer_mut().cursor_mut().set_position(
                    line.saturating_sub(1),
                    GraphemeCol(file.col.unwrap_or(1).saturating_sub(1)),
                );
                editor.buffer_mut().validate_cursor_position();
            }
            editor.set_mode(Mode::Normal);
        }
    }

    editor.set_ai_conversation_resume_enabled(resume);
    editor.enable_lsp();
    let (java_status_tx, java_status_rx) = mpsc::channel(64);
    crate::lsp_init::init_java_status_sender(java_status_tx);
    let mut channels = FrontendChannels::new(java_status_rx);
    let mut tick = tokio::time::interval(TICK_RATE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_external_check = Instant::now();
    let mut revision = 1u64;
    let mut dimensions = (120u16, 40u16);

    handle_viewport_resize(&mut editor, dimensions.0, dimensions.1);
    let mut last_snapshot = snapshot(&editor, revision);
    let mut last_render_version = editor.render_input_version();
    updates.send_replace(Some(last_snapshot.clone()));
    let _ = ready.send(Ok(()));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                process_editor_tick(&mut editor, &mut channels).await;
                process_picker_results(&mut editor, &mut channels);
                editor.process_pending_rehighlight().await;
                if last_external_check.elapsed() >= EXTERNAL_FILE_RATE {
                    process_external_file_change(&mut editor);
                    last_external_check = Instant::now();
                }
                publish_if_changed(
                    &editor,
                    &mut revision,
                    &mut last_snapshot,
                    &mut last_render_version,
                    &updates,
                );
            }
            request = requests.recv() => {
                let Some(request) = request else { break; };
                if matches!(request, GuiRequest::Shutdown) {
                    break;
                }
                handle_request(request, &mut editor, &mut dimensions, &mut revision).await;
                publish_if_changed(
                    &editor,
                    &mut revision,
                    &mut last_snapshot,
                    &mut last_render_version,
                    &updates,
                );
            }
        }
    }

    editor.close_current_file_lsp().await;
}

fn publish_if_changed(
    editor: &Editor,
    revision: &mut u64,
    previous: &mut GuiSnapshot,
    previous_render_version: &mut u64,
    updates: &watch::Sender<Option<GuiSnapshot>>,
) {
    let render_version = editor.render_input_version();
    if render_version == *previous_render_version {
        return;
    }
    *previous_render_version = render_version;
    // Compare at the current revision so time-based runtime ticks that did not
    // affect the visible projection produce no webview traffic or DOM work.
    let mut next = snapshot(editor, *revision);
    if next == *previous {
        return;
    }
    *revision = revision.wrapping_add(1);
    next.revision = *revision;
    *previous = next.clone();
    updates.send_replace(Some(next));
}

async fn handle_request(
    request: GuiRequest,
    editor: &mut Editor,
    dimensions: &mut (u16, u16),
    revision: &mut u64,
) {
    let reply = match request {
        GuiRequest::Snapshot {
            columns,
            rows,
            reply,
        } => {
            let next = (columns.max(20), rows.max(5));
            if *dimensions != next {
                *dimensions = next;
                handle_viewport_resize(editor, next.0, next.1);
            }
            let _ = reply.send(Ok(snapshot(editor, *revision)));
            return;
        }
        GuiRequest::Key { input, reply } => {
            let result = input
                .into_core()
                .and_then(|event| InputHandler::handle_key_event_no_dirty(editor, event));
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::AttachImages { paths, reply } => {
            let result = attach_chat_images(editor, &paths);
            if let Err(error) = &result {
                editor.set_status_message(format!("Image attachment: {error}"));
            }
            refresh_after_input(editor);
            (reply, result)
        }
        GuiRequest::Paste { text, reply } => {
            let result = editor.handle_paste_event(&text);
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::SetCursor {
            pane,
            line,
            display_column,
            reply,
        } => {
            if !editor.focus_window(pane) {
                let _ = reply.send(Err(format!("Unknown editor pane: {pane}")));
                return;
            }
            editor.set_mode(Mode::Normal);
            let text = editor
                .buffer()
                .line_text(line)
                .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
                .unwrap_or_default();
            let char_column = crate::display::display_col_to_char_col(
                &text,
                display_column,
                editor.indent_options().tab_width,
            );
            let grapheme_column =
                crate::unicode::char_to_grapheme_col(&text, crate::unicode::CharCol(char_column));
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(line, grapheme_column);
            editor.buffer_mut().validate_cursor_position();
            editor.update_scroll_offset();
            refresh_after_input(editor);
            (reply, Ok(()))
        }
        GuiRequest::SelectTab { index, reply } => {
            editor.goto_tab(index);
            refresh_after_input(editor);
            (reply, Ok(()))
        }
        GuiRequest::FocusPane { index, reply } => {
            let result = editor
                .focus_window(index)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown editor pane: {index}"));
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::SelectPicker { index, reply } => {
            if let Some(picker) = editor.picker_mut() {
                picker.set_selected_index(index);
            }
            let result = InputHandler::handle_key_event_no_dirty(
                editor,
                ovim_core::key::KeyEvent::new(
                    ovim_core::key::KeyCode::Enter,
                    ovim_core::key::Modifiers::NONE,
                ),
            );
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::SelectFileTree {
            index,
            activate,
            reply,
        } => {
            editor.file_tree_mut().set_selected_index(index);
            editor.set_mode(Mode::FileTree);
            if activate {
                editor.open_file_from_tree();
            }
            refresh_after_input(editor);
            (reply, Ok(()))
        }
        GuiRequest::SelectProblem {
            kind,
            index,
            activate,
            reply,
        } => {
            let result = match kind.as_str() {
                "quickfix" => {
                    editor.quickfix_list_mut().set_selected(index);
                    if activate {
                        editor.jump_to_quickfix_entry();
                    }
                    Ok(())
                }
                "location" => {
                    editor.location_list_mut().set_selected(index);
                    if activate {
                        editor.jump_to_location_entry();
                    }
                    Ok(())
                }
                _ => Err(anyhow::anyhow!("Unknown problem list: {kind}")),
            };
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::SelectLsp {
            index,
            activate,
            reply,
        } => {
            let result = if let Some(panel) = editor.lsp_manager_panel_mut() {
                if index < panel.entries.len() {
                    panel.selected_index = index;
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("Unknown LSP entry: {index}"))
                }
            } else {
                Err(anyhow::anyhow!("The LSP manager is not open"))
            };
            let result = if activate && result.is_ok() {
                InputHandler::handle_key_event_no_dirty(
                    editor,
                    ovim_core::key::KeyEvent::new(
                        ovim_core::key::KeyCode::Enter,
                        ovim_core::key::Modifiers::NONE,
                    ),
                )
            } else {
                result
            };
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::Shutdown => unreachable!(),
    };

    let (sender, result) = reply;
    let response = result.map_err(|error| error.to_string());
    let _ = sender.send(response);
}

/// Project the editor into a bounded DOM-friendly view model.
pub fn snapshot(editor: &Editor, revision: u64) -> GuiSnapshot {
    let buffer = editor.buffer();
    let cursor = buffer.cursor();
    let total_lines = buffer.line_count();
    let first_line = editor.scroll_offset().min(total_lines.saturating_sub(1));
    let visible = editor.viewport_height().max(1) + SNAPSHOT_OVERSCAN;
    let tab_width = editor.indent_options().tab_width.max(1);
    let wrap_width = editor
        .options
        .wrap
        .then(|| editor.wrap_map().map(|map| map.wrap_width()))
        .flatten();
    let active_window_width = editor
        .window_manager()
        .and_then(|manager| manager.get_window(manager.focused_window_index()))
        .map(|window| window.width())
        .unwrap_or(120);
    let text_view_width = crate::frontend::compute_text_width(editor, active_window_width).max(1);
    let lines = project_lines(
        editor,
        buffer,
        cursor.line(),
        cursor.col().0,
        first_line,
        editor.scroll_subrow(),
        visible,
        true,
        tab_width,
        wrap_width,
        editor.horizontal_offset(),
        text_view_width,
    );

    let file_path = buffer.file_path().map(str::to_string);
    let file_name = file_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .or_else(|| buffer.display_name())
        .unwrap_or("Untitled")
        .to_string();
    let project_name = editor
        .file_tree()
        .root_path()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .or_else(|| {
            file_path
                .as_deref()
                .and_then(|path| Path::new(path).parent())
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
        })
        .unwrap_or("ovim")
        .to_string();
    let language = file_path
        .as_deref()
        .and_then(|path| editor.language_id_for_path(path))
        .unwrap_or_else(|| "plain text".to_string());
    let (errors, warnings, information, hints) = editor.cached_diagnostic_count();
    let (added, modified, removed) = buffer.git_status().change_counts();
    let cursor_line_text = buffer
        .line_text(cursor.line())
        .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
        .unwrap_or_default();
    let cursor_char_column = crate::unicode::grapheme_to_char_col(&cursor_line_text, cursor.col());
    let cursor_display_column =
        crate::display::char_col_to_display_col(&cursor_line_text, cursor_char_column.0, tab_width);
    let (layout, panes) = project_panes(
        editor,
        &file_name,
        &lines,
        first_line,
        cursor_display_column,
        tab_width,
    );

    GuiSnapshot {
        revision,
        mode: editor.mode().display_name().to_string(),
        dashboard: editor.should_show_dashboard(),
        file_path,
        file_name,
        project_name,
        language,
        encoding: buffer.encoding().display_name().to_string(),
        line_ending: buffer.line_ending().display_name().to_string(),
        modified: buffer.is_modified(),
        read_only: buffer.is_read_only(),
        selection_text: editor.visual_selection_text(),
        cursor: GuiCursor {
            line: cursor.line(),
            column: cursor.col().0,
            display_column: cursor_display_column,
        },
        horizontal_offset: editor.horizontal_offset(),
        wrap: editor.options.wrap,
        tab_width,
        first_line,
        total_lines,
        lines,
        layout,
        panes,
        tabs: (0..editor.tab_count())
            .map(|index| {
                let active = index == editor.current_tab_index();
                let modified = if active {
                    buffer.is_modified()
                } else {
                    editor
                        .tab_page_manager()
                        .tab(index)
                        .and_then(|tab| tab.buffer_id())
                        .and_then(|id| editor.get_buffer_by_id(id))
                        .is_some_and(|buffer| buffer.is_modified())
                };
                GuiTab {
                    index,
                    title: editor.get_tab_title(index),
                    active,
                    modified,
                }
            })
            .collect(),
        active_tab: editor.current_tab_index(),
        git_branch: editor.git_branch().map(str::to_string),
        git_changes: GuiGitChanges {
            added,
            modified,
            removed,
        },
        diagnostics: GuiDiagnostics {
            errors,
            warnings,
            information,
            hints,
        },
        lsp_status: editor.lsp_status().to_string(),
        status_message: editor.status_message().to_string(),
        prompt: prompt(editor),
        picker: picker(editor),
        completion: completion(editor),
        hover: editor.hover_info().map(|content| {
            let position = editor.hover_position();
            GuiHover {
                content: content.to_string(),
                line: position.map(|position| position.0),
                column: position.map(|position| position.1),
            }
        }),
        file_tree: file_tree(editor),
        ai_chat: ai_chat(editor),
        test_panel: test_panel(editor),
        problems: problem_list(editor),
        lsp_manager: lsp_manager(editor),
        debug: debug_panel(editor),
        theme: theme(editor),
        should_quit: editor.should_quit(),
    }
}

fn project_lines(
    editor: &Editor,
    buffer: &crate::buffer::Buffer,
    cursor_line: usize,
    cursor_column: usize,
    first_line: usize,
    scroll_subrow: usize,
    visible: usize,
    focused: bool,
    tab_width: usize,
    wrap_width: Option<usize>,
    horizontal_offset: usize,
    text_view_width: usize,
) -> Vec<GuiLine> {
    let selection = focused.then(|| editor.visual_selection()).flatten();
    let highlights = focused.then(|| editor.current_search()).flatten();
    let mut projected = Vec::with_capacity(visible);

    'lines: for line_index in first_line..buffer.line_count() {
        let text = buffer
            .line_text(line_index)
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .unwrap_or_default();
        let syntax = buffer.highlights_for_line(line_index);
        let search_matches = highlights
            .map(|search| search.find_all_in_line(&text))
            .unwrap_or_default();
        let diagnostic = if focused {
            editor.diagnostics_for_line(line_index)
        } else {
            Vec::new()
        }
        .iter()
        .map(|diagnostic| match diagnostic.severity {
            Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
            Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => "information",
            Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
            _ => "error",
        })
        .next()
        .map(str::to_string);

        let display_window = wrap_width.is_none().then(|| {
            (
                horizontal_offset.saturating_sub(HORIZONTAL_OVERSCAN),
                horizontal_offset
                    .saturating_add(text_view_width)
                    .saturating_add(HORIZONTAL_OVERSCAN),
            )
        });
        let (segment_start, segments) = segments_for_line(
            &text,
            line_index,
            cursor_line,
            cursor_column,
            editor.mode(),
            selection,
            &syntax,
            &search_matches,
            tab_width,
            display_window,
        );
        let visual_rows = if wrap_width.is_some() {
            split_visual_rows(segments, wrap_width)
        } else {
            vec![(segment_start, coalesce_segments(segments))]
        };
        let skip = if line_index == first_line {
            scroll_subrow.min(visual_rows.len().saturating_sub(1))
        } else {
            0
        };
        let git = buffer
            .git_status()
            .get_line_status(line_index)
            .map(|status| match status {
                LineStatus::Added => "added",
                LineStatus::Modified => "modified",
                LineStatus::Removed => "removed",
            })
            .map(str::to_string);

        for (visual_index, (display_start, segments)) in
            visual_rows.into_iter().enumerate().skip(skip)
        {
            let continuation = visual_index > 0;
            projected.push(GuiLine {
                number: line_index + 1,
                continuation,
                display_start,
                current: cursor_line == line_index,
                segments,
                git: (!continuation).then(|| git.clone()).flatten(),
                diagnostic: (!continuation).then(|| diagnostic.clone()).flatten(),
            });
            if projected.len() >= visible {
                break 'lines;
            }
        }
    }
    projected
}

fn project_panes(
    editor: &Editor,
    active_file_name: &str,
    active_lines: &[GuiLine],
    active_first_line: usize,
    active_display_column: usize,
    tab_width: usize,
) -> (GuiLayoutNode, Vec<GuiPane>) {
    let Some(manager) = editor.window_manager() else {
        let cursor = editor.buffer().cursor();
        return (
            GuiLayoutNode::Pane { pane: 0 },
            vec![GuiPane {
                index: 0,
                focused: true,
                file_name: active_file_name.to_string(),
                modified: editor.buffer().is_modified(),
                cursor: GuiCursor {
                    line: cursor.line(),
                    column: cursor.col().0,
                    display_column: active_display_column,
                },
                first_line: active_first_line,
                scroll_subrow: editor.scroll_subrow(),
                horizontal_offset: editor.horizontal_offset(),
                total_lines: editor.buffer().line_count(),
                lines: active_lines.to_vec(),
            }],
        );
    };

    let mut pane_index = 0;
    let layout = project_layout(manager.root(), &mut pane_index);
    let focused_index = manager.focused_window_index();
    let panes = (0..manager.window_count())
        .filter_map(|index| {
            let window = manager.get_window(index)?;
            let focused = index == focused_index;
            let buffer = editor
                .get_buffer(window.buffer_id())
                .unwrap_or_else(|| editor.buffer());
            let cursor = if focused {
                editor.buffer().cursor()
            } else {
                window.cursor()
            };
            let first_line = if focused {
                active_first_line
            } else {
                window
                    .scroll_offset()
                    .min(buffer.line_count().saturating_sub(1))
            };
            let lines = if focused {
                active_lines.to_vec()
            } else {
                project_lines(
                    editor,
                    buffer,
                    cursor.line(),
                    cursor.col().0,
                    first_line,
                    window.scroll_subrow(),
                    window.height() as usize + SNAPSHOT_OVERSCAN,
                    false,
                    tab_width,
                    editor.options.wrap.then(|| {
                        crate::frontend::compute_text_width(editor, window.width()).max(1)
                    }),
                    window.horizontal_offset(),
                    crate::frontend::compute_text_width(editor, window.width()).max(1),
                )
            };
            let line_text = buffer
                .line_text(cursor.line())
                .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
                .unwrap_or_default();
            let char_column = crate::unicode::grapheme_to_char_col(&line_text, cursor.col());
            let display_column = if focused {
                active_display_column
            } else {
                crate::display::char_col_to_display_col(&line_text, char_column.0, tab_width)
            };
            let file_name = buffer
                .file_path()
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .or_else(|| buffer.display_name())
                .unwrap_or("Untitled")
                .to_string();

            Some(GuiPane {
                index,
                focused,
                file_name,
                modified: buffer.is_modified(),
                cursor: GuiCursor {
                    line: cursor.line(),
                    column: cursor.col().0,
                    display_column,
                },
                first_line,
                scroll_subrow: window.scroll_subrow(),
                horizontal_offset: window.horizontal_offset(),
                total_lines: buffer.line_count(),
                lines,
            })
        })
        .collect();
    (layout, panes)
}

fn project_layout(node: &crate::editor::WindowNode, pane: &mut usize) -> GuiLayoutNode {
    match node {
        crate::editor::WindowNode::Leaf(_) => {
            let index = *pane;
            *pane += 1;
            GuiLayoutNode::Pane { pane: index }
        }
        crate::editor::WindowNode::Split {
            direction,
            ratio,
            first,
            second,
        } => GuiLayoutNode::Split {
            direction: match direction {
                crate::editor::SplitDirection::Horizontal => "horizontal",
                crate::editor::SplitDirection::Vertical => "vertical",
            }
            .to_string(),
            ratio: *ratio,
            first: Box::new(project_layout(first, pane)),
            second: Box::new(project_layout(second, pane)),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn segments_for_line(
    text: &str,
    line: usize,
    cursor_line: usize,
    cursor_column: usize,
    mode: Mode,
    selection: Option<((usize, usize), (usize, usize))>,
    highlights: &[(std::ops::Range<usize>, HighlightGroup)],
    search_matches: &[(usize, usize)],
    tab_width: usize,
    display_window: Option<(usize, usize)>,
) -> (usize, Vec<GuiSegment>) {
    let mut segments: Vec<GuiSegment> = Vec::new();
    let mut display_column = 0usize;
    let mut char_column = 0usize;
    let mut grapheme_count = 0usize;
    let mut first_display_column = None;

    for (column, (byte, grapheme)) in text.grapheme_indices(true).enumerate() {
        let token = highlights
            .iter()
            .rev()
            .find(|(range, _)| range.start <= byte && byte < range.end)
            .map(|(_, group)| syntax_name(*group));
        let cursor = line == cursor_line && column == cursor_column;
        let selected = selection.is_some_and(|range| selected_at(line, column, mode, range));
        let search_match = search_matches
            .iter()
            .any(|(start, end)| *start <= char_column && char_column < *end);
        let control = if grapheme.chars().count() == 1 {
            grapheme
                .chars()
                .next()
                .and_then(crate::display::control_char_caret)
        } else {
            None
        };
        let cells = if grapheme == "\t" {
            tab_width - (display_column % tab_width)
        } else if control.is_some() {
            2
        } else {
            crate::display::grapheme_display_width(grapheme)
        };

        let segment_end = display_column.saturating_add(cells);
        let visible =
            display_window.is_none_or(|(start, end)| segment_end > start && display_column < end);
        if visible {
            let rendered = if grapheme == "\t" {
                " ".repeat(cells)
            } else if let Some(caret) = control {
                caret.into_iter().collect()
            } else {
                grapheme.to_string()
            };
            first_display_column.get_or_insert(display_column);
            segments.push(GuiSegment {
                text: rendered,
                cells,
                token: token.map(str::to_string),
                cursor,
                selected,
                search_match,
            });
        }
        display_column += cells;
        char_column += grapheme.chars().count();
        grapheme_count = column + 1;
    }

    if line == cursor_line && cursor_column >= grapheme_count {
        let visible = display_window.is_none_or(|(start, end)| {
            display_column.saturating_add(1) > start && display_column < end
        });
        if visible {
            first_display_column.get_or_insert(display_column);
        }
        if visible {
            segments.push(GuiSegment {
                text: " ".to_string(),
                cells: 1,
                token: None,
                cursor: true,
                selected: false,
                search_match: false,
            });
        }
    }
    if segments.is_empty() {
        let start = display_window.map_or(0, |(start, _)| start);
        first_display_column = Some(start);
        segments.push(GuiSegment {
            text: " ".to_string(),
            cells: 1,
            token: None,
            cursor: line == cursor_line,
            selected: false,
            search_match: false,
        });
    }
    (first_display_column.unwrap_or(0), segments)
}

fn split_visual_rows(
    segments: Vec<GuiSegment>,
    wrap_width: Option<usize>,
) -> Vec<(usize, Vec<GuiSegment>)> {
    let Some(width) = wrap_width.map(|width| width.max(1)) else {
        return vec![(0, coalesce_segments(segments))];
    };

    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut row_cells = 0usize;
    let mut flat_column = 0usize;
    let mut row_start = 0usize;

    for segment in segments {
        // Tabs are expanded to spaces by `segments_for_line`. Split those
        // cells independently because a tab may cross a wrap boundary.
        let pieces = if segment.cells > 1
            && segment.text.chars().count() == segment.cells
            && segment.text.chars().all(|ch| ch == ' ')
        {
            (0..segment.cells)
                .map(|_| GuiSegment {
                    text: " ".to_string(),
                    cells: 1,
                    token: segment.token.clone(),
                    cursor: segment.cursor,
                    selected: segment.selected,
                    search_match: segment.search_match,
                })
                .collect::<Vec<_>>()
        } else {
            vec![segment]
        };

        for piece in pieces {
            if !row.is_empty() && row_cells + piece.cells > width {
                rows.push((row_start, coalesce_segments(std::mem::take(&mut row))));
                row_start = flat_column;
                row_cells = 0;
            }
            row_cells += piece.cells;
            flat_column += piece.cells;
            row.push(piece);
        }
    }

    if !row.is_empty() {
        rows.push((row_start, coalesce_segments(row)));
    }
    if rows.is_empty() {
        rows.push((0, Vec::new()));
    }
    rows
}

fn coalesce_segments(segments: Vec<GuiSegment>) -> Vec<GuiSegment> {
    let mut merged: Vec<GuiSegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if let Some(previous) = merged.last_mut()
            && previous.token == segment.token
            && previous.selected == segment.selected
            && previous.search_match == segment.search_match
            && !previous.cursor
            && !segment.cursor
        {
            previous.text.push_str(&segment.text);
            previous.cells += segment.cells;
        } else {
            merged.push(segment);
        }
    }
    merged
}

fn selected_at(
    line: usize,
    column: usize,
    mode: Mode,
    ((start_line, start_col), (end_line, end_col)): ((usize, usize), (usize, usize)),
) -> bool {
    if line < start_line || line > end_line {
        return false;
    }
    match mode {
        Mode::VisualLine => true,
        Mode::VisualBlock => column >= start_col && column <= end_col,
        _ if start_line == end_line => column >= start_col && column <= end_col,
        _ if line == start_line => column >= start_col,
        _ if line == end_line => column <= end_col,
        _ => true,
    }
}

fn prompt(editor: &Editor) -> Option<GuiPrompt> {
    match editor.mode() {
        Mode::Command => Some(GuiPrompt {
            prefix: ":".to_string(),
            text: editor.command_line().to_string(),
            cursor: editor.command_cursor(),
        }),
        Mode::Search => Some(GuiPrompt {
            prefix: if editor.search_forward() { "/" } else { "?" }.to_string(),
            text: editor.search_buffer().to_string(),
            cursor: editor.search_cursor(),
        }),
        Mode::RenameInput => Some(GuiPrompt {
            prefix: "rename".to_string(),
            text: editor.rename_buffer().to_string(),
            cursor: editor.rename_cursor(),
        }),
        _ => None,
    }
}

fn picker(editor: &Editor) -> Option<GuiPicker> {
    let picker = editor.picker()?;
    let title = match picker.mode() {
        crate::editor::PickerMode::FindFiles => "Find files",
        crate::editor::PickerMode::LiveGrep => "Search project",
        crate::editor::PickerMode::Custom => "Choose an action",
        crate::editor::PickerMode::Completion => "Completions",
        crate::editor::PickerMode::LspLocations => "Locations",
    };
    let total = picker.filtered_result_count();
    let selected = picker.selected_index();
    let start = centered_window_start(selected, total, MAX_PICKER_ITEMS);
    let items = (start..total.min(start + MAX_PICKER_ITEMS))
        .filter_map(|index| picker.filtered_result(index).map(|item| (index, item)))
        .map(|(index, item)| GuiPickerItem {
            index,
            display: item.display.clone(),
            location: item.location.clone(),
            detail: item.content.clone(),
            matched: item.match_positions.clone(),
        })
        .collect();
    Some(GuiPicker {
        title: title.to_string(),
        query: picker.query().to_string(),
        file_filter: picker
            .has_file_filter()
            .then(|| picker.file_filter().to_string()),
        selected,
        total,
        items,
    })
}

fn completion(editor: &Editor) -> Option<GuiCompletion> {
    let menu = editor.completion_menu();
    let selected = menu.selected_index();
    let start = centered_window_start(selected, menu.items().len(), MAX_COMPLETION_ITEMS);
    menu.is_visible().then(|| GuiCompletion {
        selected,
        items: menu
            .items()
            .iter()
            .enumerate()
            .skip(start)
            .take(MAX_COMPLETION_ITEMS)
            .map(|(index, item)| GuiCompletionItem {
                index,
                label: item.label.clone(),
                detail: item.detail.clone(),
                kind: item.kind.map(|kind| format!("{kind:?}")),
            })
            .collect(),
    })
}

fn file_tree(editor: &Editor) -> Option<GuiFileTree> {
    let tree = editor.file_tree();
    let selected = tree.selected_index();
    let start = centered_window_start(selected, tree.flattened().len(), MAX_FILE_TREE_ITEMS);
    tree.is_visible().then(|| GuiFileTree {
        root: tree.root_name().to_string(),
        selected,
        items: tree
            .flattened()
            .iter()
            .enumerate()
            .skip(start)
            .take(MAX_FILE_TREE_ITEMS)
            .map(|(index, node)| GuiFileTreeItem {
                index,
                name: node.name().to_string(),
                path: node.path().to_string_lossy().to_string(),
                depth: node.depth(),
                directory: node.is_dir(),
                expanded: node.is_expanded(),
            })
            .collect(),
    })
}

fn ai_chat(editor: &Editor) -> Option<GuiAiChat> {
    (editor.mode() == Mode::AiChat).then(|| {
        let messages = editor.ai_chat_messages();
        let start = messages.len().saturating_sub(40);
        GuiAiChat {
            profile: editor.ai_chat_effective_profile(),
            reasoning_effort: editor.ai_chat_reasoning_effort(),
            activity: editor.ai_chat_activity().as_str().to_string(),
            waiting: editor.ai_chat_waiting(),
            input: truncate_panel_text(editor.ai_chat_input(), 12_000),
            input_cursor: editor.ai_chat_input_cursor(),
            pending_images: editor
                .ai_chat_pending_images()
                .iter()
                .map(|image| {
                    image
                        .path
                        .file_name()
                        .unwrap_or(image.path.as_os_str())
                        .to_string_lossy()
                        .to_string()
                })
                .collect(),
            setup: chat_setup(editor),
            messages: messages[start..]
                .iter()
                .map(|message| GuiChatMessage {
                    role: match message.role {
                        ovim_core::ai::chat_types::ChatRole::User => "user",
                        ovim_core::ai::chat_types::ChatRole::Assistant => "assistant",
                        ovim_core::ai::chat_types::ChatRole::Thinking => "thinking",
                        ovim_core::ai::chat_types::ChatRole::Error => "error",
                        ovim_core::ai::chat_types::ChatRole::Tool => "tool",
                    }
                    .to_string(),
                    content: truncate_panel_text(&message.content, 8_000),
                    model: message.model.clone(),
                    tools: message
                        .tool_calls
                        .iter()
                        .take(24)
                        .map(|tool| tool.name.clone())
                        .collect(),
                })
                .collect(),
            streaming: editor
                .ai_chat_streaming_content()
                .filter(|content| !content.is_empty())
                .map(|content| truncate_panel_text(content, 12_000)),
            approval: editor
                .ai_chat_pending_tool_approval_summary()
                .or_else(|| editor.ai_chat_pending_no_repo_folder_approval_summary()),
        }
    })
}

fn test_panel(editor: &Editor) -> Option<GuiTestPanel> {
    let panel = editor.test_panel();
    let run = editor
        .is_test_panel_open()
        .then(|| panel.latest())
        .flatten()?;
    let start = run.lines.len().saturating_sub(300);
    Some(GuiTestPanel {
        scope: run.scope_label.to_string(),
        command: run.command.clone(),
        directory: run.dir_name.clone(),
        status: format!("{:?}", run.status).to_lowercase(),
        elapsed_ms: ((run.elapsed().as_millis().min(u64::MAX as u128) as u64) / 100) * 100,
        summary: run.summary.clone(),
        truncated: run.truncated + start,
        lines: run.lines[start..]
            .iter()
            .map(|line| truncate_panel_text(line, 2_000))
            .collect(),
    })
}

fn problem_list(editor: &Editor) -> Option<GuiProblemList> {
    let (kind, list) = if editor.is_quickfix_window_open() {
        ("quickfix", editor.quickfix_list())
    } else if editor.is_location_window_open() {
        ("location", editor.location_list())
    } else {
        return None;
    };
    let selected = list.selected_index();
    let start = selected
        .saturating_sub(60)
        .min(list.len().saturating_sub(200));
    let items = list
        .entries()
        .iter()
        .enumerate()
        .skip(start)
        .take(200)
        .map(|(index, entry)| GuiProblem {
            index,
            severity: format!("{:?}", entry.entry_type).to_lowercase(),
            file: entry
                .filename
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string(),
            line: entry.lnum,
            column: entry.col,
            message: truncate_panel_text(&entry.text, 2_000),
        })
        .collect();
    Some(GuiProblemList {
        kind: kind.to_string(),
        title: list.title().to_string(),
        selected,
        total: list.len(),
        items,
    })
}

fn lsp_manager(editor: &Editor) -> Option<GuiLspManager> {
    let panel = editor.lsp_manager_panel()?;
    let filtered = panel.filtered_entries();
    let selected_position = filtered
        .iter()
        .position(|(index, _)| *index == panel.selected_index)
        .unwrap_or(0);
    let start = centered_window_start(selected_position, filtered.len(), 200);
    let items = filtered
        .into_iter()
        .skip(start)
        .take(200)
        .map(|(index, entry)| GuiLspEntry {
            index,
            language: entry.language_name.clone(),
            section: entry.section.label().to_string(),
            command: entry.lsp_command.clone(),
            state: entry.server_state.clone(),
            installing: panel.active_installs.get(&entry.language_id).map(|status| {
                use crate::editor::lsp_manager_panel::InstallStatus;
                match status {
                    InstallStatus::Installing(message) => message.clone(),
                    InstallStatus::Success => "installed".to_string(),
                    InstallStatus::Failed(error) => format!("failed: {error}"),
                }
            }),
        })
        .collect();
    Some(GuiLspManager {
        filter: panel.filter_query().to_string(),
        selected: panel.selected_index,
        show_detail: panel.show_detail,
        items,
    })
}

fn debug_panel(editor: &Editor) -> Option<GuiDebugPanel> {
    let debug = editor.debug_state();
    (debug.session_active && debug.panels_visible).then(|| GuiDebugPanel {
        running: debug.is_running,
        reason: debug.stop_reason.clone(),
        execution_line: debug.execution_line,
        stack: debug
            .stack_frames
            .iter()
            .take(80)
            .enumerate()
            .map(|(index, frame)| GuiDebugFrame {
                name: frame.name.clone(),
                file: frame
                    .source
                    .as_ref()
                    .and_then(|source| source.name.as_deref().or(source.path.as_deref()))
                    .unwrap_or("")
                    .to_string(),
                line: frame.line,
                selected: index == debug.selected_frame,
            })
            .collect(),
        output: debug
            .output_lines
            .iter()
            .rev()
            .take(300)
            .rev()
            .map(|line| truncate_panel_text(line, 2_000))
            .collect(),
    })
}

fn centered_window_start(selected: usize, total: usize, capacity: usize) -> usize {
    if total <= capacity {
        return 0;
    }
    selected
        .saturating_sub(capacity / 2)
        .min(total.saturating_sub(capacity))
}

fn truncate_panel_text(text: &str, max_graphemes: usize) -> String {
    let truncated = crate::unicode::truncate_graphemes(text, max_graphemes);
    if truncated.len() == text.len() {
        text.to_string()
    } else {
        format!("{truncated}…")
    }
}

fn theme(editor: &Editor) -> GuiTheme {
    let fallback = crate::syntax::ColorScheme::default_dark();
    let scheme = editor.get_color_scheme().unwrap_or(&fallback);
    let syntax_groups = [
        HighlightGroup::Keyword,
        HighlightGroup::Function,
        HighlightGroup::Type,
        HighlightGroup::TypeBuiltin,
        HighlightGroup::String,
        HighlightGroup::Number,
        HighlightGroup::Comment,
        HighlightGroup::Operator,
        HighlightGroup::Variable,
        HighlightGroup::VariableBuiltin,
        HighlightGroup::Macro,
        HighlightGroup::Constant,
        HighlightGroup::Property,
        HighlightGroup::Parameter,
        HighlightGroup::Label,
        HighlightGroup::Punctuation,
        HighlightGroup::PunctuationDelimiter,
        HighlightGroup::Tag,
        HighlightGroup::Constructor,
        HighlightGroup::MarkupItalic,
        HighlightGroup::MarkupBold,
        HighlightGroup::MarkupHeading,
        HighlightGroup::MarkupRaw,
        HighlightGroup::SpecialKey,
        HighlightGroup::Other,
    ];
    let syntax = syntax_groups
        .into_iter()
        .map(|group| {
            (
                syntax_name(group).to_string(),
                color_css(scheme.get_syntax_color(group)),
            )
        })
        .collect();

    GuiTheme {
        name: scheme.name.clone(),
        background: color_css(scheme.get_ui_color(UiGroup::Background)),
        foreground: color_css(scheme.get_ui_color(UiGroup::Foreground)),
        surface: color_css(scheme.get_ui_color(UiGroup::MenuBackground)),
        surface_selected: color_css(scheme.get_ui_color(UiGroup::MenuSelected)),
        border: color_css(scheme.get_ui_color(UiGroup::Border)),
        accent: color_css(scheme.get_ui_color(UiGroup::TabActiveBg)),
        accent_foreground: color_css(scheme.get_ui_color(UiGroup::TabActiveFg)),
        muted: color_css(scheme.get_ui_color(UiGroup::LineNumber)),
        cursor_line: color_css(scheme.get_ui_color(UiGroup::CursorLine)),
        selection: color_css(scheme.get_ui_color(UiGroup::Visual)),
        search: color_css(scheme.get_ui_color(UiGroup::Search)),
        error: color_css(scheme.get_ui_color(UiGroup::Error)),
        warning: color_css(scheme.get_ui_color(UiGroup::Warning)),
        info: color_css(scheme.get_ui_color(UiGroup::Info)),
        success: color_css(scheme.get_ui_color(UiGroup::Success)),
        syntax,
    }
}

fn syntax_name(group: HighlightGroup) -> &'static str {
    match group {
        HighlightGroup::Keyword => "keyword",
        HighlightGroup::Function => "function",
        HighlightGroup::Type => "type",
        HighlightGroup::TypeBuiltin => "type-builtin",
        HighlightGroup::String => "string",
        HighlightGroup::Number => "number",
        HighlightGroup::Comment => "comment",
        HighlightGroup::Operator => "operator",
        HighlightGroup::Variable => "variable",
        HighlightGroup::VariableBuiltin => "variable-builtin",
        HighlightGroup::Macro => "macro",
        HighlightGroup::Constant => "constant",
        HighlightGroup::Property => "property",
        HighlightGroup::Parameter => "parameter",
        HighlightGroup::Label => "label",
        HighlightGroup::Punctuation => "punctuation",
        HighlightGroup::PunctuationDelimiter => "punctuation-delimiter",
        HighlightGroup::Tag => "tag",
        HighlightGroup::Constructor => "constructor",
        HighlightGroup::MarkupItalic => "markup-italic",
        HighlightGroup::MarkupBold => "markup-bold",
        HighlightGroup::MarkupHeading => "markup-heading",
        HighlightGroup::MarkupRaw => "markup-raw",
        HighlightGroup::SpecialKey => "special-key",
        HighlightGroup::Other => "other",
    }
}

fn color_css(color: Color) -> String {
    let (red, green, blue) = match color {
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::White => (229, 229, 229),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::Gray => (204, 204, 204),
        Color::Rgb(red, green, blue) => (red, green, blue),
        Color::Indexed(index) => indexed_rgb(index),
        Color::Reset => (205, 214, 244),
    };
    format!("#{red:02x}{green:02x}{blue:02x}")
}

fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let cube = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (
                component(cube / 36),
                component((cube % 36) / 6),
                component(cube % 6),
            )
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_bounded_and_preserves_cursor_and_syntax() {
        let content: String = (0..200)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let mut editor = Editor::with_content(&content);
        editor.set_file_path("sample.rs".to_string());
        editor.buffer_mut().enable_syntax_highlighting();
        editor.set_viewport_height(20);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(10, GraphemeCol(4));

        let view = snapshot(&editor, 7);

        assert_eq!(view.revision, 7);
        assert_eq!(view.cursor.line, 10);
        assert!(view.lines.len() <= 24);
        assert!(view
            .lines
            .iter()
            .flat_map(|line| &line.segments)
            .any(|span| { span.token.as_deref() == Some("keyword") }));
        assert!(view
            .lines
            .iter()
            .flat_map(|line| &line.segments)
            .any(|span| span.cursor));
    }

    #[test]
    fn xterm_palette_resolution_covers_cube_and_grayscale() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(21), (0, 0, 255));
        assert_eq!(indexed_rgb(231), (255, 255, 255));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }

    #[test]
    fn dropped_gui_image_reaches_chat_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "ovim-gui-image-{}-{}.png",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let mut editor = Editor::new();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();

        attach_chat_images(&mut editor, std::slice::from_ref(&path)).unwrap();
        let view = snapshot(&editor, 1);

        assert_eq!(editor.ai_chat_pending_images().len(), 1);
        assert_eq!(
            view.ai_chat.unwrap().pending_images,
            vec![path.file_name().unwrap().to_string_lossy().to_string()]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unicode_tabs_controls_and_wrap_rows_use_terminal_cell_geometry() {
        let (start, segments) = segments_for_line(
            "\t界\u{7f}e\u{301}",
            0,
            0,
            1,
            Mode::Normal,
            None,
            &[],
            &[],
            4,
            None,
        );

        assert_eq!(start, 0);
        assert_eq!(
            segments.iter().map(|segment| segment.cells).sum::<usize>(),
            9
        );
        assert_eq!(segments[0].text, "    ");
        assert_eq!(segments[1].text, "界");
        assert_eq!(segments[1].cells, 2);
        assert_eq!(segments[2].text, "^?");
        assert!(segments[1].cursor);

        let rows = split_visual_rows(segments, Some(4));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[1].0, 4);
        assert_eq!(rows[2].0, 8);
        assert_eq!(
            rows[0].1.iter().map(|segment| segment.cells).sum::<usize>(),
            4
        );
    }

    #[test]
    fn horizontal_projection_bounds_a_generated_long_line() {
        let text = "x".repeat(100_000);
        let (start, segments) = segments_for_line(
            &text,
            0,
            1,
            0,
            Mode::Normal,
            None,
            &[],
            &[],
            4,
            Some((49_900, 50_100)),
        );

        assert_eq!(start, 49_900);
        assert_eq!(segments.len(), 200);
        assert_eq!(
            segments.iter().map(|segment| segment.cells).sum::<usize>(),
            200
        );
    }

    #[test]
    fn snapshot_projects_the_real_split_tree() {
        let mut editor = Editor::with_content("one\ntwo\nthree\n");
        editor.init_window_manager(100, 30);
        editor.split_window_vertical();
        editor.focus_next_window();
        editor.split_window_horizontal();

        let view = snapshot(&editor, 3);

        assert_eq!(view.panes.len(), 3);
        assert_eq!(view.panes.iter().filter(|pane| pane.focused).count(), 1);
        assert!(matches!(
            view.layout,
            GuiLayoutNode::Split {
                direction,
                second,
                ..
            } if direction == "vertical" && matches!(*second, GuiLayoutNode::Split { ref direction, .. } if direction == "horizontal")
        ));
    }

    #[test]
    fn publish_gate_skips_idle_frames_and_emits_dirty_state_once() {
        let mut editor = Editor::with_content("hello\n");
        let mut revision = 1;
        let mut previous = snapshot(&editor, revision);
        let mut render_version = editor.render_input_version();
        let (updates, receiver) = watch::channel(None);

        publish_if_changed(
            &editor,
            &mut revision,
            &mut previous,
            &mut render_version,
            &updates,
        );
        assert!(receiver.borrow().is_none());

        editor.set_status_message("saved");
        editor.mark_dirty();
        publish_if_changed(
            &editor,
            &mut revision,
            &mut previous,
            &mut render_version,
            &updates,
        );
        assert_eq!(
            receiver.borrow().as_ref().map(|view| view.revision),
            Some(2)
        );

        updates.send_replace(None);
        publish_if_changed(
            &editor,
            &mut revision,
            &mut previous,
            &mut render_version,
            &updates,
        );
        assert!(receiver.borrow().is_none());
    }
}
