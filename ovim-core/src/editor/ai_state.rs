use crate::ai::chat_types::{ConversationTree, NodeId};
use crate::ai::skills::SkillCatalog;
use crate::ai::tools::ToolRegistry;
use crate::ai::{AiConfig, PROFILE_LOCAL};
use crate::buffer::BufferId;
use crate::mode::Mode;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) struct DurableRunServices {
    pub store: Arc<crate::run_log::LocalRunStore>,
    pub catalog: Arc<crate::run_log::RunCatalog>,
}

#[derive(Clone)]
pub(crate) struct DurableChatBinding {
    pub binding: crate::run_log::ConversationBinding,
    pub locator: crate::agent_runtime::ConversationLocator,
}

#[derive(Debug, Clone)]
pub struct ChatRuntimeNodeRef {
    pub event_id: crate::run_log::EventId,
    pub branch: crate::agent_runtime::BranchLocator,
}

#[derive(Debug, Clone)]
pub struct AiSelectionSnapshot {
    /// Buffer the coordinates and selected text were captured from.
    pub buffer_id: BufferId,
    pub start_line: usize,
    /// Inclusive, zero-based grapheme column.
    pub start_col: usize,
    pub end_line: usize,
    /// Exclusive, zero-based grapheme column.
    pub end_col: usize,
    /// Half-open Rope character offsets for the selected text.
    pub start_char: usize,
    pub end_char: usize,
    pub anchor_line: usize,
    pub selected_text: String,
    pub selection_mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAuthDialogPhase {
    Offer,
    Refreshing,
    WaitingForBrowser,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthDialogSummary {
    pub phase: CodexAuthDialogPhase,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub(crate) enum CodexAuthResume {
    None,
    SubmitChat,
}

pub(crate) struct CodexAuthDialog {
    pub phase: CodexAuthDialogPhase,
    pub detail: Option<String>,
    pub authorize_url: Option<String>,
    pub resume: CodexAuthResume,
}

pub(crate) struct PendingCodexAuth {
    pub receiver: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
    pub task: tokio::task::JoinHandle<()>,
}

impl Drop for PendingCodexAuth {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct AiState {
    /// Provider-independent run/agent/turn history and transient bindings.
    pub agent_runtime: Box<crate::agent_runtime::AgentRuntime>,
    /// Present when durable run storage could not be initialized and this
    /// process is recording agent history in memory only. Retained for a later
    /// status/diagnostics projection rather than silently hiding provenance.
    pub run_storage_warning: Option<Box<str>>,
    /// Durable services are retained alongside the runtime so catalog identity
    /// and event history use exactly the same store as live appends.
    pub(crate) durable_runs: Option<Box<DurableRunServices>>,
    pub(crate) durable_chat_bindings: HashMap<(BufferId, String), DurableChatBinding>,
    /// Persisted conversations are restored only when explicitly enabled by
    /// the process entry point (`ovim --resume`).
    pub(crate) resume_durable_conversations: bool,
    pub config: AiConfig,
    /// Dedicated read-only delegated-agent control plane. It snapshots the
    /// startup config and never replaces root chat orchestration.
    pub(crate) subagents: Box<super::ai_subagents::AiSubagentService>,
    pub active_selection: Option<AiSelectionSnapshot>,
    /// Global because opening chat can require sign-in.
    pub(crate) codex_auth_dialog: Option<CodexAuthDialog>,
    pub(crate) pending_codex_auth: Option<PendingCodexAuth>,
    pub(crate) pending_external_url: Option<String>,
    pub active_profile: String,
    /// Active chat session state (None when chat is closed).
    pub chat: Option<super::ai_chat_state::AiChatState>,
    /// Persistent conversations keyed by (stable_buffer_id, conversation_name).
    pub conversations: HashMap<(BufferId, String), ConversationTree>,
    /// UI message-node projection onto durable event/branch identity.
    pub conversation_runtime_nodes:
        HashMap<(BufferId, String), HashMap<NodeId, ChatRuntimeNodeRef>>,
    /// Registry of all available tools.
    pub tool_registry: ToolRegistry,
    /// User-configured skill metadata and lazily activated instructions.
    pub skill_catalog: SkillCatalog,
    /// Monotonic signal that a running agent has paused for user attention.
    /// Frontends compare this value with the last one they observed so a
    /// single prompt can notify once without coupling sound playback to
    /// polling or rendering.
    pub ai_attention_generation: u64,
    /// Whether we've already asked for no-repo folder access in this process session.
    pub no_repo_session_prompted: bool,
    /// User-approved folder root for project-level AI tools when not in a git repo.
    pub no_repo_session_allowed_root: Option<PathBuf>,
    /// Loaded workflow specs keyed by workflow name.
    pub workflows: HashMap<String, crate::ai::workflow::WorkflowSpec>,
    /// Historical workflow runs (latest appended at end).
    pub workflow_runs: Vec<crate::ai::WorkflowRunRecord>,
    /// Pending async workflow runs.
    pub pending_workflow_runs: Vec<crate::ai::workflow::PendingWorkflowRun>,
    /// Monotonic run id for workflow executions.
    pub next_workflow_run_id: u64,
}

impl AiState {
    fn with_agent_runtime(
        agent_runtime: crate::agent_runtime::AgentRuntime,
        run_storage_warning: Option<Box<str>>,
        durable_runs: Option<DurableRunServices>,
    ) -> Self {
        let mut config = AiConfig::load().unwrap_or_else(|_| AiConfig::default());
        let default_profile = if config.profiles.contains_key(&config.default_profile) {
            config.default_profile.clone()
        } else {
            PROFILE_LOCAL.to_string()
        };

        // Initialize default contexts if empty
        if config.contexts.is_empty() {
            for ctx in &["chat", "query"] {
                config
                    .contexts
                    .insert(ctx.to_string(), default_profile.clone());
            }
        }

        let subagents = Box::new(super::ai_subagents::AiSubagentService::new(&config));
        // Parent controls are intentionally not registered as ordinary editor
        // tools. Their schemas are injected only for an enabled, durable root
        // turn that currently owns DispatchAgents authority.
        let tool_registry = ToolRegistry::new();
        #[cfg(not(test))]
        let skill_catalog = SkillCatalog::discover();
        #[cfg(test)]
        let skill_catalog = SkillCatalog::default();
        Self {
            agent_runtime: Box::new(agent_runtime),
            run_storage_warning,
            durable_runs: durable_runs.map(Box::new),
            durable_chat_bindings: HashMap::new(),
            resume_durable_conversations: false,
            config,
            subagents,
            active_selection: None,
            codex_auth_dialog: None,
            pending_codex_auth: None,
            pending_external_url: None,
            active_profile: default_profile,
            chat: None,
            conversations: HashMap::new(),
            conversation_runtime_nodes: HashMap::new(),
            tool_registry,
            skill_catalog,
            ai_attention_generation: 0,
            no_repo_session_prompted: false,
            no_repo_session_allowed_root: None,
            workflows: HashMap::new(),
            workflow_runs: Vec::new(),
            pending_workflow_runs: Vec::new(),
            next_workflow_run_id: 1,
        }
    }

    /// Creates an AI state backed by an explicit durable layout. Tests and
    /// embedders use this instead of mutating `OVIM_RUNS_DIR` process-wide.
    pub(crate) fn with_run_storage_layout(
        layout: crate::run_log::RunStorageLayout,
    ) -> Result<Self, crate::run_log::RunLogError> {
        // Validate the root eagerly so a permission or path failure can be
        // surfaced before the first provider turn begins.
        layout.ensure_root()?;
        let sink = Arc::new(crate::run_log::LocalRunStore::new(layout.clone()));
        let catalog = crate::run_log::RunCatalog::open(&layout)
            .map(Arc::new)
            .map_err(|error| crate::run_log::RunLogError::Storage {
                operation: "open durable run catalog".into(),
                detail: error.to_string(),
            })?;
        Ok(Self::with_agent_runtime(
            crate::agent_runtime::AgentRuntime::with_sink(sink.clone()),
            None,
            Some(DurableRunServices {
                store: sink,
                catalog,
            }),
        ))
    }

    fn with_discovered_run_storage(
        discovered: Result<crate::run_log::RunStorageLayout, crate::run_log::RunLogError>,
    ) -> Self {
        match discovered.and_then(Self::with_run_storage_layout) {
            Ok(state) => state,
            Err(error) => Self::with_agent_runtime(
                crate::agent_runtime::AgentRuntime::new(),
                Some(
                    format!(
                        "durable agent run storage is unavailable; history is in-memory only: {error}"
                    )
                    .into_boxed_str(),
                ),
                None,
            ),
        }
    }
}

impl Default for AiState {
    fn default() -> Self {
        // Unit tests must never discover or write the user's real data path.
        #[cfg(test)]
        {
            Self::with_agent_runtime(crate::agent_runtime::AgentRuntime::new(), None, None)
        }

        #[cfg(not(test))]
        {
            Self::with_discovered_run_storage(crate::run_log::RunStorageLayout::discover())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{AgentSpec, BranchLocator};
    use crate::run_log::{LocalRunStore, RunEventSink, RunStorageLayout};

    #[test]
    fn explicit_durable_state_persists_runs_across_recreation() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));

        let first_run = {
            let mut state = AiState::with_run_storage_layout(layout.clone()).unwrap();
            let turn = state
                .agent_runtime
                .begin_turn(
                    "first conversation",
                    BranchLocator("main".into()),
                    "persist the first turn",
                    AgentSpec::chat(),
                )
                .unwrap();
            assert!(state.run_storage_warning.is_none());
            turn.run_id
        };

        let second_run = {
            let mut recreated = AiState::with_run_storage_layout(layout.clone()).unwrap();
            recreated
                .agent_runtime
                .begin_turn(
                    "second conversation",
                    BranchLocator("main".into()),
                    "persist after reopening",
                    AgentSpec::chat(),
                )
                .unwrap()
                .run_id
        };

        // Runtime conversation maps are intentionally not restored yet. This
        // assertion covers storage discovery/reopen only.
        let reopened = LocalRunStore::new(layout);
        let runs = reopened.runs().unwrap();
        assert!(runs.contains(&first_run));
        assert!(runs.contains(&second_run));
        assert!(!reopened.events(&first_run).unwrap().is_empty());
        assert!(!reopened.events(&second_run).unwrap().is_empty());
    }

    #[test]
    fn ordinary_test_default_is_explicitly_transient() {
        let state = AiState::default();
        assert!(state.run_storage_warning.is_none());
    }

    #[test]
    fn initialization_failure_falls_back_with_a_visible_warning() {
        let state =
            AiState::with_discovered_run_storage(Err(crate::run_log::RunLogError::Storage {
                operation: "test durable initialization".into(),
                detail: "read-only location".into(),
            }));

        assert!(state
            .run_storage_warning
            .as_deref()
            .unwrap()
            .contains("history is in-memory only"));
    }
}
