use crate::ai::chat_types::{
    ChatFocus, ChatOpts, ImageAttachment, NodeId, StreamChunk, ToolCallInfo, ToolSummaryKind,
};
use crate::buffer::BufferId;
use crate::mode::Mode;
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Boundary at which an agent must verify that the user understands the
/// current repository state. This policy is independent from YOLO approvals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComprehensionPolicy {
    #[default]
    Off,
    /// Require a checkpoint before externally publishing work.
    Publish,
    /// Require a checkpoint before local commits as well as publication.
    Commit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatModelPickerSection {
    #[default]
    Model,
    Effort,
}

#[derive(Clone, Debug)]
pub(crate) enum PendingRuntimeTermination {
    Completed,
    Failed(String),
    Interrupted(String),
}

impl ComprehensionPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Publish => "publish",
            Self::Commit => "commit",
        }
    }
}

/// A mastery checkpoint is valid only while the repository fingerprint still
/// matches. The summary is retained for UI/headless clients and future audit.
#[derive(Clone, Debug)]
pub struct ComprehensionCheckpoint {
    pub repository_fingerprint: String,
    pub summary: String,
    pub critical_concepts: Vec<String>,
}

/// A paused tool call that requires explicit user approval to access
/// paths outside the active project boundary.
pub struct PendingToolApproval {
    pub tool_call: ToolCallInfo,
    /// The policy or Terra explanation that caused this escalation.
    pub reason: String,
    pub runtime_tool: Option<crate::agent_runtime::PendingToolRef>,
    /// Whether ToolStarted was already recorded before policy paused it.
    pub runtime_tool_started: bool,
    pub remaining_tool_calls: Vec<ToolCallInfo>,
    pub model_name: String,
    pub requested_path: PathBuf,
    pub approval_root: PathBuf,
    /// Present for an app-server dynamic tool. Keeping this sender alive is
    /// what genuinely pauses Codex while the approval UI is visible.
    pub dynamic_response: Option<tokio::sync::oneshot::Sender<Result<String, String>>>,
    pub dynamic_turn: Option<crate::agent_runtime::PendingTurnRef>,
}

impl AiTurnBlocker {
    fn activity(self) -> AiChatActivity {
        match self {
            Self::ToolApproval => AiChatActivity::WaitingToolApproval,
            Self::AutoModeClassification => AiChatActivity::ClassifyingTool,
            Self::ShellExecution => AiChatActivity::RunningShell,
            Self::WebExecution => AiChatActivity::RunningWeb,
            Self::CodeExplanation => AiChatActivity::WaitingCodeExplanation,
        }
    }
}

/// Mutually exclusive reason an active provider turn is blocked or delegated.
///
/// `pending_job` is intentionally not part of this enum: app-server inference
/// remains allocated while a dynamic tool waits for one of these interactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AiTurnBlocker {
    ToolApproval,
    AutoModeClassification,
    ShellExecution,
    WebExecution,
    CodeExplanation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentComposerActionKind {
    Message,
    Followup,
}

pub struct PendingAgentComposerAction {
    pub kind: AgentComposerActionKind,
    pub agent_id: String,
    pub previous_input: String,
    pub previous_cursor: usize,
}

/// A Terra auto-mode verdict in flight for a Codex dynamic bash request.
pub struct PendingAutoModeClassification {
    pub tool_call: ToolCallInfo,
    pub runtime_tool: crate::agent_runtime::PendingToolRef,
    pub runtime_turn: crate::agent_runtime::PendingTurnRef,
    pub dynamic_response: tokio::sync::oneshot::Sender<Result<String, String>>,
    pub receiver:
        tokio::sync::oneshot::Receiver<Result<crate::ai::auto_mode::ClassifierVerdict, String>>,
}

/// How an authorized shell effect resumes once its background task finishes.
pub enum ShellExecutionContinuation {
    /// A provider-owned dynamic tool call waiting on its response channel.
    Dynamic {
        runtime_tool: crate::agent_runtime::PendingToolRef,
        runtime_turn: crate::agent_runtime::PendingTurnRef,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// A completed provider response whose local tool batch is being drained.
    Batch {
        runtime_tool: Option<crate::agent_runtime::PendingToolRef>,
        runtime_turn: Option<crate::agent_runtime::PendingTurnRef>,
        remaining_tool_calls: Vec<ToolCallInfo>,
        model_name: String,
    },
}

/// An authorized shell effect running off the editor/event-loop thread.
pub struct PendingShellExecution {
    pub tool_call: ToolCallInfo,
    pub continuation: ShellExecutionContinuation,
    pub receiver: tokio::sync::oneshot::Receiver<ShellExecutionObservation>,
    pub progress: tokio::sync::mpsc::UnboundedReceiver<ShellProgressEvent>,
    pub task: tokio::task::JoinHandle<()>,
    /// Kills the spawned command itself: aborting a started `spawn_blocking`
    /// task never stops the closure, so cancellation must reach the child.
    pub kill: std::sync::Arc<ShellKillHandle>,
}

/// Cross-thread cancellation handle for a running shell effect.
///
/// The blocking shell task publishes the spawned child's process id, and the
/// editor thread requests a kill on cancel. Publication is checked against
/// the cancelled flag so a cancel that lands before the child spawns still
/// prevents the command from running (or reaps it immediately).
#[derive(Default)]
pub struct ShellKillHandle {
    state: std::sync::Mutex<ShellKillState>,
}

#[derive(Default)]
struct ShellKillState {
    /// Invariant: a pid is published here only while the shell leader is
    /// un-reaped (running or zombie). The kernel reserves an un-reaped
    /// leader's pid — and therefore its pgid — so `killpg` on a published
    /// pid can never signal an unrelated reused process group, even after
    /// every living group member has exited. The pid is forgotten in the
    /// same critical section that reaps the leader (`reap_locked`).
    child_pid: Option<u32>,
    interrupt_requested: bool,
    cancelled: bool,
}

impl ShellKillHandle {
    /// Record the freshly spawned child. Returns `false` when cancellation
    /// already happened, in which case the caller must kill the child itself.
    pub fn publish_child(&self, pid: u32) -> bool {
        let mut state = self.state.lock().expect("shell kill handle poisoned");
        state.child_pid = Some(pid);
        !state.cancelled && !state.interrupt_requested
    }

    /// Attempt one non-blocking reap of the published child under the
    /// handle lock.
    ///
    /// Reaping releases the leader's pid (and, once the group empties, the
    /// pgid) for reuse, so it must happen atomically with forgetting the
    /// pid: a concurrent `cancel` either runs before the reap and `killpg`s
    /// a pgid the un-reaped leader still reserves, or runs after and finds
    /// no pid at all — it can never signal a reused id. `Ok(None)` means
    /// the child is still running. On `Err` the child's state is unknown,
    /// so the pid is forgotten as well rather than risking a later signal
    /// on stale information.
    pub fn reap_locked(
        &self,
        try_reap: impl FnOnce() -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut state = self.state.lock().expect("shell kill handle poisoned");
        let result = try_reap();
        if !matches!(result, Ok(None)) {
            state.child_pid = None;
        }
        result
    }

    /// True once the execution was cancelled; checked before spawning.
    pub fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .expect("shell kill handle poisoned")
            .cancelled
    }

    pub fn is_interrupted(&self) -> bool {
        let state = self.state.lock().expect("shell kill handle poisoned");
        state.interrupt_requested || state.cancelled
    }

    /// The pid last published by the blocking task, if the child is alive.
    pub fn published_child(&self) -> Option<u32> {
        self.state
            .lock()
            .expect("shell kill handle poisoned")
            .child_pid
    }

    /// Mark the execution cancelled and kill the published child, if any.
    /// The signal is sent while holding the handle lock so it cannot race
    /// the reap in `reap_locked`: a published pid is guaranteed to still
    /// name our un-reaped group leader.
    pub fn cancel(&self) {
        let mut state = self.state.lock().expect("shell kill handle poisoned");
        state.cancelled = true;
        if let Some(pid) = state.child_pid.take() {
            kill_shell_process_group(pid);
        }
    }

    /// Request the process group's normal terminal interrupt without taking
    /// ownership away from the final reap or a later force-kill escalation.
    pub fn interrupt(&self) {
        let mut state = self.state.lock().expect("shell kill handle poisoned");
        state.interrupt_requested = true;
        if let Some(pid) = state.child_pid {
            interrupt_shell_process_group(pid);
        }
    }
}

#[cfg(unix)]
fn interrupt_shell_process_group(pid: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGINT);
}

#[cfg(not(unix))]
fn interrupt_shell_process_group(_pid: u32) {}

/// SIGKILL the process group led by `pid`. The shell child is spawned as a
/// group leader, so this stops the command and everything it forked.
///
/// Callers must guarantee `pid` names a leader that has NOT been reaped yet
/// (see `ShellKillState::child_pid`): the kernel then reserves the pid and
/// pgid, so `killpg` either hits our own group or fails with ESRCH
/// harmlessly once every living member has exited. There is deliberately no
/// plain `kill(pid)` fallback — that call has no such protection and could
/// signal an unrelated process if the pid were ever stale.
pub fn kill_shell_process_group(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;
        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

pub struct ShellExecutionObservation {
    pub result: crate::ai::tools::ToolResult,
    pub delta: Option<crate::run_log::WorkspaceDelta>,
    pub capture_error: Option<String>,
    /// The command may have run, but its resulting disk state was not captured.
    pub outcome_unknown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub enum ShellProgressEvent {
    Spawned {
        pid: u32,
    },
    Output {
        stream: ShellOutputStream,
        bytes: Vec<u8>,
    },
    CapturingChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellTranscriptPhase {
    Preparing,
    Running,
    InterruptRequested,
    CapturingChanges,
    Succeeded,
    Failed,
    Interrupted,
    OutcomeUnknown,
    Archived,
}

#[derive(Debug, Clone)]
pub struct ShellTranscriptChunk {
    pub stream: ShellOutputStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ShellTranscript {
    pub tool_call: ToolCallInfo,
    pub command: String,
    pub workdir: PathBuf,
    pub phase: ShellTranscriptPhase,
    pub pid: Option<u32>,
    pub started_at: std::time::Instant,
    pub last_output_at: Option<std::time::Instant>,
    pub completed_at: Option<std::time::Instant>,
    pub chunks: VecDeque<ShellTranscriptChunk>,
    pub retained_bytes: usize,
    pub dropped_bytes: usize,
    pub expired: bool,
}

impl ShellTranscript {
    pub const MAX_RETAINED_BYTES: usize = 512 * 1024;

    pub fn new(tool_call: ToolCallInfo, command: String, workdir: PathBuf) -> Self {
        Self {
            tool_call,
            command,
            workdir,
            phase: ShellTranscriptPhase::Preparing,
            pid: None,
            started_at: std::time::Instant::now(),
            last_output_at: None,
            completed_at: None,
            chunks: VecDeque::new(),
            retained_bytes: 0,
            dropped_bytes: 0,
            expired: false,
        }
    }

    pub fn append(&mut self, stream: ShellOutputStream, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.last_output_at = Some(std::time::Instant::now());
        self.retained_bytes = self.retained_bytes.saturating_add(bytes.len());
        self.chunks
            .push_back(ShellTranscriptChunk { stream, bytes });
        while self.retained_bytes > Self::MAX_RETAINED_BYTES {
            let Some(chunk) = self.chunks.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(chunk.bytes.len());
            self.dropped_bytes = self.dropped_bytes.saturating_add(chunk.bytes.len());
        }
    }

    pub fn finish(&mut self, phase: ShellTranscriptPhase) {
        self.phase = phase;
        self.completed_at = Some(std::time::Instant::now());
    }

    pub fn expire_output(&mut self) {
        self.dropped_bytes = self.dropped_bytes.saturating_add(self.retained_bytes);
        self.retained_bytes = 0;
        self.chunks.clear();
        self.expired = true;
    }
}

/// An Exa request running off the editor/event-loop thread.
pub struct PendingWebExecution {
    pub tool_call: ToolCallInfo,
    pub runtime_tool: Option<crate::agent_runtime::PendingToolRef>,
    pub runtime_turn: Option<crate::agent_runtime::PendingTurnRef>,
    pub remaining_tool_calls: Vec<ToolCallInfo>,
    pub model_name: String,
    pub receiver: tokio::sync::oneshot::Receiver<crate::ai::exa::WebToolOutcome>,
    pub task: tokio::task::JoinHandle<()>,
}

pub enum SubagentControlContinuation {
    Dynamic {
        runtime_tool: crate::agent_runtime::PendingToolRef,
        runtime_turn: crate::agent_runtime::PendingTurnRef,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    Batch {
        runtime_tool: Option<crate::agent_runtime::PendingToolRef>,
        runtime_turn: Option<crate::agent_runtime::PendingTurnRef>,
        remaining_tool_calls: Vec<ToolCallInfo>,
        model_name: String,
    },
}

/// A mailbox wait or hierarchy interruption parked away from the editor loop.
pub struct PendingSubagentControl {
    pub tool_call: ToolCallInfo,
    pub continuation: SubagentControlContinuation,
    pub receiver: tokio::sync::oneshot::Receiver<crate::ai::tools::ToolResult>,
    pub task: tokio::task::JoinHandle<()>,
}

pub enum CodeExplanationContinuation {
    Batch {
        runtime_tool: Option<crate::agent_runtime::PendingToolRef>,
        runtime_turn: Option<crate::agent_runtime::PendingTurnRef>,
        remaining_tool_calls: Vec<ToolCallInfo>,
        model_name: String,
    },
    Dynamic {
        runtime_tool: crate::agent_runtime::PendingToolRef,
        runtime_turn: crate::agent_runtime::PendingTurnRef,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// A user-triggered replay of an already completed history item. It is
    /// entirely local and must not write another tool result or resume inference.
    Replay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeExplanationInteraction {
    Navigating,
    Composing { input: String, cursor: usize },
    Answering { step: usize, exchange: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeExplanationExchange {
    pub question: String,
    pub answer: String,
    pub failed: bool,
}

#[derive(Debug, Clone)]
pub struct CachedCodeExplanation {
    pub tool_call_id: String,
    pub steps: Vec<super::code_explanation::CodeExplanationStep>,
    pub bytes: usize,
}

pub struct PendingCodeExplanation {
    pub tool_call: ToolCallInfo,
    pub steps: Vec<super::code_explanation::CodeExplanationStep>,
    pub current: usize,
    /// First wrapped answer row shown in the walkthrough reply viewport.
    pub answer_scroll: usize,
    /// Per-step discussion projected into the walkthrough card. The same
    /// questions and answers are also committed to the main conversation.
    pub threads: Vec<Vec<CodeExplanationExchange>>,
    pub interaction: CodeExplanationInteraction,
    /// Tool navigation must not silently retarget later agent mutations.
    pub original_active_buffer_id: BufferId,
    /// Ephemeral read-only buffer rendering the current invocation-time snapshot.
    pub presentation_buffer_id: Option<BufferId>,
    /// Present only while the original explain_with_codebase call is blocked.
    /// A question consumes this continuation but leaves the walkthrough open.
    pub continuation: Option<CodeExplanationContinuation>,
}

#[derive(Debug, Clone)]
pub struct ExaSetupDialog {
    pub input: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub environment_override: bool,
}

#[derive(Debug, Clone)]
pub struct ToolEventSummary {
    pub kind: ToolSummaryKind,
    pub label: String,
    pub call: ToolCallInfo,
}

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub content: String,
}

/// Viewport state for chat history rendering.
pub struct ChatViewportState {
    /// Row scroll offset from bottom (0 = latest).
    pub row_scroll_from_bottom: usize,
    /// Whether viewport should track latest output automatically.
    pub follow_latest: bool,
    /// Total rendered row count when pinning started.
    pub pinned_base_total_rows: Option<usize>,
}

impl Default for ChatViewportState {
    fn default() -> Self {
        Self {
            row_scroll_from_bottom: 0,
            follow_latest: true,
            pinned_base_total_rows: None,
        }
    }
}

/// Selection state for message-history interactions.
#[derive(Default)]
pub struct ChatHistoryState {
    /// Selected node in the active branch, if any.
    ///
    /// Using node identity keeps selection stable when new messages append.
    pub selected_node_id: Option<NodeId>,
    /// Selected scheduled input, addressed by stable identity because steers
    /// may disappear asynchronously when the provider accepts them.
    pub selected_queued_id: Option<u64>,
    /// Selected live shell row, which is presentation-only until its tool
    /// result becomes a durable conversation message.
    pub selected_shell_tool_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ShellInspectorState {
    pub tool_call_id: String,
    pub row_scroll_from_bottom: usize,
    pub follow_latest: bool,
    pub search_query: Option<String>,
    pub search_input: Option<String>,
    pub search_match_line: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedChatInputKind {
    /// Join the active round at its next completed tool boundary.
    Steer,
    /// Start a new round after the active round completes.
    FollowUp,
    /// Execute an editor-owned slash command after the active round.
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedChatInput {
    pub id: u64,
    pub kind: QueuedChatInputKind,
    pub content: String,
    pub images: Vec<ImageAttachment>,
    pub code_attachment: Option<CodeAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAttachment {
    pub buffer_id: BufferId,
    pub path: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub buffer_revision: usize,
    pub text: String,
}

impl CodeAttachment {
    pub fn label(&self) -> String {
        let path = self.path.as_deref().unwrap_or("untitled");
        if self.start_line == self.end_line {
            format!("{path}:{}", self.start_line + 1)
        } else {
            format!("{path}:{}–{}", self.start_line + 1, self.end_line + 1)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatViewMode {
    #[default]
    DockedChat,
    ReviewFocused,
}

/// Authoritative projection of the work currently owned by an AI chat.
///
/// Provider jobs may remain allocated while an app-server tool is paused, so
/// consumers must not infer lifecycle from `pending_job` or `waiting` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiChatActivity {
    Idle,
    Inference,
    ClassifyingTool,
    RunningShell,
    RunningWeb,
    WaitingToolApproval,
    WaitingFolderApproval,
    WaitingCodeExplanation,
}

impl AiChatActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Inference => "inference",
            Self::ClassifyingTool => "classifying_tool",
            Self::RunningShell => "running_shell",
            Self::RunningWeb => "running_web",
            Self::WaitingToolApproval => "waiting_tool_approval",
            Self::WaitingFolderApproval => "waiting_folder_approval",
            Self::WaitingCodeExplanation => "waiting_code_explanation",
        }
    }

    pub fn has_pending_work(self) -> bool {
        self != Self::Idle
    }
}

pub struct AiChatState {
    pub opts: ChatOpts,
    /// Buffer ID where the chat was originally opened.
    /// Used for the conversation key — never changes during the session.
    pub origin_buffer_id: BufferId,
    /// Stable runtime branch locator for the active conversation trajectory.
    pub runtime_branch: crate::agent_runtime::BranchLocator,
    /// Active ovim turn. Provider continuations and dynamic tools share it.
    pub runtime_turn: Option<Box<crate::agent_runtime::PendingTurnRef>>,
    /// Terminal transition that could not be persisted. Retained so the next
    /// submission can retry it before starting a new turn.
    pub(crate) pending_runtime_termination: Option<PendingRuntimeTermination>,
    /// Buffer ID that mutations target. Updated by `open_file`.
    pub active_buffer_id: BufferId,
    /// Chat input text.
    pub input: String,
    /// Byte offset cursor in input.
    pub input_cursor: usize,
    /// Selected entry in the derived slash-command completion popup.
    pub slash_completion_selected: usize,
    /// Images dropped into the current composer.
    pub pending_images: Vec<ImageAttachment>,
    /// Visually selected code attached to the current composer.
    pub pending_code_attachment: Option<CodeAttachment>,
    /// Image currently expanded over the chat panel.
    pub image_modal: Option<PathBuf>,
    /// First-run/recovery dialog for Ovim-owned Exa web search.
    pub exa_setup: Option<ExaSetupDialog>,
    /// User inputs submitted while an agent round is active.
    pub queued_inputs: VecDeque<QueuedChatInput>,
    pub next_queued_input_id: u64,
    /// Which zone has focus.
    pub focus: ChatFocus,
    /// Active half of the combined model and reasoning-effort picker.
    pub model_picker_section: ChatModelPickerSection,
    /// Per-chat reasoning effort. `None` inherits the selected profile.
    pub reasoning_effort_override: Option<String>,
    /// Viewport behavior for chat history.
    pub viewport: ChatViewportState,
    /// Incremented whenever `/clear` starts a fresh provider context.
    pub context_generation: u64,
    /// Latest successful model-context checkpoint. The durable conversation
    /// remains intact; request assembly applies this only to its active branch.
    pub(crate) compaction_checkpoint: Option<super::ai_compaction::CompactionCheckpoint>,
    /// Message-history selection state.
    pub history: ChatHistoryState,
    /// Whether assistant can suggest edits.
    pub allow_edits: bool,
    /// Per-chat opt-in that executes tool requests without Terra or approval
    /// prompts. Hard path/integrity validation remains enforced.
    pub yolo_mode: bool,
    /// Per-chat comprehension boundary. Unlike YOLO, this is a workflow gate,
    /// not a tool-approval preference.
    pub comprehension_policy: ComprehensionPolicy,
    pub comprehension_checkpoint: Option<ComprehensionCheckpoint>,
    /// Waiting for AI response.
    pub waiting: bool,
    /// Pending async chat job.
    pub pending_job: Option<PendingAiChatJob>,
    /// Scratch buffer state for <C-g> editing.
    pub scratch: Option<ScratchBufferState>,
    /// Mode the editor was in before opening chat.
    pub mode_before_chat: Mode,
    /// Accumulated streaming content (committed on Done).
    pub streaming_content: Option<String>,
    /// Accumulated streaming thinking (committed on Done).
    pub streaming_thinking: Option<String>,
    /// Byte offsets already emitted to the normalized run log. The UI keeps
    /// one accumulated message while provenance can preserve tool chronology.
    pub runtime_recorded_content_bytes: usize,
    pub runtime_recorded_thinking_bytes: usize,
    pub runtime_last_content_event: Option<crate::run_log::EventId>,
    pub runtime_last_reasoning_event: Option<crate::run_log::EventId>,
    /// Node IDs of thinking messages that are expanded in the UI.
    pub expanded_thinking: HashSet<NodeId>,
    /// Tool call IDs whose arguments and results are expanded in the UI.
    pub expanded_tool_events: HashSet<String>,
    /// Whether the tree panel sidebar is open.
    pub tree_panel_open: bool,
    /// Cursor index into the flattened DFS tree list.
    pub tree_panel_cursor: usize,
    /// Whether keyboard navigation in the shared sidebar targets delegated
    /// agents (`true`) or conversation branches (`false`).
    pub agent_tree_focused: bool,
    /// Cursor into `AgentControlPlaneSnapshot::hierarchy`.
    pub agent_tree_cursor: usize,
    /// Durable event watermark last reflected on screen. When the run advances
    /// past this the chat is repainted so agent cards update live even while the
    /// parent turn is idle. Zero means nothing observed yet.
    pub last_observed_agent_sequence: u64,
    /// Stable selected/followed identities. These are presentation state only;
    /// lifecycle and control data always comes from the durable projection.
    pub selected_agent_id: Option<String>,
    pub followed_agent_id: Option<String>,
    /// A targeted child message/follow-up temporarily owns the existing chat
    /// composer. The prior root draft is restored after submit or cancel.
    pub pending_agent_composer_action: Option<PendingAgentComposerAction>,
    /// Tool calls accumulated during streaming.
    pub streaming_tool_calls: Vec<ToolCallInfo>,
    /// Opaque inference-strategy items accumulated for the assistant message.
    pub streaming_provider_state: Vec<serde_json::Value>,
    /// Number of individual tool calls executed in current turn.
    pub tool_call_count: u64,
    /// Paused tool call awaiting user approval for outside-project access.
    pub pending_tool_approval: Option<PendingToolApproval>,
    pub pending_auto_mode_classification: Option<PendingAutoModeClassification>,
    pub pending_shell_execution: Option<PendingShellExecution>,
    /// Bounded live/completed shell output keyed by tool-call id.
    pub shell_transcripts: HashMap<String, ShellTranscript>,
    /// Oldest completed shell transcript first; running transcripts are never
    /// placed here or evicted.
    pub shell_transcript_lru: VecDeque<String>,
    /// Process Inspector overlay, if one shell transcript is being viewed.
    pub shell_inspector: Option<ShellInspectorState>,
    pub pending_web_execution: Option<PendingWebExecution>,
    pub pending_subagent_control: Option<PendingSubagentControl>,
    /// Interactive code walkthrough currently blocking the invoking tool.
    pub pending_code_explanation: Option<PendingCodeExplanation>,
    /// Oldest retained completed walkthrough first. Entries hold only files
    /// below the large-file cutoff and share source text across repeated pages.
    pub code_explanation_cache: VecDeque<CachedCodeExplanation>,
    pub code_explanation_cache_bytes: usize,
    /// First-chat-open prompt when session starts outside a git repo.
    pub pending_no_repo_folder_approval: Option<PathBuf>,
    /// Session-scoped roots explicitly approved for path-restricted tool access
    /// (outside-project and sensitive-path overrides).
    pub approved_external_roots: Vec<PathBuf>,
    /// Compact tool event summaries keyed by tool call id.
    pub tool_event_summaries: HashMap<String, ToolEventSummary>,
    /// Images loaded by a tool and waiting to be attached to its tool result.
    pub tool_result_images: HashMap<String, Vec<ImageAttachment>>,
    /// File snapshots captured by snapshot_file tool, keyed by snapshot id.
    pub file_snapshots: HashMap<String, FileSnapshot>,
    /// Monotonic counter for snapshot ids.
    pub next_snapshot_id: u64,
    /// Tracks which lines each agent turn modified, per buffer.
    pub agent_edits: AgentEditTracker,
    /// Whether the buffer was clean when chat opened (for auto-save guard).
    pub buffer_was_clean_at_chat_start: bool,
    /// Most recent save outcome from an agent mutation.
    pub last_save_outcome: Option<String>,
    /// Chat panel view mode.
    pub view_mode: ChatViewMode,
    /// User-selected width of the docked chat as a percentage of the shared
    /// buffer/chat area. `None` keeps the context-sensitive default.
    pub panel_width_percent: Option<u16>,
    /// Current undo group ID for grouping agent edits per turn.
    pub current_undo_group: Option<u64>,
    /// Next undo group ID to assign.
    pub next_undo_group_id: u64,
}

impl AiChatState {
    const MAX_COMPLETED_SHELL_TRANSCRIPTS: usize = 10;
    const MAX_COMPLETED_SHELL_BYTES: usize = 2 * 1024 * 1024;

    /// Retire a shell transcript at a terminal boundary: stamp its final phase,
    /// move it to the tail of the completed-transcript LRU, and re-run eviction.
    ///
    /// Every path that stops a running shell process must go through here. A
    /// path that only cancels the kill token and aborts the task leaves the
    /// transcript in `Running` forever — the chat keeps rendering it as a live
    /// process — and, because it never enters the LRU, its retained output can
    /// never be reclaimed by `evict_old_shell_transcripts`, which only walks the
    /// LRU. That was the shape of the leak on the new-turn path.
    pub fn retire_shell_transcript(&mut self, tool_call_id: &str, phase: ShellTranscriptPhase) {
        let Some(transcript) = self.shell_transcripts.get_mut(tool_call_id) else {
            return;
        };
        transcript.finish(phase);
        self.shell_transcript_lru.retain(|id| id != tool_call_id);
        self.shell_transcript_lru
            .push_back(tool_call_id.to_string());
        self.evict_old_shell_transcripts();
    }

    pub fn evict_old_shell_transcripts(&mut self) {
        loop {
            let completed_bytes: usize = self
                .shell_transcript_lru
                .iter()
                .filter_map(|id| self.shell_transcripts.get(id))
                .map(|transcript| transcript.retained_bytes)
                .sum();
            if self.shell_transcript_lru.len() <= Self::MAX_COMPLETED_SHELL_TRANSCRIPTS
                && completed_bytes <= Self::MAX_COMPLETED_SHELL_BYTES
            {
                break;
            }
            let Some(id) = self.shell_transcript_lru.pop_front() else {
                break;
            };
            if let Some(transcript) = self.shell_transcripts.get_mut(&id) {
                transcript.expire_output();
            }
        }
    }

    pub(crate) fn turn_blocker(&self) -> Option<AiTurnBlocker> {
        let blockers = [
            self.pending_tool_approval
                .as_ref()
                .map(|_| AiTurnBlocker::ToolApproval),
            self.pending_auto_mode_classification
                .as_ref()
                .map(|_| AiTurnBlocker::AutoModeClassification),
            self.pending_shell_execution
                .as_ref()
                .map(|_| AiTurnBlocker::ShellExecution),
            self.pending_web_execution
                .as_ref()
                .map(|_| AiTurnBlocker::WebExecution),
            self.pending_code_explanation
                .as_ref()
                .and_then(|pending| pending.continuation.as_ref())
                .map(|_| AiTurnBlocker::CodeExplanation),
        ];
        debug_assert!(
            blockers.iter().flatten().count() <= 1,
            "an AI turn cannot have multiple blockers"
        );
        blockers.into_iter().flatten().next()
    }

    pub fn activity(&self) -> AiChatActivity {
        // Blocking user decisions take precedence over the provider job that
        // may be intentionally retained behind them.
        let blocker = self.turn_blocker();
        if blocker == Some(AiTurnBlocker::ToolApproval) {
            AiChatActivity::WaitingToolApproval
        } else if self.pending_no_repo_folder_approval.is_some() {
            AiChatActivity::WaitingFolderApproval
        } else if let Some(blocker) = blocker {
            blocker.activity()
        } else if self.pending_job.is_some() || self.runtime_turn.is_some() || self.waiting {
            AiChatActivity::Inference
        } else if self.pending_code_explanation.is_some() {
            // A question can complete the provider turn that originally owned
            // the walkthrough. The walkthrough itself still needs user input,
            // so headless clients and hidden-chat status must not report idle.
            AiChatActivity::WaitingCodeExplanation
        } else {
            AiChatActivity::Idle
        }
    }

    /// Number of lines in the input text (at least 1).
    pub fn input_line_count(&self) -> usize {
        self.input.lines().count().max(1)
    }

    pub fn new(opts: ChatOpts, active_buffer_id: BufferId, mode_before: Mode) -> Self {
        let allow_edits = opts.allow_edits;
        Self {
            opts,
            origin_buffer_id: active_buffer_id,
            runtime_branch: crate::agent_runtime::BranchLocator("branch-0".into()),
            runtime_turn: None,
            pending_runtime_termination: None,
            active_buffer_id,
            input: String::new(),
            input_cursor: 0,
            slash_completion_selected: 0,
            pending_images: Vec::new(),
            pending_code_attachment: None,
            image_modal: None,
            exa_setup: None,
            queued_inputs: VecDeque::new(),
            next_queued_input_id: 1,
            focus: ChatFocus::TextInput,
            model_picker_section: ChatModelPickerSection::Model,
            reasoning_effort_override: None,
            viewport: ChatViewportState::default(),
            context_generation: 0,
            compaction_checkpoint: None,
            history: ChatHistoryState::default(),
            allow_edits,
            yolo_mode: false,
            comprehension_policy: ComprehensionPolicy::Off,
            comprehension_checkpoint: None,
            waiting: false,
            pending_job: None,
            scratch: None,
            mode_before_chat: mode_before,
            streaming_content: None,
            streaming_thinking: None,
            runtime_recorded_content_bytes: 0,
            runtime_recorded_thinking_bytes: 0,
            runtime_last_content_event: None,
            runtime_last_reasoning_event: None,
            expanded_thinking: HashSet::new(),
            expanded_tool_events: HashSet::new(),
            tree_panel_open: false,
            tree_panel_cursor: 0,
            agent_tree_focused: true,
            agent_tree_cursor: 0,
            last_observed_agent_sequence: 0,
            selected_agent_id: None,
            followed_agent_id: None,
            pending_agent_composer_action: None,
            streaming_tool_calls: Vec::new(),
            streaming_provider_state: Vec::new(),
            tool_call_count: 0,
            pending_tool_approval: None,
            pending_auto_mode_classification: None,
            pending_shell_execution: None,
            shell_transcripts: HashMap::new(),
            shell_transcript_lru: VecDeque::new(),
            shell_inspector: None,
            pending_web_execution: None,
            pending_subagent_control: None,
            pending_code_explanation: None,
            code_explanation_cache: VecDeque::new(),
            code_explanation_cache_bytes: 0,
            pending_no_repo_folder_approval: None,
            approved_external_roots: Vec::new(),
            tool_event_summaries: HashMap::new(),
            tool_result_images: HashMap::new(),
            file_snapshots: HashMap::new(),
            next_snapshot_id: 0,
            agent_edits: AgentEditTracker::new(),
            buffer_was_clean_at_chat_start: false,
            last_save_outcome: None,
            view_mode: ChatViewMode::DockedChat,
            panel_width_percent: None,
            current_undo_group: None,
            next_undo_group_id: 0,
        }
    }
}

pub struct PendingAiChatJob {
    pub receiver: tokio::sync::mpsc::UnboundedReceiver<StreamChunk>,
    pub task: tokio::task::JoinHandle<()>,
    pub profile_name: String,
    pub model_name: String,
    /// Runtime identity captured before the provider request was spawned.
    pub turn: Box<crate::agent_runtime::PendingTurnRef>,
    /// Prevents late output from attaching to a newly selected UI branch.
    pub branch_generation: u64,
    /// Codex app-server steering input. Other providers use ovim's local
    /// post-tool continuation boundary and leave this unset.
    pub steer_tx:
        Option<tokio::sync::mpsc::UnboundedSender<crate::ai::chat_types::ProviderSteerUpdate>>,
}

pub struct ScratchBufferState {
    pub scratch_buffer_id: BufferId,
    pub original_buffer_id: BufferId,
    pub original_input: String,
}

/// Tracks which lines the agent modified, per buffer.
/// Ranges are 0-indexed, inclusive: (start_line, end_line).
pub struct AgentEditTracker {
    /// Per-buffer modified line ranges: sorted, non-overlapping.
    pub all_edits: HashMap<BufferId, Vec<(usize, usize)>>,
}

impl AgentEditTracker {
    pub fn new() -> Self {
        Self {
            all_edits: HashMap::new(),
        }
    }

    /// Record that lines [start..=end] (0-indexed) were modified in the given buffer.
    pub fn record_edit(&mut self, buffer_id: BufferId, start_line: usize, end_line: usize) {
        let ranges = self.all_edits.entry(buffer_id).or_default();
        ranges.push((start_line, end_line));
        Self::merge_ranges(ranges);
    }

    /// Adjust all tracked ranges after lines were inserted.
    /// `after_line` is 0-indexed: N new lines inserted after this line shift
    /// all ranges with start > after_line by +count.
    pub fn adjust_for_insert(&mut self, buffer_id: BufferId, after_line: usize, count: usize) {
        if let Some(ranges) = self.all_edits.get_mut(&buffer_id) {
            for range in ranges.iter_mut() {
                if range.0 > after_line {
                    range.0 += count;
                    range.1 += count;
                } else if range.1 > after_line {
                    // Range overlaps the insertion point — extend it
                    range.1 += count;
                }
            }
        }
    }

    /// Adjust all tracked ranges after lines were deleted.
    /// Lines [start..=end] (0-indexed) were removed.
    pub fn adjust_for_delete(&mut self, buffer_id: BufferId, start_line: usize, end_line: usize) {
        let count = end_line - start_line + 1;
        if let Some(ranges) = self.all_edits.get_mut(&buffer_id) {
            ranges.retain_mut(|range| {
                if range.1 < start_line {
                    // Entirely before deletion — unchanged
                    true
                } else if range.0 > end_line {
                    // Entirely after deletion — shift down
                    range.0 -= count;
                    range.1 -= count;
                    true
                } else if range.0 >= start_line && range.1 <= end_line {
                    // Entirely within deletion — remove
                    false
                } else {
                    // Partially overlapping — shrink
                    if range.0 < start_line {
                        range.1 = start_line.saturating_sub(1);
                    } else {
                        range.0 = start_line;
                        range.1 = range.1.saturating_sub(count);
                    }
                    range.0 <= range.1
                }
            });
        }
    }

    /// Check if a line (0-indexed) in the given buffer was modified by the agent.
    pub fn is_line_modified(&self, buffer_id: BufferId, line: usize) -> bool {
        if let Some(ranges) = self.all_edits.get(&buffer_id) {
            for &(start, end) in ranges {
                if line >= start && line <= end {
                    return true;
                }
                if start > line {
                    break; // ranges are sorted
                }
            }
        }
        false
    }

    /// Get the next agent edit boundary from a given line (forward or backward).
    pub fn next_edit_boundary(
        &self,
        buffer_id: BufferId,
        from_line: usize,
        forward: bool,
    ) -> Option<usize> {
        let ranges = self.all_edits.get(&buffer_id)?;
        if forward {
            for &(start, _end) in ranges {
                if start > from_line {
                    return Some(start);
                }
            }
            // Wrap around to first
            ranges.first().map(|&(start, _)| start)
        } else {
            for &(start, _end) in ranges.iter().rev() {
                if start < from_line {
                    return Some(start);
                }
            }
            // Wrap around to last
            ranges.last().map(|&(start, _)| start)
        }
    }

    /// Total number of edit ranges across all buffers.
    pub fn total_edit_count(&self) -> usize {
        self.all_edits.values().map(|v| v.len()).sum()
    }

    /// Number of buffers that have agent edits.
    pub fn edited_buffer_count(&self) -> usize {
        self.all_edits.values().filter(|v| !v.is_empty()).count()
    }

    /// Clear all tracked edits.
    pub fn clear(&mut self) {
        self.all_edits.clear();
    }

    /// Merge overlapping/adjacent ranges in a sorted list.
    fn merge_ranges(ranges: &mut Vec<(usize, usize)>) {
        ranges.sort_by_key(|r| r.0);
        let mut i = 0;
        while i + 1 < ranges.len() {
            // Merge if overlapping or adjacent (end + 1 >= next start)
            if ranges[i].1 + 1 >= ranges[i + 1].0 {
                ranges[i].1 = ranges[i].1.max(ranges[i + 1].1);
                ranges.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod shell_transcript_tests {
    use super::*;

    fn call(id: &str) -> ToolCallInfo {
        ToolCallInfo {
            id: id.into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "echo test" }),
        }
    }

    #[test]
    fn transcript_drops_oldest_output_when_per_process_limit_is_exceeded() {
        let mut transcript = ShellTranscript::new(call("shell-1"), "echo test".into(), ".".into());
        let chunk_size = 8 * 1024;
        for marker in 0..(ShellTranscript::MAX_RETAINED_BYTES / chunk_size + 2) {
            transcript.append(ShellOutputStream::Stdout, vec![marker as u8; chunk_size]);
        }

        assert!(transcript.retained_bytes <= ShellTranscript::MAX_RETAINED_BYTES);
        assert_eq!(transcript.dropped_bytes, 2 * chunk_size);
        assert_eq!(transcript.chunks.front().unwrap().bytes[0], 2);
    }

    #[test]
    fn expiring_a_transcript_keeps_metadata_but_releases_output() {
        let mut transcript = ShellTranscript::new(call("shell-1"), "echo test".into(), ".".into());
        transcript.append(ShellOutputStream::Stderr, b"warning\n".to_vec());
        transcript.finish(ShellTranscriptPhase::Succeeded);
        transcript.expire_output();

        assert!(transcript.expired);
        assert!(transcript.chunks.is_empty());
        assert_eq!(transcript.retained_bytes, 0);
        assert_eq!(transcript.command, "echo test");
        assert_eq!(transcript.phase, ShellTranscriptPhase::Succeeded);
    }
}
