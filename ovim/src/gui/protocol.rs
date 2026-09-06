//! The serializable half of the GUI protocol.
//!
//! Everything a frontend needs to render Ovim, and everything it can ask Ovim
//! to do, is expressed here as plain data: [`GuiSnapshot`] is a fully
//! projected frame, [`GuiCommand`] is a request with its reply channel
//! removed, and [`GuiReply`] is the answer. Because nothing in this module
//! touches Tauri, an editor loop can produce these values with the GUI feature
//! switched off, which is what lets the same conversation run over a socket
//! instead of an in-process channel.
//!
//! The wire format is JSON and it is consumed by the TypeScript frontend in
//! `ovim/gui/src`, so field renames here are breaking changes there.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl GuiKeyInput {
    pub(super) fn into_core(self) -> Result<ovim_core::key::KeyEvent> {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiSnapshot {
    pub revision: u64,
    pub mode: String,
    pub dashboard: bool,
    pub file_path: Option<String>,
    pub file_name: String,
    pub workspace_path: Option<String>,
    pub project_name: String,
    pub language: String,
    pub encoding: String,
    pub line_ending: String,
    pub modified: bool,
    pub has_unsaved_changes: bool,
    pub buffer_revision: usize,
    pub read_only: bool,
    pub selection_text: Option<String>,
    pub cursor: GuiCursor,
    pub horizontal_offset: usize,
    pub wrap: bool,
    pub tab_width: usize,
    pub expand_tab: bool,
    pub first_line: usize,
    pub total_lines: usize,
    pub lines: Vec<GuiLine>,
    pub layout: GuiLayoutNode,
    pub panes: Vec<GuiPane>,
    pub tabs: Vec<GuiTab>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCursor {
    pub line: usize,
    pub column: usize,
    pub display_column: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLine {
    pub number: usize,
    pub continuation: bool,
    pub display_start: usize,
    pub current: bool,
    pub segments: Vec<GuiSegment>,
    pub git: Option<String>,
    pub diagnostic: Option<String>,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPane {
    pub index: usize,
    pub buffer_id: u64,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiSegment {
    pub text: String,
    pub cells: usize,
    pub token: Option<String>,
    pub cursor: bool,
    pub selected: bool,
    pub search_match: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTab {
    pub id: u64,
    pub index: usize,
    pub title: String,
    pub active: bool,
    pub modified: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiGitChanges {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDiagnostics {
    pub errors: usize,
    pub warnings: usize,
    pub information: usize,
    pub hints: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPrompt {
    pub prefix: String,
    pub text: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPicker {
    pub title: String,
    pub query: String,
    pub file_filter: Option<String>,
    pub selected: usize,
    pub total: usize,
    pub items: Vec<GuiPickerItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPickerItem {
    pub index: usize,
    pub display: String,
    pub location: String,
    pub detail: Option<String>,
    pub matched: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCompletion {
    pub selected: usize,
    pub items: Vec<GuiCompletionItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCompletionItem {
    pub index: usize,
    pub label: String,
    pub detail: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiHover {
    pub content: String,
    pub line: Option<usize>,
    pub display_column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiFileTree {
    pub root: String,
    pub selected: usize,
    pub items: Vec<GuiFileTreeItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiFileTreeItem {
    pub index: usize,
    pub name: String,
    pub path: String,
    pub depth: usize,
    pub directory: bool,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiAiChat {
    pub profile: String,
    pub pending_code_attachment: Option<GuiCodeAttachment>,
    pub profiles: Vec<GuiAiProfileOption>,
    pub reasoning_effort: String,
    pub reasoning_effort_selection: String,
    pub reasoning_efforts: Vec<String>,
    pub yolo_mode: bool,
    pub comprehension_policy: String,
    pub comprehension_checkpoint: Option<String>,
    pub activity: String,
    pub waiting: bool,
    pub input: String,
    pub input_cursor: usize,
    pub pending_images: Vec<String>,
    pub queued_inputs: Vec<GuiQueuedChatInput>,
    pub setup: Option<GuiChatSetup>,
    pub messages: Vec<GuiChatMessage>,
    pub streaming: Option<String>,
    pub streaming_thinking: Option<String>,
    pub thinking_live: bool,
    pub focus: String,
    pub agents: Vec<GuiAgentOption>,
    pub selected_agent_id: Option<String>,
    pub followed_agent_id: Option<String>,
    pub agent_cursor: usize,
    pub approval: Option<String>,
    pub code_explanation: Option<GuiCodeExplanation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCodeAttachment {
    pub buffer_id: u64,
    pub label: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub linewise: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiQueuedChatInput {
    pub id: u64,
    pub kind: String,
    pub content: String,
    pub image_count: usize,
    pub has_code_attachment: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiCodeExplanation {
    pub current: usize,
    pub total: usize,
    pub page: GuiCodeExplanationPage,
    pub discussion: GuiCodeExplanationDiscussion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GuiCodeExplanationPage {
    Concept {
        title: String,
        body: String,
    },
    Code {
        path: String,
        start_line: usize,
        end_line: usize,
        comment: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum GuiCodeExplanationDiscussion {
    Navigating {
        question_count: usize,
        latest_question: Option<String>,
        latest_answer: Option<String>,
        latest_failed: bool,
    },
    Composing {
        input: String,
        cursor: usize,
        question_count: usize,
    },
    Answering {
        question: String,
        answer: String,
        question_count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatMessage {
    pub id: String,
    pub index: usize,
    pub selected: bool,
    pub role: String,
    pub content: String,
    pub attachment: Option<String>,
    pub model: Option<String>,
    pub tool_name: Option<String>,
    pub tools: Vec<String>,
    pub images: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTestPanel {
    pub scope: String,
    pub command: String,
    pub directory: String,
    pub status: String,
    pub elapsed_ms: u64,
    pub summary: Option<String>,
    pub failure: Option<GuiTestFailure>,
    pub truncated: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTestFailure {
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiProblemList {
    pub kind: String,
    pub title: String,
    pub selected: usize,
    pub total: usize,
    pub items: Vec<GuiProblem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiProblem {
    pub index: usize,
    pub severity: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLspManager {
    pub filter: String,
    pub selected: usize,
    pub show_detail: bool,
    pub items: Vec<GuiLspEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLspEntry {
    pub index: usize,
    pub language: String,
    pub section: String,
    pub command: Option<String>,
    pub state: Option<String>,
    pub installing: Option<String>,
    pub install_hint: Option<String>,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDebugPanel {
    pub running: bool,
    pub reason: Option<String>,
    pub execution_line: Option<u64>,
    pub stack: Vec<GuiDebugFrame>,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiDebugFrame {
    pub name: String,
    pub file: String,
    pub line: u64,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiAgentOption {
    pub id: String,
    pub task_name: String,
    pub lifecycle: String,
    pub model: String,
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiAiProfileOption {
    pub id: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatSetupAction {
    pub label: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiVectorSource {
    pub source: String,
    pub file_name: String,
}

/// A [`GuiRequest`](super::GuiRequest) payload with its reply channel removed.
///
/// `GuiRequest` embeds a `oneshot::Sender`, so it can never leave the process.
/// `GuiCommand` carries the same information as data; a transport pairs it
/// with a reply channel of its own to rebuild the request on the editor side.
/// The two enums are kept variant-for-variant identical on purpose — the
/// conversions in `gui::mod` are exhaustive, so adding a request variant
/// without a command variant fails to compile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "command",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GuiCommand {
    Snapshot {
        columns: u16,
        rows: u16,
    },
    VectorSource,
    VectorFeedback {
        feedback: String,
    },
    DiffWorkspace,
    OpenDiffBuffer {
        title: String,
        content: String,
    },
    Key {
        input: GuiKeyInput,
    },
    OpenAiChat,
    UpdateChatInput {
        expected_input: String,
        expected_cursor: usize,
        input: String,
        cursor: usize,
        action: Option<GuiKeyInput>,
    },
    SetChatInputCursor {
        offset: usize,
    },
    SetChatInputWidth {
        columns: usize,
    },
    RemoveChatImage {
        index: usize,
    },
    SelectAiProfile {
        profile: String,
    },
    SelectReasoningEffort {
        effort: String,
    },
    SelectChatMessage {
        index: usize,
    },
    ManageQueuedChatInput {
        id: u64,
        action: String,
    },
    AiPolicy {
        action: String,
    },
    EditorCommand {
        command: String,
    },
    SelectChatAgent {
        agent_id: Option<String>,
    },
    Paste {
        text: String,
    },
    /// Paths naming files on the host that *runs the editor*.
    ///
    /// A remote frontend cannot use this: its drag-and-drop paths are local.
    /// [`GuiCommand::AttachImageData`] is the transport-independent form.
    AttachImages {
        paths: Vec<PathBuf>,
    },
    AttachImageData {
        name: String,
        data: Vec<u8>,
    },
    SetCursor {
        pane: usize,
        line: usize,
        display_column: usize,
    },
    SelectTab {
        index: usize,
    },
    FocusPane {
        index: usize,
    },
    SelectPicker {
        index: usize,
    },
    SelectCompletion {
        index: usize,
        activate: bool,
    },
    SelectFileTree {
        index: usize,
        activate: bool,
    },
    SelectProblem {
        kind: String,
        index: usize,
        activate: bool,
    },
    SelectLsp {
        index: usize,
        activate: bool,
    },
    SelectDebugFrame {
        index: usize,
    },
    Shutdown,
}

impl GuiCommand {
    /// The reply this command produces.
    ///
    /// A transport needs the shape before it sends anything, so it can build a
    /// correctly typed reply channel. Deriving it from the command rather than
    /// letting the caller choose keeps the two from drifting apart.
    pub fn reply_kind(&self) -> GuiReplyKind {
        match self {
            GuiCommand::Snapshot { .. } => GuiReplyKind::Snapshot,
            GuiCommand::VectorSource => GuiReplyKind::VectorSource,
            GuiCommand::DiffWorkspace => GuiReplyKind::Path,
            GuiCommand::Shutdown => GuiReplyKind::None,
            GuiCommand::VectorFeedback { .. }
            | GuiCommand::OpenDiffBuffer { .. }
            | GuiCommand::Key { .. }
            | GuiCommand::OpenAiChat
            | GuiCommand::UpdateChatInput { .. }
            | GuiCommand::SetChatInputCursor { .. }
            | GuiCommand::SetChatInputWidth { .. }
            | GuiCommand::RemoveChatImage { .. }
            | GuiCommand::SelectAiProfile { .. }
            | GuiCommand::SelectReasoningEffort { .. }
            | GuiCommand::SelectChatMessage { .. }
            | GuiCommand::ManageQueuedChatInput { .. }
            | GuiCommand::AiPolicy { .. }
            | GuiCommand::EditorCommand { .. }
            | GuiCommand::SelectChatAgent { .. }
            | GuiCommand::Paste { .. }
            | GuiCommand::AttachImages { .. }
            | GuiCommand::AttachImageData { .. }
            | GuiCommand::SetCursor { .. }
            | GuiCommand::SelectTab { .. }
            | GuiCommand::FocusPane { .. }
            | GuiCommand::SelectPicker { .. }
            | GuiCommand::SelectCompletion { .. }
            | GuiCommand::SelectFileTree { .. }
            | GuiCommand::SelectProblem { .. }
            | GuiCommand::SelectLsp { .. }
            | GuiCommand::SelectDebugFrame { .. } => GuiReplyKind::Unit,
        }
    }
}

/// Which [`GuiReply`] a [`GuiCommand`] answers with.
///
/// `None` is not a reply variant: it means the command is fire-and-forget and
/// the editor never answers at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuiReplyKind {
    None,
    Unit,
    Snapshot,
    VectorSource,
    Path,
}

/// The answer to a [`GuiCommand`].
///
/// Thirty-one commands share four reply shapes, so the reply is tagged by
/// shape rather than by command. Both this enum and [`GuiCommand`] use
/// adjacent tagging: an internal tag would collide with the payload's own
/// fields (`EditorCommand` has a `command` field) and cannot wrap a `Result`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", content = "result", rename_all = "camelCase")]
pub enum GuiReply {
    Unit(Result<(), String>),
    /// Boxed because a projected frame dwarfs the other three replies, and
    /// every command that returns one of those would otherwise pay for its
    /// size. `Box` is transparent to serde, so the JSON is unaffected.
    Snapshot(Box<Result<GuiSnapshot, String>>),
    VectorSource(Result<GuiVectorSource, String>),
    /// A path on the editor's host. Serializing it requires valid UTF-8, which
    /// every path Ovim opens already is because the buffer stores paths as
    /// `String`.
    Path(Result<PathBuf, String>),
}

impl GuiReply {
    /// The shape of this reply, for matching against [`GuiCommand::reply_kind`].
    pub fn kind(&self) -> GuiReplyKind {
        match self {
            GuiReply::Unit(_) => GuiReplyKind::Unit,
            GuiReply::Snapshot(_) => GuiReplyKind::Snapshot,
            GuiReply::VectorSource(_) => GuiReplyKind::VectorSource,
            GuiReply::Path(_) => GuiReplyKind::Path,
        }
    }
}

/// A JSON round trip, as a transport would perform it.
///
/// Value equality after the trip is the assertion that matters: a field the
/// wire format drops either fails to deserialize or comes back different.
#[cfg(test)]
pub(super) fn round_trip<T>(value: &T) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("GUI protocol values serialize as JSON");
    serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("GUI protocol value did not deserialize: {error}\n{json}"))
}

/// One value of every [`GuiCommand`] variant.
///
/// Every field carries a distinct, non-default value so that a round trip
/// which loses one is visible rather than accidentally correct.
#[cfg(test)]
pub(super) fn sample_commands() -> Vec<GuiCommand> {
    let plain_key = GuiKeyInput {
        key: "j".to_string(),
        shift: true,
        control: false,
        alt: true,
        meta: false,
    };
    let modified_key = GuiKeyInput {
        key: "Enter".to_string(),
        shift: false,
        control: true,
        alt: false,
        meta: true,
    };
    vec![
        GuiCommand::Snapshot {
            columns: 132,
            rows: 44,
        },
        GuiCommand::VectorSource,
        GuiCommand::VectorFeedback {
            feedback: "lighter stroke".to_string(),
        },
        GuiCommand::DiffWorkspace,
        GuiCommand::OpenDiffBuffer {
            title: "Diff · src/main.rs".to_string(),
            content: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
        },
        GuiCommand::Key {
            input: plain_key.clone(),
        },
        GuiCommand::OpenAiChat,
        GuiCommand::UpdateChatInput {
            expected_input: "before".to_string(),
            expected_cursor: 3,
            input: "after".to_string(),
            cursor: 5,
            action: Some(modified_key),
        },
        GuiCommand::SetChatInputCursor { offset: 7 },
        GuiCommand::SetChatInputWidth { columns: 96 },
        GuiCommand::RemoveChatImage { index: 2 },
        GuiCommand::SelectAiProfile {
            profile: "anthropic".to_string(),
        },
        GuiCommand::SelectReasoningEffort {
            effort: "high".to_string(),
        },
        GuiCommand::SelectChatMessage { index: 4 },
        GuiCommand::ManageQueuedChatInput {
            id: 11,
            action: "cancel".to_string(),
        },
        GuiCommand::AiPolicy {
            action: "yolo".to_string(),
        },
        GuiCommand::EditorCommand {
            command: "set number".to_string(),
        },
        GuiCommand::SelectChatAgent {
            agent_id: Some("agent-9".to_string()),
        },
        GuiCommand::Paste {
            text: "pasted text".to_string(),
        },
        GuiCommand::AttachImages {
            paths: vec![PathBuf::from("diagram.png"), PathBuf::from("photo.jpeg")],
        },
        GuiCommand::AttachImageData {
            name: "pasted-image.png".to_string(),
            data: vec![137, 80, 78, 71],
        },
        GuiCommand::SetCursor {
            pane: 1,
            line: 42,
            display_column: 8,
        },
        GuiCommand::SelectTab { index: 3 },
        GuiCommand::FocusPane { index: 2 },
        GuiCommand::SelectPicker { index: 6 },
        GuiCommand::SelectCompletion {
            index: 1,
            activate: true,
        },
        GuiCommand::SelectFileTree {
            index: 12,
            activate: false,
        },
        GuiCommand::SelectProblem {
            kind: "diagnostics".to_string(),
            index: 5,
            activate: true,
        },
        GuiCommand::SelectLsp {
            index: 9,
            activate: false,
        },
        GuiCommand::SelectDebugFrame { index: 2 },
        GuiCommand::Shutdown,
        GuiCommand::Key { input: plain_key },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::mem::discriminant;

    #[test]
    fn the_command_sample_covers_every_variant_exactly_once() {
        // `GuiRequest` has 31 variants and `GuiCommand` mirrors it one for
        // one, so this count is what stops a new variant from being added
        // without round-trip coverage. The trailing duplicate in the sample is
        // a second `Key` with different modifiers, which is deliberate.
        let commands = sample_commands();
        let distinct: HashSet<_> = commands.iter().map(discriminant).collect();
        assert_eq!(distinct.len(), 31);
    }

    #[test]
    fn every_command_variant_survives_a_json_round_trip() {
        for command in sample_commands() {
            assert_eq!(round_trip(&command), command);
        }
    }

    #[test]
    fn a_command_is_tagged_by_name_with_its_payload_beside_it() {
        // Adjacent tagging is load-bearing: `EditorCommand` owns a field
        // called `command`, which an internal tag of the same name would
        // shadow, and it is the reason the payload sits under its own key.
        let json = serde_json::to_value(GuiCommand::EditorCommand {
            command: "set number".to_string(),
        })
        .unwrap();

        assert_eq!(json["command"], "editorCommand");
        assert_eq!(json["payload"]["command"], "set number");
    }

    #[test]
    fn a_command_payload_uses_camel_case_field_names() {
        let json = serde_json::to_value(GuiCommand::SetCursor {
            pane: 1,
            line: 42,
            display_column: 8,
        })
        .unwrap();

        assert_eq!(json["payload"]["displayColumn"], 8);
    }

    #[test]
    fn every_reply_variant_survives_a_json_round_trip() {
        for reply in [
            GuiReply::Unit(Ok(())),
            GuiReply::Unit(Err("no chat is open".to_string())),
            GuiReply::VectorSource(Ok(GuiVectorSource {
                source: "documentsize 24x24\n".to_string(),
                file_name: "close.strok".to_string(),
            })),
            GuiReply::VectorSource(Err("not a vector buffer".to_string())),
            GuiReply::Path(Ok(PathBuf::from("workspace/project"))),
            GuiReply::Path(Err("no workspace".to_string())),
            GuiReply::Snapshot(Box::new(Err("the editor stopped".to_string()))),
        ] {
            assert_eq!(round_trip(&reply), reply);
        }
    }

    #[test]
    fn a_reply_reports_the_shape_its_command_asked_for() {
        for command in sample_commands() {
            let kind = command.reply_kind();
            let reply = match kind {
                GuiReplyKind::None => continue,
                GuiReplyKind::Unit => GuiReply::Unit(Ok(())),
                GuiReplyKind::Snapshot => GuiReply::Snapshot(Box::new(Err("unused".to_string()))),
                GuiReplyKind::VectorSource => GuiReply::VectorSource(Err("unused".to_string())),
                GuiReplyKind::Path => GuiReply::Path(Err("unused".to_string())),
            };
            assert_eq!(reply.kind(), kind, "{command:?}");
        }
    }

    #[test]
    fn internally_tagged_panels_round_trip_through_their_own_tag_field() {
        // These two enums are the only snapshot types that carry an internal
        // tag, and internal tagging is the serde form most likely to break
        // when `Deserialize` is added, so they are checked directly rather
        // than only through whatever a live editor happens to produce.
        for page in [
            GuiCodeExplanationPage::Concept {
                title: "Ropes".to_string(),
                body: "Text is stored as a rope.".to_string(),
            },
            GuiCodeExplanationPage::Code {
                path: "ovim-core/src/buffer/mod.rs".to_string(),
                start_line: 10,
                end_line: 24,
                comment: "The rope lives here.".to_string(),
            },
        ] {
            assert_eq!(round_trip(&page), page);
        }

        for discussion in [
            GuiCodeExplanationDiscussion::Navigating {
                question_count: 2,
                latest_question: Some("Why a rope?".to_string()),
                latest_answer: Some("Cheap edits.".to_string()),
                latest_failed: true,
            },
            GuiCodeExplanationDiscussion::Composing {
                input: "How does undo work".to_string(),
                cursor: 4,
                question_count: 3,
            },
            GuiCodeExplanationDiscussion::Answering {
                question: "How does undo work?".to_string(),
                answer: "It snapshots the rope.".to_string(),
                question_count: 3,
            },
        ] {
            assert_eq!(round_trip(&discussion), discussion);
        }
    }

    #[test]
    fn a_key_input_keeps_modifiers_that_default_to_false_when_absent() {
        // The modifier fields carry `#[serde(default)]`, so dropping one from
        // the wire format would silently deserialize as `false` instead of
        // failing. Asserting on a value with mixed modifiers is what makes
        // that visible.
        let input = GuiKeyInput {
            key: "k".to_string(),
            shift: true,
            control: false,
            alt: true,
            meta: false,
        };

        assert_eq!(round_trip(&input), input);
        assert!(serde_json::to_string(&input)
            .unwrap()
            .contains("\"alt\":true"));
    }
}
