//! Native GUI bridge.
//!
//! The editor remains the source of truth. A dedicated runtime thread owns it
//! and exposes compact, serializable view models to desktop frontends. This is
//! intentionally not an HTML text editor pretending to be Vim: keyboard,
//! paste, picker, tab, and file-tree actions all flow back through Ovim's real
//! input/state machinery.

#[cfg(feature = "gui")]
pub mod app;
#[cfg(feature = "gui")]
pub mod browser;
#[cfg(feature = "gui")]
mod menu;
pub mod protocol;
#[cfg(feature = "gui")]
pub mod window;

// The protocol types used to live in this module. They are re-exported so the
// Tauri command layer and the API keep importing them from `gui`, and so that
// `gui::protocol` stays the single place a transport has to look.
pub use protocol::{
    GuiAgentOption, GuiAiChat, GuiAiProfileOption, GuiChatMessage, GuiChatSetup,
    GuiChatSetupAction, GuiCodeAttachment, GuiCodeExplanation, GuiCodeExplanationDiscussion,
    GuiCodeExplanationPage, GuiCommand, GuiCompletion, GuiCompletionItem, GuiCursor, GuiDebugFrame,
    GuiDebugPanel, GuiDiagnostics, GuiFileTree, GuiFileTreeItem, GuiGitChanges, GuiHover,
    GuiKeyInput, GuiLayoutNode, GuiLine, GuiLspEntry, GuiLspManager, GuiPane, GuiPicker,
    GuiPickerItem, GuiProblem, GuiProblemList, GuiPrompt, GuiQueuedChatInput, GuiReply,
    GuiReplyKind, GuiSegment, GuiSnapshot, GuiTab, GuiTestFailure, GuiTestPanel, GuiTheme,
    GuiVectorSource,
};

use crate::cli::FileArg;
use crate::color::Color;
use crate::editor::{Editor, EditorServices, InputHandler, PendingWindowOpen};
use crate::frontend::{
    handle_viewport_resize, process_editor_tick, process_external_file_change,
    process_picker_results, refresh_after_input, FrontendChannels,
};
use crate::git::LineStatus;
use crate::mode::Mode;
use crate::syntax::{HighlightGroup, UiGroup};
use crate::unicode::GraphemeCol;
use anyhow::{Context, Result};
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

fn chat_setup(editor: &Editor) -> Option<GuiChatSetup> {
    if let Some(summary) = editor.codex_auth_dialog_summary() {
        use ovim_core::editor::CodexAuthDialogPhase;
        let (title, detail, actions) = match summary.phase {
            CodexAuthDialogPhase::Offer => (
                "Sign in to Codex",
                summary.detail.unwrap_or_else(|| {
                    "Use your ChatGPT plan for Codex inference inside Ovim.".to_string()
                }),
                vec![
                    ("Sign in", "Enter"),
                    ("Use device code", "d"),
                    ("Open browser", "b"),
                    ("Not now", "Escape"),
                ],
            ),
            CodexAuthDialogPhase::Refreshing => (
                "Refreshing Codex sign-in",
                "Your draft and selection are preserved while Ovim refreshes its credentials."
                    .to_string(),
                vec![("Cancel", "Escape")],
            ),
            CodexAuthDialogPhase::PreparingDeviceCode => (
                "Preparing device sign-in",
                "Requesting a one-time code for SSH or a headless machine.".to_string(),
                vec![("Cancel", "Escape")],
            ),
            CodexAuthDialogPhase::WaitingForDeviceCode => (
                "Finish Codex device sign-in",
                format!(
                    "Open {} and enter the one-time code {}. Continue only if you started this login in Ovim.",
                    summary
                        .authorize_url
                        .as_deref()
                        .unwrap_or("the OpenAI device sign-in page"),
                    summary.user_code.as_deref().unwrap_or("shown by Ovim")
                ),
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
                vec![
                    ("Try again", "Enter"),
                    ("Use device code", "d"),
                    ("Open browser", "b"),
                    ("Cancel", "Escape"),
                ],
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

fn attach_chat_images(editor: &mut Editor, paths: &[std::path::PathBuf]) -> Result<()> {
    if editor.mode() != Mode::AiChat {
        anyhow::bail!("Open AI chat before dropping an image");
    }

    if paths.is_empty() {
        return Ok(());
    }
    let validated = paths
        .iter()
        .map(|path| {
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
            path.to_str()
                .with_context(|| format!("Image path is not valid UTF-8: {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    for path in validated {
        editor.handle_paste_event(path)?;
    }
    Ok(())
}

/// One editor request, with the channel its answer travels back on.
///
/// This is the in-process form: it pairs the serializable
/// [`GuiCommand`] payload with a `oneshot::Sender`, and
/// [`GuiRequest::into_parts`] separates the two again for a transport that
/// cannot carry a channel.
pub enum GuiRequest {
    Snapshot {
        columns: u16,
        rows: u16,
        reply: oneshot::Sender<Result<GuiSnapshot, String>>,
    },
    VectorSource {
        reply: oneshot::Sender<Result<GuiVectorSource, String>>,
    },
    VectorFeedback {
        feedback: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    DiffWorkspace {
        reply: oneshot::Sender<Result<std::path::PathBuf, String>>,
    },
    OpenDiffBuffer {
        title: String,
        content: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Key {
        input: GuiKeyInput,
        reply: oneshot::Sender<Result<(), String>>,
    },
    OpenAiChat {
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpdateChatInput {
        expected_input: String,
        expected_cursor: usize,
        input: String,
        cursor: usize,
        action: Option<GuiKeyInput>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetChatInputCursor {
        offset: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetChatInputWidth {
        columns: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveChatImage {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectAiProfile {
        profile: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectReasoningEffort {
        effort: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectChatMessage {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ManageQueuedChatInput {
        id: u64,
        action: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AiPolicy {
        action: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    EditorCommand {
        command: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectChatAgent {
        agent_id: Option<String>,
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
    AttachImageData {
        name: String,
        data: Vec<u8>,
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
    SelectCompletion {
        index: usize,
        activate: bool,
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
    SelectDebugFrame {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

/// The reply half of a [`GuiRequest`], separated from its payload.
///
/// A [`GuiCommand`] can cross a process boundary but a `oneshot::Sender`
/// cannot, so the two travel apart and are rejoined by
/// [`GuiRequest::from_parts`] on the editor's side of the transport.
pub enum GuiReplySender {
    None,
    Unit(oneshot::Sender<Result<(), String>>),
    Snapshot(oneshot::Sender<Result<GuiSnapshot, String>>),
    VectorSource(oneshot::Sender<Result<GuiVectorSource, String>>),
    Path(oneshot::Sender<Result<std::path::PathBuf, String>>),
}

impl GuiReplySender {
    /// Build the reply channel that `kind` describes.
    ///
    /// Callers get the shape from [`GuiCommand::reply_kind`] rather than
    /// choosing it, which is what makes [`GuiRequest::from_parts`] unable to
    /// pair a command with the wrong channel in practice.
    pub fn channel(kind: GuiReplyKind) -> (Self, GuiReplyReceiver) {
        match kind {
            GuiReplyKind::None => (GuiReplySender::None, GuiReplyReceiver::None),
            GuiReplyKind::Unit => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Unit(tx), GuiReplyReceiver::Unit(rx))
            }
            GuiReplyKind::Snapshot => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Snapshot(tx), GuiReplyReceiver::Snapshot(rx))
            }
            GuiReplyKind::VectorSource => {
                let (tx, rx) = oneshot::channel();
                (
                    GuiReplySender::VectorSource(tx),
                    GuiReplyReceiver::VectorSource(rx),
                )
            }
            GuiReplyKind::Path => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Path(tx), GuiReplyReceiver::Path(rx))
            }
        }
    }

    pub fn kind(&self) -> GuiReplyKind {
        match self {
            GuiReplySender::None => GuiReplyKind::None,
            GuiReplySender::Unit(_) => GuiReplyKind::Unit,
            GuiReplySender::Snapshot(_) => GuiReplyKind::Snapshot,
            GuiReplySender::VectorSource(_) => GuiReplyKind::VectorSource,
            GuiReplySender::Path(_) => GuiReplyKind::Path,
        }
    }
}

/// The receiving end of a [`GuiReplySender`].
pub enum GuiReplyReceiver {
    None,
    Unit(oneshot::Receiver<Result<(), String>>),
    Snapshot(oneshot::Receiver<Result<GuiSnapshot, String>>),
    VectorSource(oneshot::Receiver<Result<GuiVectorSource, String>>),
    Path(oneshot::Receiver<Result<std::path::PathBuf, String>>),
}

impl GuiReplyReceiver {
    /// Await the editor's answer.
    ///
    /// `Ok(None)` means the command is fire-and-forget and there is nothing to
    /// wait for; `Err` means the editor dropped the channel without answering.
    pub async fn recv(self) -> Result<Option<GuiReply>, String> {
        const CLOSED: &str = "The Ovim editor thread closed the response";
        Ok(match self {
            GuiReplyReceiver::None => None,
            GuiReplyReceiver::Unit(rx) => {
                Some(GuiReply::Unit(rx.await.map_err(|_| CLOSED.to_string())?))
            }
            GuiReplyReceiver::Snapshot(rx) => Some(GuiReply::Snapshot(Box::new(
                rx.await.map_err(|_| CLOSED.to_string())?,
            ))),
            GuiReplyReceiver::VectorSource(rx) => Some(GuiReply::VectorSource(
                rx.await.map_err(|_| CLOSED.to_string())?,
            )),
            GuiReplyReceiver::Path(rx) => {
                Some(GuiReply::Path(rx.await.map_err(|_| CLOSED.to_string())?))
            }
        })
    }
}

impl GuiRequest {
    /// Rejoin a wire payload with a reply channel.
    ///
    /// The channel has to match [`GuiCommand::reply_kind`]. A caller that
    /// builds it with [`GuiReplySender::channel`] cannot get that wrong, so a
    /// mismatch is a transport bug and is reported rather than papered over by
    /// answering on a channel nobody is waiting on.
    pub fn from_parts(command: GuiCommand, reply: GuiReplySender) -> Result<Self, String> {
        Ok(match (command, reply) {
            (GuiCommand::Snapshot { columns, rows }, GuiReplySender::Snapshot(reply)) => {
                GuiRequest::Snapshot {
                    columns,
                    rows,
                    reply,
                }
            }
            (GuiCommand::VectorSource, GuiReplySender::VectorSource(reply)) => {
                GuiRequest::VectorSource { reply }
            }
            (GuiCommand::VectorFeedback { feedback }, GuiReplySender::Unit(reply)) => {
                GuiRequest::VectorFeedback { feedback, reply }
            }
            (GuiCommand::DiffWorkspace, GuiReplySender::Path(reply)) => {
                GuiRequest::DiffWorkspace { reply }
            }
            (GuiCommand::OpenDiffBuffer { title, content }, GuiReplySender::Unit(reply)) => {
                GuiRequest::OpenDiffBuffer {
                    title,
                    content,
                    reply,
                }
            }
            (GuiCommand::Key { input }, GuiReplySender::Unit(reply)) => {
                GuiRequest::Key { input, reply }
            }
            (GuiCommand::OpenAiChat, GuiReplySender::Unit(reply)) => {
                GuiRequest::OpenAiChat { reply }
            }
            (
                GuiCommand::UpdateChatInput {
                    expected_input,
                    expected_cursor,
                    input,
                    cursor,
                    action,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::UpdateChatInput {
                expected_input,
                expected_cursor,
                input,
                cursor,
                action,
                reply,
            },
            (GuiCommand::SetChatInputCursor { offset }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SetChatInputCursor { offset, reply }
            }
            (GuiCommand::SetChatInputWidth { columns }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SetChatInputWidth { columns, reply }
            }
            (GuiCommand::RemoveChatImage { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::RemoveChatImage { index, reply }
            }
            (GuiCommand::SelectAiProfile { profile }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectAiProfile { profile, reply }
            }
            (GuiCommand::SelectReasoningEffort { effort }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectReasoningEffort { effort, reply }
            }
            (GuiCommand::SelectChatMessage { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectChatMessage { index, reply }
            }
            (GuiCommand::ManageQueuedChatInput { id, action }, GuiReplySender::Unit(reply)) => {
                GuiRequest::ManageQueuedChatInput { id, action, reply }
            }
            (GuiCommand::AiPolicy { action }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AiPolicy { action, reply }
            }
            (GuiCommand::EditorCommand { command }, GuiReplySender::Unit(reply)) => {
                GuiRequest::EditorCommand { command, reply }
            }
            (GuiCommand::SelectChatAgent { agent_id }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectChatAgent { agent_id, reply }
            }
            (GuiCommand::Paste { text }, GuiReplySender::Unit(reply)) => {
                GuiRequest::Paste { text, reply }
            }
            (GuiCommand::AttachImages { paths }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AttachImages { paths, reply }
            }
            (GuiCommand::AttachImageData { name, data }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AttachImageData { name, data, reply }
            }
            (
                GuiCommand::SetCursor {
                    pane,
                    line,
                    display_column,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::SetCursor {
                pane,
                line,
                display_column,
                reply,
            },
            (GuiCommand::SelectTab { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectTab { index, reply }
            }
            (GuiCommand::FocusPane { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::FocusPane { index, reply }
            }
            (GuiCommand::SelectPicker { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectPicker { index, reply }
            }
            (GuiCommand::SelectCompletion { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectCompletion {
                    index,
                    activate,
                    reply,
                }
            }
            (GuiCommand::SelectFileTree { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectFileTree {
                    index,
                    activate,
                    reply,
                }
            }
            (
                GuiCommand::SelectProblem {
                    kind,
                    index,
                    activate,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::SelectProblem {
                kind,
                index,
                activate,
                reply,
            },
            (GuiCommand::SelectLsp { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectLsp {
                    index,
                    activate,
                    reply,
                }
            }
            (GuiCommand::SelectDebugFrame { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectDebugFrame { index, reply }
            }
            (GuiCommand::Shutdown, GuiReplySender::None) => GuiRequest::Shutdown,
            (command, reply) => {
                return Err(format!(
                    "A GUI command needing a {:?} reply was given a {:?} reply channel",
                    command.reply_kind(),
                    reply.kind()
                ))
            }
        })
    }

    /// Split a request back into the payload a transport can serialize and the
    /// reply channel it cannot.
    ///
    /// This is the exact inverse of [`GuiRequest::from_parts`]; the round-trip
    /// test in this module is what keeps a field from being dropped on the way
    /// through the wire format.
    pub fn into_parts(self) -> (GuiCommand, GuiReplySender) {
        match self {
            GuiRequest::Snapshot {
                columns,
                rows,
                reply,
            } => (
                GuiCommand::Snapshot { columns, rows },
                GuiReplySender::Snapshot(reply),
            ),
            GuiRequest::VectorSource { reply } => (
                GuiCommand::VectorSource,
                GuiReplySender::VectorSource(reply),
            ),
            GuiRequest::VectorFeedback { feedback, reply } => (
                GuiCommand::VectorFeedback { feedback },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::DiffWorkspace { reply } => {
                (GuiCommand::DiffWorkspace, GuiReplySender::Path(reply))
            }
            GuiRequest::OpenDiffBuffer {
                title,
                content,
                reply,
            } => (
                GuiCommand::OpenDiffBuffer { title, content },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Key { input, reply } => {
                (GuiCommand::Key { input }, GuiReplySender::Unit(reply))
            }
            GuiRequest::OpenAiChat { reply } => {
                (GuiCommand::OpenAiChat, GuiReplySender::Unit(reply))
            }
            GuiRequest::UpdateChatInput {
                expected_input,
                expected_cursor,
                input,
                cursor,
                action,
                reply,
            } => (
                GuiCommand::UpdateChatInput {
                    expected_input,
                    expected_cursor,
                    input,
                    cursor,
                    action,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetChatInputCursor { offset, reply } => (
                GuiCommand::SetChatInputCursor { offset },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetChatInputWidth { columns, reply } => (
                GuiCommand::SetChatInputWidth { columns },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::RemoveChatImage { index, reply } => (
                GuiCommand::RemoveChatImage { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectAiProfile { profile, reply } => (
                GuiCommand::SelectAiProfile { profile },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectReasoningEffort { effort, reply } => (
                GuiCommand::SelectReasoningEffort { effort },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectChatMessage { index, reply } => (
                GuiCommand::SelectChatMessage { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::ManageQueuedChatInput { id, action, reply } => (
                GuiCommand::ManageQueuedChatInput { id, action },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::AiPolicy { action, reply } => {
                (GuiCommand::AiPolicy { action }, GuiReplySender::Unit(reply))
            }
            GuiRequest::EditorCommand { command, reply } => (
                GuiCommand::EditorCommand { command },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectChatAgent { agent_id, reply } => (
                GuiCommand::SelectChatAgent { agent_id },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Paste { text, reply } => {
                (GuiCommand::Paste { text }, GuiReplySender::Unit(reply))
            }
            GuiRequest::AttachImages { paths, reply } => (
                GuiCommand::AttachImages { paths },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::AttachImageData { name, data, reply } => (
                GuiCommand::AttachImageData { name, data },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetCursor {
                pane,
                line,
                display_column,
                reply,
            } => (
                GuiCommand::SetCursor {
                    pane,
                    line,
                    display_column,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectTab { index, reply } => {
                (GuiCommand::SelectTab { index }, GuiReplySender::Unit(reply))
            }
            GuiRequest::FocusPane { index, reply } => {
                (GuiCommand::FocusPane { index }, GuiReplySender::Unit(reply))
            }
            GuiRequest::SelectPicker { index, reply } => (
                GuiCommand::SelectPicker { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectCompletion {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectCompletion { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectFileTree {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectFileTree { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectProblem {
                kind,
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectProblem {
                    kind,
                    index,
                    activate,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectLsp {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectLsp { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectDebugFrame { index, reply } => (
                GuiCommand::SelectDebugFrame { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Shutdown => (GuiCommand::Shutdown, GuiReplySender::None),
        }
    }
}

/// Send-side handle stored as Tauri application state.
#[derive(Clone)]
pub struct GuiBridge {
    requests: mpsc::UnboundedSender<GuiRequest>,
    updates: watch::Sender<Option<GuiSnapshot>>,
}

impl GuiBridge {
    pub fn spawn(file: Option<FileArg>, resume: bool, services: EditorServices) -> Result<Self> {
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
                    services,
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

    pub async fn vector_source(&self) -> Result<GuiVectorSource, String> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(GuiRequest::VectorSource { reply })
            .map_err(|_| "The Ovim editor thread has stopped".to_string())?;
        response
            .await
            .map_err(|_| "The Ovim editor thread closed the response".to_string())?
    }

    pub async fn vector_feedback(&self, feedback: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::VectorFeedback { feedback, reply })
            .await
    }

    pub async fn diff_workspace(&self) -> Result<std::path::PathBuf, String> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(GuiRequest::DiffWorkspace { reply })
            .map_err(|_| "The Ovim editor thread has stopped".to_string())?;
        response
            .await
            .map_err(|_| "The Ovim editor thread closed the response".to_string())?
    }

    pub async fn open_diff_buffer(&self, title: String, content: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::OpenDiffBuffer {
            title,
            content,
            reply,
        })
        .await
    }

    pub async fn key(&self, input: GuiKeyInput) -> Result<(), String> {
        self.request(|reply| GuiRequest::Key { input, reply }).await
    }

    pub async fn open_ai_chat(&self) -> Result<(), String> {
        self.request(|reply| GuiRequest::OpenAiChat { reply }).await
    }

    pub async fn paste(&self, text: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::Paste { text, reply })
            .await
    }

    pub async fn set_chat_input_cursor(&self, offset: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SetChatInputCursor { offset, reply })
            .await
    }

    pub async fn update_chat_input(
        &self,
        expected_input: String,
        expected_cursor: usize,
        input: String,
        cursor: usize,
        action: Option<GuiKeyInput>,
    ) -> Result<(), String> {
        self.request(|reply| GuiRequest::UpdateChatInput {
            expected_input,
            expected_cursor,
            input,
            cursor,
            action,
            reply,
        })
        .await
    }

    pub async fn set_chat_input_width(&self, columns: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SetChatInputWidth { columns, reply })
            .await
    }

    pub async fn remove_chat_image(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::RemoveChatImage { index, reply })
            .await
    }

    pub async fn attach_images(&self, paths: Vec<std::path::PathBuf>) -> Result<(), String> {
        self.request(|reply| GuiRequest::AttachImages { paths, reply })
            .await
    }

    pub async fn attach_image_data(&self, name: String, data: Vec<u8>) -> Result<(), String> {
        self.request(|reply| GuiRequest::AttachImageData { name, data, reply })
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

    pub async fn select_completion(&self, index: usize, activate: bool) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectCompletion {
            index,
            activate,
            reply,
        })
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

    pub async fn select_debug_frame(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectDebugFrame { index, reply })
            .await
    }

    pub async fn select_ai_profile(&self, profile: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectAiProfile { profile, reply })
            .await
    }

    pub async fn select_reasoning_effort(&self, effort: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectReasoningEffort { effort, reply })
            .await
    }

    pub async fn select_chat_message(&self, index: usize) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectChatMessage { index, reply })
            .await
    }

    pub async fn manage_queued_chat_input(&self, id: u64, action: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::ManageQueuedChatInput { id, action, reply })
            .await
    }

    pub async fn ai_policy(&self, action: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::AiPolicy { action, reply })
            .await
    }

    pub async fn editor_command(&self, command: String) -> Result<(), String> {
        self.request(|reply| GuiRequest::EditorCommand { command, reply })
            .await
    }

    pub async fn select_chat_agent(&self, agent_id: Option<String>) -> Result<(), String> {
        self.request(|reply| GuiRequest::SelectChatAgent { agent_id, reply })
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
    services: EditorServices,
    mut requests: mpsc::UnboundedReceiver<GuiRequest>,
    updates: watch::Sender<Option<GuiSnapshot>>,
    ready: std_mpsc::SyncSender<Result<(), String>>,
) {
    let mut editor = Editor::new().with_services(services);
    if let Err(error) = editor.enable_lua() {
        editor.set_status_message(format!("Lua configuration: {error}"));
    }
    editor.language_catalog().install_as_process_catalog();

    if let Some(file) = file {
        let path = Path::new(&file.path);
        let result = if path.is_dir() {
            editor.set_workspace_root(path)
        } else {
            editor.load_file_async(path).await
        };
        if let Err(error) = result {
            editor.set_file_path(file.path.clone());
            editor.set_status_message(format!("Could not open {}: {error}", file.path));
        } else {
            if !path.is_dir() {
                if let Some(line) = file.line {
                    editor.buffer_mut().cursor_mut().set_position(
                        line.saturating_sub(1),
                        GraphemeCol(file.col.unwrap_or(1).saturating_sub(1)),
                    );
                    editor.buffer_mut().validate_cursor_position();
                }
            }
            editor.set_mode(Mode::Normal);
        }
    }

    editor.set_ai_conversation_resume_enabled(resume);
    editor.enable_lsp();
    let (java_status_tx, java_status_rx) = mpsc::channel(64);
    crate::lsp_init::init_java_status_sender(java_status_tx);
    let mut channels = FrontendChannels::new(java_status_rx);
    // `:openwin` can raise a modal directory picker that stays up for as long
    // as the user browses, so its outcome returns here asynchronously instead
    // of stalling snapshot publishing behind the dialog.
    let (window_status_tx, mut window_status_rx) = mpsc::unbounded_channel::<String>();
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
                let rejected_terminal = editor.take_pending_terminal_session().is_some();
                let rejected_shell = editor.take_pending_shell_command().is_some();
                if rejected_terminal || rejected_shell {
                    editor.set_status_message("External shell sessions require the TUI frontend".to_string());
                }
                if let Some(pending) = editor.take_pending_window_open() {
                    dispatch_window_open(pending, &window_status_tx);
                }
                publish_if_changed(
                    &editor,
                    &mut revision,
                    &mut last_snapshot,
                    &mut last_render_version,
                    &updates,
                );
            }
            status = window_status_rx.recv() => {
                let Some(status) = status else { continue; };
                editor.set_status_message(status);
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

/// Open a queued project window without blocking the editor loop.
#[cfg(feature = "gui")]
fn dispatch_window_open(pending: PendingWindowOpen, status: &mpsc::UnboundedSender<String>) {
    let status = status.clone();
    tokio::spawn(async move {
        if let Some(message) = window::open_project_window(pending.path).await {
            let _ = status.send(message);
        }
    });
}

/// Without the GUI feature there is no window server to spawn into.
#[cfg(not(feature = "gui"))]
fn dispatch_window_open(_pending: PendingWindowOpen, status: &mpsc::UnboundedSender<String>) {
    let _ = status.send(crate::editor::WINDOW_FRONTEND_REQUIRED.to_string());
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

fn set_gui_chat_input_cursor(editor: &mut Editor, offset: usize) -> Result<()> {
    if editor.mode() != Mode::AiChat {
        anyhow::bail!("AI chat is not active");
    }
    let Some(chat) = editor.ai_state.chat.as_mut() else {
        anyhow::bail!("AI chat is not open");
    };
    if offset > chat.input.len() || !chat.input.is_char_boundary(offset) {
        anyhow::bail!("Chat cursor offset is not a UTF-8 boundary");
    }
    chat.input_cursor = offset;
    chat.focus = ovim_core::ai::chat_types::ChatFocus::TextInput;
    Ok(())
}

fn activate_gui_ai_chat(editor: &mut Editor) -> Result<()> {
    if editor.mode() == Mode::AiChat {
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.focus = ovim_core::ai::chat_types::ChatFocus::TextInput;
        }
        return Ok(());
    }

    // A workbench navigation action supersedes a transient picker. Otherwise
    // the picker would remain latent and reclaim focus when chat closes.
    if editor.picker().is_some() {
        editor.close_picker();
        if editor.mode() == Mode::Picker {
            editor.set_mode(Mode::Normal);
        }
    }

    editor.open_ai_chat(ovim_core::ai::chat_types::ChatOpts {
        name: "chat".into(),
        profile: editor.ai_chat_context_profile("chat"),
        allow_edits: true,
        ..Default::default()
    })
}

fn draft_vector_feedback(editor: &mut Editor, feedback: &str) -> Result<()> {
    let feedback = feedback.trim();
    if feedback.is_empty() {
        anyhow::bail!("Vector feedback cannot be empty");
    }
    if feedback.len() > 8 * 1024 {
        anyhow::bail!("Vector feedback exceeds 8 KiB");
    }
    let file_name = editor
        .buffer()
        .file_path()
        .filter(|path| {
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("strok"))
        })
        .map(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("vector.strok")
                .to_string()
        })
        .ok_or_else(|| anyhow::anyhow!("The active buffer is not a .strok document"))?;
    if editor.ai_state.chat.is_none() {
        editor.open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())?;
    } else {
        editor.set_mode(Mode::AiChat);
    }
    let chat = editor
        .ai_state
        .chat
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("AI chat could not be opened"))?;
    let prompt = format!(
        "Vector review for `{file_name}`:\n\n{feedback}\n\nUpdate the `.strok` source using Strøk's inspect/audit loop."
    );
    if !chat.input.trim().is_empty() {
        chat.input.push_str("\n\n");
    }
    chat.input.push_str(&prompt);
    chat.input_cursor = chat.input.len();
    chat.focus = ovim_core::ai::chat_types::ChatFocus::TextInput;
    refresh_after_input(editor);
    Ok(())
}

fn projected_workspace_path(editor: &Editor) -> Option<std::path::PathBuf> {
    editor
        .file_tree()
        .root_path()
        .map(Path::to_path_buf)
        .or_else(|| {
            editor
                .buffer()
                .file_path()
                .and_then(|path| Path::new(path).parent())
                .map(Path::to_path_buf)
        })
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
        GuiRequest::VectorSource { reply } => {
            let buffer = editor.buffer();
            let Some(path) = buffer.file_path() else {
                let _ = reply.send(Err("The active buffer has no file path".to_string()));
                return;
            };
            let path = Path::new(path);
            if !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("strok"))
            {
                let _ = reply.send(Err("The active buffer is not a .strok document".to_string()));
                return;
            }
            let source = buffer.rope().to_string();
            if source.len() > 4 * 1024 * 1024 {
                let _ = reply.send(Err(
                    "The Strøk document exceeds the 4 MiB preview limit".to_string()
                ));
                return;
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("vector.strok")
                .to_string();
            let _ = reply.send(Ok(GuiVectorSource { source, file_name }));
            return;
        }
        GuiRequest::VectorFeedback { feedback, reply } => {
            let result = draft_vector_feedback(editor, &feedback);
            (reply, result)
        }
        GuiRequest::DiffWorkspace { reply } => {
            let result = projected_workspace_path(editor)
                .ok_or_else(|| anyhow::anyhow!("Open a file in a Git worktree first"))
                .and_then(|path| {
                    ovim_core::native_diff::worktree_root(&path)
                        .map_err(|error| anyhow::anyhow!("{error:#}"))
                });
            let response = result.map_err(|error| error.to_string());
            let _ = reply.send(response);
            return;
        }
        GuiRequest::OpenDiffBuffer {
            title,
            content,
            reply,
        } => {
            editor.open_diff_buffer_in_new_tab(&title, &content);
            (reply, Ok(()))
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
        GuiRequest::OpenAiChat { reply } => {
            let result = activate_gui_ai_chat(editor);
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::UpdateChatInput {
            expected_input,
            expected_cursor,
            input,
            cursor,
            action,
            reply,
        } => {
            let mut result = editor
                .replace_ai_chat_input(&expected_input, expected_cursor, input, cursor)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Chat draft changed before the GUI edit arrived"));
            if result.is_ok() {
                if let Some(action) = action {
                    result = action
                        .into_core()
                        .and_then(|event| InputHandler::handle_key_event_no_dirty(editor, event));
                }
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::ManageQueuedChatInput { id, action, reply } => {
            let changed = match action.as_str() {
                "select" => Ok(editor.select_queued_ai_chat_input(id)),
                "recall" => Ok(editor.recall_queued_ai_chat_input(id)),
                "remove" => Ok(editor.discard_queued_ai_chat_input(id)),
                _ => Err(anyhow::anyhow!("Unknown queued message action: {action}")),
            };
            let result = changed.and_then(|changed| {
                changed
                    .then_some(())
                    .ok_or_else(|| anyhow::anyhow!("Queued message no longer exists"))
            });
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::AiPolicy { action, reply } => {
            let changed = match action.as_str() {
                "toggle-yolo" => editor.set_ai_chat_yolo_mode(!editor.ai_chat_yolo_mode()),
                "toggle-comprehension" => {
                    editor.toggle_ai_chat_comprehension_policy();
                    true
                }
                _ => false,
            };
            let result = changed.then_some(()).ok_or_else(|| {
                anyhow::anyhow!("Unknown or unavailable AI policy action: {action}")
            });
            if result.is_ok() {
                refresh_after_input(editor);
                editor.dispatch_pending_intents().await;
            }
            (reply, result)
        }
        GuiRequest::EditorCommand { command, reply } => {
            let result = match InputHandler::execute_command_api(editor, &command) {
                ovim_core::CommandResult::Success(success) => {
                    if let Some(message) = success.message {
                        editor.set_status_message(message.into_owned());
                    }
                    Ok(())
                }
                ovim_core::CommandResult::Error(error) => {
                    editor.set_status_message(error.error.clone());
                    Err(anyhow::anyhow!(error.error))
                }
            };
            refresh_after_input(editor);
            editor.dispatch_pending_intents().await;
            (reply, result)
        }
        GuiRequest::SelectChatMessage { index, reply } => {
            let result = editor
                .ai_chat_history_select_index(index)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown chat message: {index}"));
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::SelectChatAgent { agent_id, reply } => {
            let result = editor
                .select_ai_agent_for_navigation(agent_id.as_deref())
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown delegated agent"));
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::AttachImageData { name, data, reply } => {
            let result = editor.attach_ai_chat_image_data(&name, data);
            if let Err(error) = &result {
                editor.set_status_message(format!("Image attachment: {error}"));
            }
            refresh_after_input(editor);
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
        GuiRequest::SetChatInputCursor { offset, reply } => {
            let result = set_gui_chat_input_cursor(editor, offset);
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::SetChatInputWidth { columns, reply } => {
            let result = if editor.mode() != Mode::AiChat {
                Err(anyhow::anyhow!("AI chat is not active"))
            } else if columns == 0 || columns > 4096 {
                Err(anyhow::anyhow!("Chat input width is out of range"))
            } else {
                editor.render_cache.ai_chat_input_content_width = columns;
                Ok(())
            };
            (reply, result)
        }
        GuiRequest::RemoveChatImage { index, reply } => {
            let result = editor
                .remove_ai_chat_image(index)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown pending chat image: {index}"));
            if result.is_ok() {
                refresh_after_input(editor);
            }
            (reply, result)
        }
        GuiRequest::SelectAiProfile { profile, reply } => {
            let result = if editor.mode() != Mode::AiChat {
                Err(anyhow::anyhow!("AI chat is not active"))
            } else if editor.ai_set_profile(&profile) {
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    chat.focus = ovim_core::ai::chat_types::ChatFocus::TextInput;
                }
                refresh_after_input(editor);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Unknown AI profile: {profile}"))
            };
            (reply, result)
        }
        GuiRequest::SelectReasoningEffort { effort, reply } => {
            let result = if editor.mode() != Mode::AiChat {
                Err(anyhow::anyhow!("AI chat is not active"))
            } else if editor.set_ai_chat_reasoning_effort(&effort) {
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    chat.focus = ovim_core::ai::chat_types::ChatFocus::TextInput;
                }
                refresh_after_input(editor);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Unknown reasoning effort: {effort}"))
            };
            (reply, result)
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
            let result = editor
                .picker()
                .is_some_and(|picker| index < picker.filtered_result_count())
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown picker item: {index}"));
            if result.is_ok()
                && let Some(picker) = editor.picker_mut()
            {
                picker.set_selected_index(index);
            }
            let result = if result.is_ok() {
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
        GuiRequest::SelectCompletion {
            index,
            activate,
            reply,
        } => {
            let result = editor
                .completion_menu_mut()
                .select_index(index)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown completion item: {index}"));
            if result.is_ok() {
                if activate {
                    editor.accept_completion();
                }
                refresh_after_input(editor);
            }
            (reply, result)
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
        GuiRequest::SelectDebugFrame { index, reply } => {
            let result = (index < editor.debug_state().stack_frames.len())
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("Unknown debug frame: {index}"));
            if result.is_ok() {
                editor.select_stack_frame(index);
                refresh_after_input(editor);
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
    let workspace_path =
        projected_workspace_path(editor).map(|path| path.to_string_lossy().into_owned());
    let project_name = workspace_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("ovim")
        .to_string();
    let language = file_path
        .as_deref()
        .and_then(|path| editor.language_id_for_path(path))
        .unwrap_or_else(|| "plain text".to_string());
    let (errors, warnings, information, hints) = editor.cached_diagnostic_count();
    let (added, modified, removed) = buffer.git_status().change_counts();
    let cursor_display_column = display_column(buffer, cursor.line(), cursor.col().0, tab_width);
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
        workspace_path,
        project_name,
        language,
        encoding: buffer.encoding().display_name().to_string(),
        line_ending: buffer.line_ending().display_name().to_string(),
        modified: buffer.is_modified(),
        has_unsaved_changes: editor.any_buffer_modified(),
        buffer_revision: buffer.version(),
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
        expand_tab: editor.indent_options().expand_tab,
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
                    id: editor
                        .tab_page_manager()
                        .tab(index)
                        .map_or(0, ovim_core::editor::TabPage::id),
                    index,
                    title: editor.get_tab_title(index),
                    active,
                    modified,
                }
            })
            .collect(),
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
                display_column: position
                    .map(|(line, column)| display_column(buffer, line, column, tab_width)),
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

fn display_column(
    buffer: &crate::buffer::Buffer,
    line: usize,
    grapheme_column: usize,
    tab_width: usize,
) -> usize {
    let text = buffer
        .line_text(line)
        .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
        .unwrap_or_default();
    let char_column = crate::unicode::grapheme_to_char_col(&text, GraphemeCol(grapheme_column));
    crate::display::char_col_to_display_col(&text, char_column.0, tab_width)
}

fn diff_line_kind(text: &str) -> &'static str {
    if text.starts_with("@@") {
        "hunk"
    } else if text.starts_with('+') && !text.starts_with("+++") {
        "added"
    } else if text.starts_with('-') && !text.starts_with("---") {
        "removed"
    } else if text.starts_with("diff --git")
        || text.starts_with("index ")
        || text.starts_with("---")
        || text.starts_with("+++")
        || text.starts_with("comparison:")
        || text.starts_with("file:")
    {
        "header"
    } else {
        "context"
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
    let attached_selection = editor
        .ai_chat_pending_code_attachment()
        .filter(|attachment| attachment.buffer_id == buffer.id())
        .map(|attachment| {
            (
                (attachment.start_line, 0),
                (attachment.end_line, usize::MAX),
            )
        });
    let selection = focused
        .then(|| editor.visual_selection().or(attached_selection))
        .flatten();
    let selection_mode = if editor.visual_selection().is_some() {
        editor.mode()
    } else if editor
        .ai_chat_pending_code_attachment()
        .is_some_and(|attachment| attachment.linewise)
    {
        Mode::VisualLine
    } else {
        Mode::Visual
    };
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
            selection_mode,
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

        let diff = buffer
            .display_name()
            .filter(|name| name.starts_with("Diff · "))
            .map(|_| diff_line_kind(&text).to_string());
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
                diff: (!continuation).then(|| diff.clone()).flatten(),
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
                buffer_id: editor.buffer().id(),
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
                buffer_id: buffer.id(),
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
        let focus = editor.ai_chat_focus();
        let selected_index = (focus == ovim_core::ai::chat_types::ChatFocus::MessageHistory)
            .then(|| editor.ai_chat_history_selected_index())
            .flatten();
        let start = selected_index
            .map(|selected| centered_window_start(selected, messages.len(), 40))
            .unwrap_or_else(|| messages.len().saturating_sub(40));
        let (conversation_instance, message_node_ids) = editor
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| {
                editor
                    .ai_state
                    .conversations
                    .get(&(chat.origin_buffer_id, chat.opts.name.clone()))
            })
            .map(|conversation| {
                (
                    conversation.instance_id(),
                    conversation.node_ids_for_active_branch().to_vec(),
                )
            })
            .unwrap_or_default();
        let tool_names = messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .map(|tool| (tool.id.as_str(), tool.name.as_str()))
            .collect::<std::collections::HashMap<_, _>>();
        let selected_queued_id = editor.ai_chat_history_selected_queued_id();
        GuiAiChat {
            profile: editor.ai_chat_effective_profile(),
            pending_code_attachment: editor.ai_chat_pending_code_attachment().map(|attachment| {
                GuiCodeAttachment {
                    buffer_id: attachment.buffer_id,
                    label: attachment.label(),
                    start_line: attachment.start_line,
                    start_column: attachment.start_column,
                    end_line: attachment.end_line,
                    end_column: attachment.end_column,
                    linewise: attachment.linewise,
                }
            }),
            profiles: editor
                .ai_profile_names_sorted()
                .into_iter()
                .filter_map(|id| {
                    editor
                        .ai_state
                        .config
                        .resolve_profile(&id)
                        .map(|profile| GuiAiProfileOption {
                            id,
                            provider: profile.provider.to_string(),
                            model: profile.model.clone(),
                        })
                })
                .collect(),
            reasoning_effort: editor.ai_chat_reasoning_effort(),
            reasoning_effort_selection: editor.ai_chat_reasoning_effort_selection(),
            reasoning_efforts: ovim_core::editor::AI_CHAT_REASONING_EFFORTS
                .iter()
                .map(|effort| (*effort).to_string())
                .collect(),
            yolo_mode: editor.ai_chat_yolo_mode(),
            comprehension_policy: editor.ai_chat_comprehension_policy().as_str().to_string(),
            comprehension_checkpoint: editor
                .ai_chat_comprehension_checkpoint_summary()
                .map(str::to_string),
            activity: editor.ai_chat_activity().as_str().to_string(),
            waiting: editor.ai_chat_waiting(),
            input: editor.ai_chat_input().to_string(),
            input_cursor: editor.ai_chat_input_cursor(),
            pending_images: projected_image_names(editor.ai_chat_pending_images()),
            queued_inputs: editor
                .ai_chat_queued_inputs()
                .map(|item| GuiQueuedChatInput {
                    id: item.id,
                    kind: match item.kind {
                        ovim_core::editor::QueuedChatInputKind::Steer => "steer",
                        ovim_core::editor::QueuedChatInputKind::FollowUp => "followUp",
                        ovim_core::editor::QueuedChatInputKind::Command => "command",
                    }
                    .to_string(),
                    content: truncate_panel_text(&item.content, 8_000),
                    image_count: item.images.len(),
                    has_code_attachment: item.code_attachment.is_some(),
                    selected: selected_queued_id == Some(item.id),
                })
                .collect(),
            setup: chat_setup(editor),
            messages: messages[start..]
                .iter()
                .enumerate()
                .map(|(offset, message)| {
                    let (attachment, visible_content) =
                        ovim_core::editor::split_code_attachment_message(&message.content)
                            .map(|(label, content)| (Some(label), content))
                            .unwrap_or((None, message.content.as_str()));
                    GuiChatMessage {
                        index: start + offset,
                        selected: selected_index == Some(start + offset),
                        id: message_node_ids
                            .get(start + offset)
                            .map(|node_id| format!("{conversation_instance}:{node_id}"))
                            .unwrap_or_else(|| {
                                format!("{conversation_instance}:{}", start + offset)
                            }),
                        role: match message.role {
                            ovim_core::ai::chat_types::ChatRole::User => "user",
                            ovim_core::ai::chat_types::ChatRole::Assistant => "assistant",
                            ovim_core::ai::chat_types::ChatRole::Thinking => "thinking",
                            ovim_core::ai::chat_types::ChatRole::Error => "error",
                            ovim_core::ai::chat_types::ChatRole::Tool => "tool",
                        }
                        .to_string(),
                        content: truncate_panel_text(visible_content, 8_000),
                        attachment,
                        model: message.model.clone(),
                        tool_name: message
                            .tool_call_id
                            .as_deref()
                            .and_then(|id| tool_names.get(id).copied())
                            .map(str::to_string),
                        tools: message
                            .tool_calls
                            .iter()
                            .take(24)
                            .map(|tool| tool.name.clone())
                            .collect(),
                        images: projected_image_names(&message.images),
                    }
                })
                .collect(),
            streaming: editor
                .ai_chat_streaming_content()
                .filter(|content| !content.is_empty())
                .map(|content| truncate_panel_text(content, 12_000)),
            streaming_thinking: editor
                .ai_chat_streaming_thinking()
                .filter(|content| !content.is_empty())
                .map(|content| truncate_panel_text(content, 12_000)),
            thinking_live: editor.ai_chat_streaming_thinking().is_some(),
            focus: match focus {
                ovim_core::ai::chat_types::ChatFocus::TextInput => "textInput",
                ovim_core::ai::chat_types::ChatFocus::MessageHistory => "messageHistory",
                ovim_core::ai::chat_types::ChatFocus::ModelSelector => "modelSelector",
                ovim_core::ai::chat_types::ChatFocus::TreePanel => "treePanel",
            }
            .to_string(),
            agents: editor
                .ai_agent_current_snapshot()
                .ok()
                .flatten()
                .map(|snapshot| {
                    snapshot
                        .hierarchy()
                        .into_iter()
                        .map(|agent| GuiAgentOption {
                            id: agent.agent_id.to_string(),
                            task_name: agent.task_name.clone(),
                            lifecycle: agent.lifecycle.clone(),
                            model: agent.resolved_route.model.clone(),
                            depth: agent.ancestry.len().saturating_sub(1),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            selected_agent_id: editor.ai_agent_selected_id().map(str::to_string),
            followed_agent_id: editor.ai_agent_followed_id().map(str::to_string),
            agent_cursor: editor.ai_agent_tree_cursor(),
            approval: editor
                .ai_chat_pending_tool_approval_summary()
                .or_else(|| editor.ai_chat_pending_no_repo_folder_approval_summary()),
            code_explanation: code_explanation(editor),
        }
    })
}

fn projected_image_names(images: &[ovim_core::ai::chat_types::ImageAttachment]) -> Vec<String> {
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let name = image
                .path
                .file_name()
                .unwrap_or(image.path.as_os_str())
                .to_string_lossy();
            if name.starts_with("clipboard-") {
                format!("Pasted image {}", index + 1)
            } else {
                name.to_string()
            }
        })
        .collect()
}

fn code_explanation(editor: &Editor) -> Option<GuiCodeExplanation> {
    let view = editor.ai_code_explanation_view()?;
    let page = match view.page {
        ovim_core::editor::CodeExplanationPageView::Concept { title, body } => {
            GuiCodeExplanationPage::Concept { title, body }
        }
        ovim_core::editor::CodeExplanationPageView::Code {
            path,
            start_line,
            end_line,
            comment,
        } => GuiCodeExplanationPage::Code {
            path,
            start_line,
            end_line,
            comment,
        },
    };
    let discussion = match view.discussion {
        ovim_core::editor::CodeExplanationDiscussionView::Navigating {
            question_count,
            latest_question,
            latest_answer,
            latest_failed,
        } => GuiCodeExplanationDiscussion::Navigating {
            question_count,
            latest_question,
            latest_answer,
            latest_failed,
        },
        ovim_core::editor::CodeExplanationDiscussionView::Composing {
            input,
            cursor,
            question_count,
        } => GuiCodeExplanationDiscussion::Composing {
            input,
            cursor,
            question_count,
        },
        ovim_core::editor::CodeExplanationDiscussionView::Answering {
            question,
            answer,
            question_count,
        } => GuiCodeExplanationDiscussion::Answering {
            question,
            answer,
            question_count,
        },
    };
    Some(GuiCodeExplanation {
        current: view.current,
        total: view.total,
        page,
        discussion,
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
        failure: run.failures.first().map(|failure| GuiTestFailure {
            message: failure.message.clone(),
            file: failure
                .location
                .as_ref()
                .map(|location| location.path.to_string_lossy().to_string()),
            line: failure.location.as_ref().map(|location| location.line),
            column: failure
                .location
                .as_ref()
                .and_then(|location| location.column),
        }),
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
            install_hint: entry.install_hint.clone(),
            extensions: entry.extensions.clone(),
            root_markers: entry.root_markers.clone(),
            capabilities: entry.capabilities.clone(),
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
        HighlightGroup::DiffAdded => "diff-added",
        HighlightGroup::DiffRemoved => "diff-removed",
        HighlightGroup::DiffHeader => "diff-header",
        HighlightGroup::DiffLocation => "diff-location",
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
    fn a_realistic_snapshot_survives_a_json_round_trip() {
        // The frontend only ever sees `snapshot()`'s output, so the round trip
        // is checked against a projected frame rather than a hand-built one:
        // splits, tabs, syntax segments, an open chat, and a status message
        // between them reach every branch a plain buffer would leave empty.
        let content: String = (0..80)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let mut editor = Editor::with_content(&content);
        editor.set_file_path("src/sample.rs".to_string());
        editor.buffer_mut().enable_syntax_highlighting();
        editor.set_viewport_height(20);
        editor.init_window_manager(120, 40);
        editor.split_window_vertical();
        editor.focus_next_window();
        editor.split_window_horizontal();
        editor.new_tab();
        // Come back to the source buffer so the projected frame is the
        // interesting one rather than the empty scratch tab.
        editor.first_tab();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        editor.ai_state.chat.as_mut().unwrap().input = "explain this rope".to_string();
        editor.set_status_message("written");

        let view = snapshot(&editor, 12);

        assert!(view.panes.len() > 1, "the fixture should produce splits");
        assert!(view.tabs.len() > 1, "the fixture should produce tabs");
        assert!(view.ai_chat.is_some(), "the fixture should open the chat");
        assert!(matches!(view.layout, GuiLayoutNode::Split { .. }));
        assert!(view
            .lines
            .iter()
            .flat_map(|line| &line.segments)
            .any(|segment| segment.token.is_some()));
        assert_eq!(protocol::round_trip(&view), view);
    }

    #[test]
    fn the_snapshot_wire_format_stays_camel_case() {
        // The TypeScript frontend reads these exact keys. Adding `Deserialize`
        // must not have renamed anything, so a few representative fields --
        // one plain, one nested struct, one internally tagged enum -- are
        // pinned here.
        let mut editor = Editor::with_content("hello\n");
        editor.set_file_path("src/sample.rs".to_string());

        let json = serde_json::to_value(snapshot(&editor, 1)).unwrap();

        assert_eq!(json["fileName"], "sample.rs");
        assert!(json["totalLines"].is_number());
        assert!(json["gitChanges"]["added"].is_number());
        assert_eq!(json["layout"]["kind"], "pane");
        assert!(json.get("file_name").is_none());
    }

    #[test]
    fn every_command_rebuilds_the_request_it_came_from_without_losing_a_field() {
        // `into_parts` is the inverse of `from_parts`, so a payload field that
        // either conversion forgets shows up as an inequality here.
        for command in protocol::sample_commands() {
            let (reply, _receiver) = GuiReplySender::channel(command.reply_kind());
            let request = GuiRequest::from_parts(command.clone(), reply)
                .unwrap_or_else(|error| panic!("{command:?} could not be rebuilt: {error}"));

            let (recovered, reply) = request.into_parts();

            assert_eq!(recovered, command);
            assert_eq!(reply.kind(), command.reply_kind());
        }
    }

    #[test]
    fn a_reply_channel_of_the_wrong_shape_is_refused_rather_than_mis_wired() {
        let (reply, _receiver) = GuiReplySender::channel(GuiReplyKind::Unit);

        let error = GuiRequest::from_parts(
            GuiCommand::Snapshot {
                columns: 80,
                rows: 24,
            },
            reply,
        )
        .err()
        .expect("a mismatched reply channel must be refused");

        assert!(error.contains("Snapshot"), "{error}");
        assert!(error.contains("Unit"), "{error}");
    }

    #[tokio::test]
    async fn a_rebuilt_request_answers_on_the_channel_its_command_asked_for() {
        let command = GuiCommand::DiffWorkspace;
        let (reply, receiver) = GuiReplySender::channel(command.reply_kind());
        let request = GuiRequest::from_parts(command, reply).unwrap();

        let GuiRequest::DiffWorkspace { reply } = request else {
            panic!("a DiffWorkspace command must rebuild a DiffWorkspace request");
        };
        reply
            .send(Ok(std::path::PathBuf::from("workspace/project")))
            .unwrap();

        assert_eq!(
            receiver.recv().await.unwrap(),
            Some(GuiReply::Path(Ok(std::path::PathBuf::from(
                "workspace/project"
            ))))
        );
    }

    #[tokio::test]
    async fn a_fire_and_forget_command_has_nothing_to_wait_for() {
        let command = GuiCommand::Shutdown;
        let (reply, receiver) = GuiReplySender::channel(command.reply_kind());

        let request = GuiRequest::from_parts(command, reply).unwrap();

        assert!(matches!(request, GuiRequest::Shutdown));
        assert_eq!(receiver.recv().await.unwrap(), None);
    }

    #[test]
    fn gui_chat_activation_dismisses_a_transient_picker() {
        let workspace = tempfile::tempdir().unwrap();
        let mut editor = Editor::new();
        editor.set_picker(crate::editor::Picker::new_file_finder(
            workspace.path().to_path_buf(),
            workspace.path().to_path_buf(),
        ));
        editor.set_mode(Mode::Picker);

        activate_gui_ai_chat(&mut editor).unwrap();

        assert_eq!(editor.mode(), Mode::AiChat);
        assert!(editor.picker().is_none());
        editor.close_ai_chat();
        assert_eq!(editor.mode(), Mode::Normal);
    }

    #[test]
    fn gui_chat_activation_is_idempotent_and_preserves_the_draft() {
        let mut editor = Editor::new();
        let mode = editor.mode();
        activate_gui_ai_chat(&mut editor).unwrap();
        editor.ai_state.chat.as_mut().unwrap().input = "keep this draft".into();

        activate_gui_ai_chat(&mut editor).unwrap();

        assert_eq!(editor.mode(), Mode::AiChat);
        assert_eq!(editor.ai_chat_input(), "keep this draft");
        editor.close_ai_chat();
        assert_eq!(editor.mode(), mode);
    }

    #[test]
    fn overlay_columns_use_rendered_tab_and_wide_character_widths() {
        let editor = Editor::with_content("\t界x\n");

        assert_eq!(display_column(editor.buffer(), 0, 1, 4), 4);
        assert_eq!(display_column(editor.buffer(), 0, 2, 4), 6);
    }

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
        assert!(view.expand_tab);
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
    fn diff_visual_selection_becomes_an_inline_chat_attachment() {
        let mut editor = Editor::with_content("");
        editor.open_diff_buffer_in_new_tab(
            "Diff · src/main.rs",
            "comparison: HEAD...WORKTREE\nfile: src/main.rs\n\n@@ -1 +1 @@\n-old\n+new\n",
        );
        editor.set_mode(Mode::VisualLine);
        editor.set_visual_start(4, 0);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(5, GraphemeCol(0));
        editor.start_ai_chat_from_visual().unwrap();

        let attachment = editor.ai_chat_pending_code_attachment().unwrap();
        assert_eq!(attachment.path.as_deref(), Some("src/main.rs"));
        assert_eq!(
            attachment.source_context.as_deref(),
            Some("comparison: HEAD...WORKTREE\nfile: src/main.rs")
        );
        let view = snapshot(&editor, 1);
        let projected = view.ai_chat.unwrap().pending_code_attachment.unwrap();
        assert_eq!(projected.label, "src/main.rs:5–6");
        assert!(view.lines[4..=5]
            .iter()
            .flat_map(|line| &line.segments)
            .all(|segment| segment.selected));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gui_chat_cursor_accepts_only_full_draft_utf8_boundaries() {
        let mut editor = Editor::with_content("");
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "a界b".into();
        chat.input_cursor = chat.input.len();

        assert!(set_gui_chat_input_cursor(&mut editor, 1).is_ok());
        assert_eq!(editor.ai_chat_input_cursor(), 1);
        assert!(set_gui_chat_input_cursor(&mut editor, 2).is_err());
        assert_eq!(editor.ai_chat_input_cursor(), 1);
        assert!(set_gui_chat_input_cursor(&mut editor, 4).is_ok());
        assert_eq!(editor.ai_chat_input_cursor(), 4);
        assert_eq!(
            editor.ai_chat_focus(),
            ovim_core::ai::chat_types::ChatFocus::TextInput
        );
        let profile = editor.ai_chat_effective_profile();
        assert!(editor.ai_set_profile(&profile));
        assert!(editor.set_ai_chat_reasoning_effort("high"));
        assert_eq!(editor.ai_chat_input(), "a界b");
        let projected = snapshot(&editor, 1).ai_chat.unwrap();
        assert!(projected.profiles.iter().any(|option| option.id == profile));
        assert_eq!(projected.reasoning_effort_selection, "high");
        assert!(projected
            .reasoning_efforts
            .iter()
            .any(|effort| effort == "max"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_projects_active_code_walkthrough() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let file = dir.path().join("demo.rs");
        std::fs::write(&file, "fn demo() {}\n").unwrap();
        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        let call = ovim_core::ai::chat_types::ToolCallInfo {
            id: "gui-walkthrough".into(),
            name: "explain_with_codebase".into(),
            arguments: serde_json::json!({
                "steps": [
                    {
                        "type": "concept",
                        "title": "The entry point",
                        "body": "Start with the user-visible behavior."
                    },
                    {
                        "type": "code",
                        "path": "demo.rs",
                        "start_line": 1,
                        "comment": "This function is the entry point."
                    }
                ]
            }),
        };
        let key = {
            let chat = editor.ai_state.chat.as_ref().unwrap();
            (chat.origin_buffer_id, chat.opts.name.clone())
        };
        let conversation = editor.ai_state.conversations.get_mut(&key).unwrap();
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![call.clone()],
        );
        conversation.append_tool_result(call.id.clone(), "Walkthrough complete.".into());
        assert!(
            editor.replay_code_explanation(&call.id),
            "{}",
            editor.status_message()
        );
        editor.ai_state.chat.as_mut().unwrap().streaming_thinking =
            Some("Inspecting the walkthrough state".into());

        let chat_snapshot = snapshot(&editor, 1).ai_chat.unwrap();
        assert!(chat_snapshot.thinking_live);
        assert_eq!(
            chat_snapshot.streaming_thinking.as_deref(),
            Some("Inspecting the walkthrough state")
        );
        assert!(chat_snapshot
            .messages
            .iter()
            .all(|message| message.id.split_once(':').is_some()));
        let walkthrough = chat_snapshot.code_explanation.expect("active walkthrough");
        assert_eq!(walkthrough.current, 1);
        assert_eq!(walkthrough.total, 2);
        assert!(matches!(
            walkthrough.page,
            GuiCodeExplanationPage::Concept { ref title, ref body }
                if title == "The entry point" && body.contains("user-visible")
        ));
        assert!(matches!(
            walkthrough.discussion,
            GuiCodeExplanationDiscussion::Navigating {
                question_count: 0,
                ..
            }
        ));

        assert!(editor.move_code_explanation(true));
        let code_page = snapshot(&editor, 2)
            .ai_chat
            .unwrap()
            .code_explanation
            .expect("code walkthrough page");
        assert_eq!(code_page.current, 2);
        assert!(matches!(
            code_page.page,
            GuiCodeExplanationPage::Code {
                ref path,
                start_line: 1,
                end_line: 1,
                ref comment,
            } if path == "demo.rs" && comment.contains("entry point")
        ));
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
        editor
            .attach_ai_chat_image_data("clipboard.png", b"\x89PNG\r\n\x1a\nfixture".to_vec())
            .unwrap();
        assert_eq!(
            snapshot(&editor, 2).ai_chat.unwrap().pending_images,
            vec![
                path.file_name().unwrap().to_string_lossy().to_string(),
                "Pasted image 2".to_string(),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn submitted_image_remains_visible_in_the_transcript_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("layout.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let mut editor = Editor::new();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        attach_chat_images(&mut editor, std::slice::from_ref(&path)).unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "Inspect this layout".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().unwrap();

        let view = snapshot(&editor, 1).ai_chat.unwrap();
        assert!(view.pending_images.is_empty());
        assert_eq!(view.messages[0].images, vec!["layout.png"]);
        editor.cancel_ai_chat_generation();
    }

    #[test]
    fn invalid_multi_image_drop_does_not_partially_attach() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.png");
        let invalid = directory.path().join("notes.txt");
        std::fs::write(&valid, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        std::fs::write(&invalid, b"not an image").unwrap();
        let mut editor = Editor::new();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();

        let error = attach_chat_images(&mut editor, &[valid, invalid]).unwrap_err();

        assert!(error.to_string().contains("Unsupported image format"));
        assert!(editor.ai_chat_pending_images().is_empty());
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

    #[test]
    fn snapshot_projects_ai_safety_controls() {
        let mut editor = Editor::new();
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        assert!(editor.set_ai_chat_yolo_mode(true));
        assert!(editor
            .set_ai_chat_comprehension_policy(ovim_core::editor::ComprehensionPolicy::Commit,));

        let chat = snapshot(&editor, 1).ai_chat.expect("chat projection");
        assert!(chat.yolo_mode);
        assert_eq!(chat.comprehension_policy, "commit");
        assert!(chat.comprehension_checkpoint.is_none());
    }

    #[test]
    fn vector_feedback_opens_chat_and_preserves_an_existing_draft() {
        let mut editor = Editor::with_content("documentsize 24x24\n");
        editor.set_file_path("icons/close.STROK".to_string());
        editor
            .open_ai_chat(ovim_core::ai::chat_types::ChatOpts::default())
            .unwrap();
        editor.ai_state.chat.as_mut().unwrap().input = "Keep this context.".to_string();

        draft_vector_feedback(&mut editor, "Make the stroke feel less heavy.").unwrap();

        assert_eq!(editor.mode(), Mode::AiChat);
        let input = &editor.ai_state.chat.as_ref().unwrap().input;
        assert!(input.starts_with("Keep this context.\n\n"));
        assert!(input.contains("Vector review for `close.STROK`"));
        assert!(input.contains("Make the stroke feel less heavy."));
        assert_eq!(editor.ai_chat_input_cursor(), input.len());
    }

    #[test]
    fn vector_feedback_rejects_non_strok_buffers() {
        let mut editor = Editor::with_content("hello\n");
        editor.set_file_path("notes.txt".to_string());
        assert!(draft_vector_feedback(&mut editor, "change this").is_err());
        assert!(editor.ai_state.chat.is_none());
    }
}
