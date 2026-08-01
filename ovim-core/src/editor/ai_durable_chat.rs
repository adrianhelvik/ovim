use crate::agent_runtime::{BranchLocator, ConversationLocator};
use crate::ai::chat_types::ConversationTree;
use crate::buffer::BufferId;
use crate::run_log::{
    BranchLifecycleEvent, ConversationKey, ConversationScope, EventEnvelope, EventKind,
    MessageRole, RepositoryRegistration, RepositorySnapshot, RunEventSink,
};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::ai_state::DurableChatBinding;
use super::Editor;

impl Editor {
    /// Controls whether opening a chat may restore persisted conversation
    /// history. The default is false; the binary enables it only for
    /// `ovim --resume`.
    pub fn set_ai_conversation_resume_enabled(&mut self, enabled: bool) {
        self.ai_state.resume_durable_conversations = enabled;
    }

    /// Establish durable catalog identity and restore history before opening
    /// the UI. Buffer ids are deliberately absent from the runtime locator.
    pub(crate) fn prepare_durable_ai_chat(
        &mut self,
        buffer_id: BufferId,
        logical_name: &str,
    ) -> Result<()> {
        let ui_key = (buffer_id, logical_name.to_owned());
        let durable_name = if logical_name.trim().is_empty() {
            "chat"
        } else {
            logical_name
        };
        let Some(services) = self.ai_state.durable_runs.as_ref() else {
            return match self.ai_state.run_storage_warning.as_deref() {
                Some(warning) => Err(anyhow!(
                    "{warning}. Check Ovim's data-directory permissions and reopen the chat"
                )),
                None => Ok(()),
            };
        };
        if self.ai_state.durable_chat_bindings.contains_key(&ui_key) {
            return Ok(());
        }

        let start = self
            .get_buffer_by_id(buffer_id)
            .and_then(|buffer| buffer.file_path().map(PathBuf::from))
            .or_else(|| self.ai_project_start_path())
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| anyhow!("no repository start path is available"))?;
        // New buffers commonly point at a file that has not been written yet.
        // libgit2 cannot discover upward from a nonexistent path, so begin at
        // its nearest existing ancestor while retaining the original path for
        // conversation scoping below.
        let repository_start = start
            .ancestors()
            .find(|candidate| candidate.exists())
            .unwrap_or(start.as_path());
        let (repository_id, project_root) = match RepositorySnapshot::capture(
            repository_start,
            crate::run_log::RepositoryId::new(),
        ) {
            Ok(snapshot) => {
                let worktree = snapshot
                    .local_paths
                    .workdir
                    .clone()
                    .ok_or_else(|| anyhow!("bare repositories cannot host an editor chat"))?;
                let common_git_dir = snapshot
                    .local_paths
                    .common_git_dir
                    .clone()
                    .unwrap_or_else(|| snapshot.local_paths.git_dir.clone());
                let repository = services
                    .catalog
                    .register_repository(RepositoryRegistration {
                        common_git_dir,
                        worktree_aliases: vec![worktree.clone()],
                    })?;
                (repository.repository_id, worktree)
            }
            Err(crate::run_log::RepositorySnapshotError::NotRepository { .. }) => {
                let approved_root = self
                    .ai_state
                    .no_repo_session_allowed_root
                    .as_ref()
                    .map(|root| crate::ai::path_policy::canonicalize_or_normalize(root))
                    .ok_or_else(|| {
                        anyhow!(
                            "approve this folder in the chat prompt to enable durable agent edits"
                        )
                    })?;
                let start = crate::ai::path_policy::canonicalize_or_normalize(&start);
                if !start.starts_with(&approved_root) {
                    return Err(anyhow!(
                        "the chat target {} is outside the approved folder {}",
                        start.display(),
                        approved_root.display()
                    ));
                }
                let folder = services
                    .catalog
                    .register_approved_folder(&approved_root)
                    .context("could not persist the approved folder identity")?;
                (folder.repository_id, approved_root)
            }
            Err(error) => return Err(error).context("could not inspect the Git repository"),
        };
        let conversation_key = ConversationKey {
            repository_id,
            scope: conversation_scope(self.get_buffer_by_id(buffer_id), &project_root)?,
            logical_name: durable_name.to_owned(),
        };
        let resume = self.ai_state.resume_durable_conversations;
        // Tracks whether the run this editor writes to is discoverable by a
        // later `ovim --resume`; unbound runs are process-local and never
        // displace previously persisted history.
        let mut fresh_run_is_unbound = false;
        let mut binding = if resume {
            services
                .catalog
                .open_conversation(conversation_key, crate::run_log::BranchId::new())?
        } else {
            match services
                .catalog
                .start_fresh_conversation(conversation_key, crate::run_log::BranchId::new())?
            {
                crate::run_log::FreshConversationStart::Bound(binding) => binding,
                crate::run_log::FreshConversationStart::Unbound(binding) => {
                    fresh_run_is_unbound = true;
                    binding
                }
            }
        };
        if resume && services.store.events(&binding.run_id)?.is_empty() {
            // An empty run has no history to preserve, so rebinding to a
            // fresh run cannot orphan anything.
            binding = services
                .catalog
                .start_conversation(binding.key.clone(), crate::run_log::BranchId::new())?;
        }
        let locator = ConversationLocator(format!("conversation:{}", binding.conversation_id));
        let prepared = (|| -> Result<Vec<EventEnvelope>> {
            let events = services.store.events(&binding.run_id)?;
            self.ai_state.agent_runtime.restore_conversation(
                locator.clone(),
                binding.clone(),
                events.clone(),
            )?;
            Ok(events)
        })();
        let (mut binding, locator, mut events, fallback_status) = match prepared {
            Ok(events) => (binding, locator, events, None),
            Err(error)
                if error
                    .downcast_ref::<crate::agent_runtime::AgentRuntimeError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::agent_runtime::AgentRuntimeError::InvalidHistory(_)
                                | crate::agent_runtime::AgentRuntimeError::UnrecoveredWork(_)
                        )
                    }) =>
            {
                let unrecovered_work = error
                    .downcast_ref::<crate::agent_runtime::AgentRuntimeError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::agent_runtime::AgentRuntimeError::UnrecoveredWork(_)
                        )
                    });
                let recovered = if unrecovered_work {
                    // Nonterminal work is only sealed when every process that
                    // registered as a writer of this run is provably dead.
                    recover_run_with_no_live_owner(
                        services,
                        &mut self.ai_state.agent_runtime,
                        &binding,
                        &locator,
                    )?
                } else {
                    None
                };
                if let Some(events) = recovered {
                    (
                        binding,
                        locator,
                        events,
                        Some("Recovered an interrupted AI conversation after a crash"),
                    )
                } else {
                    // Never mutate a run that may still have a live writer.
                    // Keep it intact — and, for possibly-active runs, still
                    // discoverable — and continue on an independent run.
                    let fresh = if unrecovered_work {
                        services
                            .catalog
                            .start_fresh_conversation(
                                binding.key.clone(),
                                crate::run_log::BranchId::new(),
                            )?
                            .into_binding()
                    } else {
                        // Invalid history can never be restored; rebinding
                        // keeps future opens working while the broken run
                        // stays on disk for diagnosis.
                        services.catalog.start_conversation(
                            binding.key.clone(),
                            crate::run_log::BranchId::new(),
                        )?
                    };
                    let fresh_locator =
                        ConversationLocator(format!("conversation:{}", fresh.conversation_id));
                    self.ai_state.agent_runtime.restore_conversation(
                        fresh_locator.clone(),
                        fresh.clone(),
                        Vec::new(),
                    )?;
                    let status = if unrecovered_work {
                        "Previous AI run may still be active; started an independent chat \
                         that will not be resumable"
                    } else {
                        "Previous AI chat history was invalid; started a fresh chat"
                    };
                    (fresh, fresh_locator, Vec::new(), Some(status))
                }
            }
            Err(error) => return Err(error),
        };

        if resume && !events.is_empty() {
            let source = self
                .ai_state
                .agent_runtime
                .selected_branch(&locator)
                .map(|(locator, _)| locator.clone())
                .ok_or_else(|| anyhow!("resumed conversation has no selected branch"))?;
            let source_tip = self
                .ai_state
                .agent_runtime
                .selected_branch_tip(&locator)
                .cloned()
                .ok_or_else(|| anyhow!("resumed conversation has no branch tip"))?;
            let target = BranchLocator(format!("resume-{}", crate::run_log::BranchId::new()));
            self.ai_state.agent_runtime.fork_branch_at(
                &locator,
                &source,
                target.clone(),
                source_tip,
            )?;
            self.ai_state
                .agent_runtime
                .select_branch(&locator, &target)?;
            let branch_id = self
                .ai_state
                .agent_runtime
                .selected_branch(&locator)
                .map(|(_, branch)| branch.branch_id.clone())
                .ok_or_else(|| anyhow!("resumed branch was not selected"))?;
            let catalog_updated = services
                .catalog
                .update_selected_branch(&binding, branch_id.clone())?;
            binding.selected_branch_id = branch_id;
            if !catalog_updated {
                crate::log_debug!(
                    "agent_runtime",
                    "conversation catalog advanced in another process; continuing independent run {}",
                    binding.run_id
                );
            }
            events = services.store.events(&binding.run_id)?;
        }
        // Record this process as a live writer so a later resume can prove
        // whether a crashed run's owner is really gone before recovering it.
        let (owner_pid, owner_start_time) = crate::run_log::current_process_liveness();
        services
            .catalog
            .register_run_owner(&binding.run_id, owner_pid, owner_start_time)?;
        let (conversation, nodes) = project_visible_messages(&events, &binding.selected_branch_id);
        self.ai_state
            .conversations
            .insert(ui_key.clone(), conversation);
        self.ai_state
            .conversation_runtime_nodes
            .insert(ui_key.clone(), nodes);
        self.ai_state
            .durable_chat_bindings
            .insert(ui_key, DurableChatBinding { binding, locator });
        if let Some(status) = fallback_status {
            self.set_status_message(status);
        } else if !resume {
            self.set_status_message(if fresh_run_is_unbound {
                "Started a fresh AI conversation that will not be resumable; the previous \
                conversation is still available via ovim --resume"
            } else {
                "Started a fresh AI conversation; launch ovim with --resume to restore it later"
            });
        }
        Ok(())
    }

    pub(crate) fn durable_ai_mutations_available(&self) -> bool {
        if self.ai_state.run_storage_warning.is_some() {
            return false;
        }
        #[cfg(test)]
        if self.ai_state.durable_runs.is_none() {
            // Legacy editor unit tests intentionally use the explicit
            // in-memory runtime and are not a production authorization path.
            return true;
        }
        self.ai_state.durable_runs.is_some()
            && self
                .ai_state
                .durable_chat_bindings
                .contains_key(&self.ai_chat_conversation_key())
    }

    /// Forget the provider-native thread while retaining ovim's durable audit log.
    /// The next turn reconstructs context exclusively from the newly cleared UI tree.
    pub(crate) fn reset_durable_ai_chat_provider_session(&mut self) -> Result<()> {
        let key = self.ai_chat_conversation_key();
        let Some(services) = self.ai_state.durable_runs.as_ref() else {
            return Ok(());
        };
        let Some(binding) = self.ai_state.durable_chat_bindings.get(&key) else {
            return Ok(());
        };
        let Some((_, branch)) = self
            .ai_state
            .agent_runtime
            .selected_branch(&binding.locator)
        else {
            return Ok(());
        };
        crate::ai::DurableCodexSession::new(
            services.catalog.clone(),
            binding.binding.root_agent_id.clone(),
            branch.branch_id.clone(),
        )
        .invalidate()?;
        Ok(())
    }
}

/// Attempts append-only crash recovery for a resumed run whose restore failed
/// with nonterminal work. Returns the restored events only when every
/// registered owner of the run is provably dead (PID plus process-start-time
/// marker, mirroring stale-session detection), sealing succeeded, and the
/// recovered history restored cleanly. Any other outcome — no recorded owners
/// (stores that predate owner tracking), a possibly-live owner, or a concurrent
/// writer detected while sealing — leaves the run untouched and returns `None`.
fn recover_run_with_no_live_owner(
    services: &super::ai_state::DurableRunServices,
    runtime: &mut crate::agent_runtime::AgentRuntime,
    binding: &crate::run_log::ConversationBinding,
    locator: &ConversationLocator,
) -> Result<Option<Vec<EventEnvelope>>> {
    let owners = services.catalog.run_owners(&binding.run_id)?;
    if owners.is_empty()
        || owners
            .iter()
            .any(|owner| crate::run_log::process_is_alive(owner.pid, owner.process_start_time))
    {
        return Ok(None);
    }
    let sink: std::sync::Arc<dyn RunEventSink> = services.store.clone();
    let plan = match crate::run_log::RecoveryPlanner::new(sink).plan(&binding.run_id) {
        Ok(plan) => plan,
        Err(error) => {
            crate::log_debug!(
                "agent_runtime",
                "crash recovery for run {} could not be planned: {error}",
                binding.run_id
            );
            return Ok(None);
        }
    };
    if let Err(error) = crate::run_log::apply_recovery(&plan, services.store.as_ref(), true) {
        // A stale plan means another process appended concurrently, so the
        // run is not exclusively ours after all; leave it intact.
        crate::log_debug!(
            "agent_runtime",
            "crash recovery for run {} was not applied: {error}",
            binding.run_id
        );
        return Ok(None);
    }
    let events = services.store.events(&binding.run_id)?;
    match runtime.restore_conversation(locator.clone(), binding.clone(), events.clone()) {
        Ok(_) => Ok(Some(events)),
        Err(error) => {
            crate::log_debug!(
                "agent_runtime",
                "recovered run {} still failed to restore: {error}",
                binding.run_id
            );
            Ok(None)
        }
    }
}

fn conversation_scope(
    buffer: Option<&crate::buffer::Buffer>,
    worktree: &Path,
) -> Result<ConversationScope> {
    let Some(path) = buffer.and_then(|buffer| buffer.file_path()) else {
        return Ok(ConversationScope::NoFile);
    };
    let path = crate::ai::path_policy::canonicalize_or_normalize(Path::new(path));
    let root = crate::ai::path_policy::canonicalize_or_normalize(worktree);
    let relative = path
        .strip_prefix(&root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    if relative.as_os_str().is_empty() {
        Ok(ConversationScope::NoFile)
    } else {
        Ok(ConversationScope::RepositoryPath(relative.to_owned()))
    }
}

fn project_visible_messages(
    events: &[EventEnvelope],
    selected_branch_id: &crate::run_log::BranchId,
) -> (
    ConversationTree,
    HashMap<crate::ai::chat_types::NodeId, super::ai_state::ChatRuntimeNodeRef>,
) {
    let by_id = events
        .iter()
        .map(|event| (event.event_id.clone(), event))
        .collect::<HashMap<_, _>>();
    let labels = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchLifecycle(BranchLifecycleEvent {
                branch_id,
                label: Some(label),
                ..
            }) => Some((branch_id.clone(), BranchLocator(label.clone()))),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut causal = HashSet::new();
    let mut cursor = events
        .iter()
        .rev()
        .find(|event| event.branch_id.as_ref() == Some(selected_branch_id));
    while let Some(event) = cursor {
        if !causal.insert(event.event_id.clone()) {
            break;
        }
        cursor = event
            .caused_by
            .as_ref()
            .and_then(|cause| by_id.get(cause).copied());
    }

    let mut conversation = ConversationTree::new();
    let mut nodes = HashMap::new();
    for event in events
        .iter()
        .filter(|event| causal.contains(&event.event_id))
    {
        if matches!(
            &event.kind,
            EventKind::Unknown { name, .. }
                if name == crate::agent_runtime::CONVERSATION_CONTEXT_RESET_EVENT
        ) {
            conversation = ConversationTree::new();
            nodes.clear();
            continue;
        }
        let EventKind::Message(message) = &event.kind else {
            continue;
        };
        let node =
            match message.role {
                MessageRole::User => conversation.append_user_message(message.content.clone()),
                MessageRole::Agent => conversation
                    .append_assistant_message(message.content.clone(), "Agent".to_owned()),
                MessageRole::ReasoningSummary => conversation
                    .append_thinking_message(message.content.clone(), "Agent".to_owned()),
                MessageRole::System => continue,
            };
        if let Some(branch) = event
            .branch_id
            .as_ref()
            .and_then(|id| labels.get(id))
            .cloned()
        {
            nodes.insert(
                node,
                super::ai_state::ChatRuntimeNodeRef {
                    event_id: event.event_id.clone(),
                    branch,
                },
            );
        }
    }
    (conversation, nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::editor::ai_state::AiState;
    use crate::run_log::RunStorageLayout;
    use std::fs;

    fn repository_file() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        git2::Repository::init(directory.path()).unwrap();
        let file = directory.path().join("src.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        (directory, file)
    }

    fn durable_editor(file: &Path, layout: RunStorageLayout) -> Editor {
        let mut editor = Editor::default();
        editor.open_file(file).unwrap();
        editor.ai_state = Box::new(AiState::with_run_storage_layout(layout).unwrap());
        editor.set_ai_conversation_resume_enabled(true);
        editor
    }

    fn editable_chat() -> ChatOpts {
        ChatOpts {
            name: "chat".into(),
            allow_edits: true,
            ..Default::default()
        }
    }

    fn advertised_tools(editor: &Editor) -> HashSet<String> {
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .unwrap();
        editor
            .build_tool_schemas_for_chat(profile)
            .into_iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect()
    }

    #[test]
    fn durable_storage_failure_is_actionable_instead_of_silent() {
        let mut editor = Editor::default();
        editor.ai_state.run_storage_warning = Some(
            "durable agent run storage is unavailable: permission denied"
                .to_string()
                .into_boxed_str(),
        );

        let error = editor
            .prepare_durable_ai_chat(editor.buffer().id(), "chat")
            .unwrap_err();

        assert!(error.to_string().contains("permission denied"));
        assert!(error.to_string().contains("permissions"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn editable_repository_chat_advertises_shell_and_mutations() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let mut editor = durable_editor(&file, RunStorageLayout::new(storage.path().join("runs")));

        editor.open_ai_chat(editable_chat()).unwrap();

        let tools = advertised_tools(&editor);
        assert!(tools.contains("bash"));
        assert!(tools.contains("edit_range"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approved_non_git_folder_advertises_shell_and_mutations_at_that_boundary() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let storage = tempfile::tempdir().unwrap();
        let mut editor = durable_editor(&file, RunStorageLayout::new(storage.path().join("runs")));

        editor.open_ai_chat(editable_chat()).unwrap();
        assert!(editor.ai_chat_resolve_pending_no_repo_folder_approval(true));

        assert_eq!(
            editor.ai_effective_project_root().unwrap(),
            crate::ai::path_policy::canonicalize_or_normalize(folder.path())
        );
        assert!(editor.durable_ai_mutations_available());
        let tools = advertised_tools(&editor);
        assert!(tools.contains("bash"));
        assert!(tools.contains("edit_range"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unapproved_non_git_folder_fails_closed() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let storage = tempfile::tempdir().unwrap();
        let mut editor = durable_editor(&file, RunStorageLayout::new(storage.path().join("runs")));

        editor.open_ai_chat(editable_chat()).unwrap();

        assert!(!editor.durable_ai_mutations_available());
        let tools = advertised_tools(&editor);
        assert!(!tools.contains("bash"));
        assert!(!tools.contains("edit_range"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_only_chat_never_advertises_shell_or_mutations() {
        let (_repository, repository_file) = repository_file();
        let non_git = tempfile::tempdir().unwrap();
        let non_git_file = non_git.path().join("notes.txt");
        fs::write(&non_git_file, "hello\n").unwrap();

        for file in [&repository_file, &non_git_file] {
            let storage = tempfile::tempdir().unwrap();
            let mut editor =
                durable_editor(file, RunStorageLayout::new(storage.path().join("runs")));
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            if editor.ai_chat_has_pending_no_repo_folder_approval() {
                assert!(editor.ai_chat_resolve_pending_no_repo_folder_approval(true));
            }

            let tools = advertised_tools(&editor);
            assert!(!tools.contains("bash"));
            assert!(!tools.contains("edit_range"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approved_folder_conversation_resumes_with_the_same_durable_identity() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let original = {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(editable_chat()).unwrap();
            editor.ai_chat_resolve_pending_no_repo_folder_approval(true);
            let turn = editor.begin_ai_runtime_turn("remember this").unwrap();
            editor.ai_state.agent_runtime.complete_turn(&turn).unwrap();
            editor
                .ai_state
                .durable_chat_bindings
                .get(&editor.ai_chat_conversation_key())
                .unwrap()
                .binding
                .clone()
        };

        let mut resumed = durable_editor(&file, layout);
        resumed.ai_state.no_repo_session_allowed_root = Some(folder.path().to_path_buf());
        resumed.open_ai_chat(editable_chat()).unwrap();
        let binding = &resumed
            .ai_state
            .durable_chat_bindings
            .get(&resumed.ai_chat_conversation_key())
            .unwrap()
            .binding;
        assert_eq!(binding.key.repository_id, original.key.repository_id);
        assert_eq!(binding.run_id, original.run_id);
        assert_eq!(resumed.ai_chat_messages().len(), 1);
    }

    #[test]
    fn new_file_inside_repository_gets_durable_chat_identity() {
        let directory = tempfile::tempdir().unwrap();
        git2::Repository::init(directory.path()).unwrap();
        let storage = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state =
            AiState::with_run_storage_layout(RunStorageLayout::new(storage.path().join("runs")))
                .unwrap();
        let new_file = directory.path().join("nested").join("README.md");
        editor.set_file_path(new_file.to_string_lossy().into_owned());

        let buffer_id = editor.buffer().id();
        editor.prepare_durable_ai_chat(buffer_id, "chat").unwrap();

        assert!(editor
            .ai_state
            .durable_chat_bindings
            .contains_key(&(buffer_id, "chat".to_string())));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reopening_same_repository_path_restores_messages_on_a_new_branch() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let first_binding = {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            let turn = editor.begin_ai_runtime_turn("inspect this").unwrap();
            editor
                .ai_state
                .agent_runtime
                .append_agent_message(&turn, "It is small.")
                .unwrap();
            editor.ai_state.agent_runtime.complete_turn(&turn).unwrap();
            editor
                .ai_state
                .durable_chat_bindings
                .get(&editor.ai_chat_conversation_key())
                .unwrap()
                .binding
                .clone()
        };

        let mut reopened = durable_editor(&file, layout);
        reopened.open_ai_chat(ChatOpts::default()).unwrap();
        let restored = reopened
            .ai_state
            .durable_chat_bindings
            .get(&reopened.ai_chat_conversation_key())
            .unwrap();
        assert_eq!(
            restored.binding.conversation_id,
            first_binding.conversation_id
        );
        assert_eq!(restored.binding.run_id, first_binding.run_id);
        assert_ne!(
            restored.binding.selected_branch_id,
            first_binding.selected_branch_id
        );
        let messages = reopened.conversation().unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "inspect this");
        assert_eq!(messages[1].content, "It is small.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multiple_resumed_editors_continue_on_independent_branches() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        {
            let mut original = durable_editor(&file, layout.clone());
            original.open_ai_chat(ChatOpts::default()).unwrap();
            let turn = original.begin_ai_runtime_turn("shared history").unwrap();
            original
                .ai_state
                .agent_runtime
                .append_agent_message(&turn, "base answer")
                .unwrap();
            original
                .ai_state
                .agent_runtime
                .complete_turn(&turn)
                .unwrap();
        }

        let mut first = durable_editor(&file, layout.clone());
        first.open_ai_chat(ChatOpts::default()).unwrap();
        let first_binding = first
            .ai_state
            .durable_chat_bindings
            .get(&first.ai_chat_conversation_key())
            .unwrap()
            .binding
            .clone();

        let mut second = durable_editor(&file, layout);
        second.open_ai_chat(ChatOpts::default()).unwrap();
        let second_binding = second
            .ai_state
            .durable_chat_bindings
            .get(&second.ai_chat_conversation_key())
            .unwrap()
            .binding
            .clone();

        assert_eq!(first_binding.run_id, second_binding.run_id);
        assert_ne!(
            first_binding.selected_branch_id,
            second_binding.selected_branch_id
        );

        let first_turn = first.begin_ai_runtime_turn("first continuation").unwrap();
        first
            .ai_state
            .agent_runtime
            .complete_turn(&first_turn)
            .unwrap();
        let second_turn = second.begin_ai_runtime_turn("second continuation").unwrap();
        second
            .ai_state
            .agent_runtime
            .complete_turn(&second_turn)
            .unwrap();
        assert_ne!(first_turn.branch_id, second_turn.branch_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reopening_without_resume_starts_fresh_and_preserves_old_run() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let old_run = {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            let turn = editor.begin_ai_runtime_turn("expensive history").unwrap();
            editor
                .ai_state
                .agent_runtime
                .append_agent_message(&turn, "large answer")
                .unwrap();
            editor.ai_state.agent_runtime.complete_turn(&turn).unwrap();
            turn.run_id
        };

        let mut reopened = durable_editor(&file, layout.clone());
        reopened.set_ai_conversation_resume_enabled(false);
        reopened.open_ai_chat(ChatOpts::default()).unwrap();
        let fresh_run = reopened
            .ai_state
            .durable_chat_bindings
            .get(&reopened.ai_chat_conversation_key())
            .unwrap()
            .binding
            .run_id
            .clone();

        assert_ne!(fresh_run, old_run);
        assert!(reopened.ai_chat_messages().is_empty());
        assert!(!reopened
            .ai_state
            .durable_runs
            .as_ref()
            .unwrap()
            .store
            .events(&old_run)
            .unwrap()
            .is_empty());
        drop(reopened);

        // The old conversation must remain reachable, not merely present on
        // disk: a later resume restores the original history.
        let mut resumed = durable_editor(&file, layout);
        resumed.open_ai_chat(ChatOpts::default()).unwrap();
        let restored = resumed
            .ai_state
            .durable_chat_bindings
            .get(&resumed.ai_chat_conversation_key())
            .unwrap()
            .binding
            .clone();
        assert_eq!(restored.run_id, old_run);
        let messages = resumed.conversation().unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "expensive history");
        assert_eq!(messages[1].content, "large answer");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_after_default_open_restores_the_original_history() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let original = {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            for (question, answer) in [
                ("first question", "first answer"),
                ("second question", "second answer"),
            ] {
                let turn = editor.begin_ai_runtime_turn(question).unwrap();
                editor
                    .ai_state
                    .agent_runtime
                    .append_agent_message(&turn, answer)
                    .unwrap();
                editor.ai_state.agent_runtime.complete_turn(&turn).unwrap();
            }
            editor
                .ai_state
                .durable_chat_bindings
                .get(&editor.ai_chat_conversation_key())
                .unwrap()
                .binding
                .clone()
        };

        // A default (non-resume) open starts fresh without displacing the
        // discovery of the long conversation.
        {
            let mut default_open = durable_editor(&file, layout.clone());
            default_open.set_ai_conversation_resume_enabled(false);
            default_open.open_ai_chat(ChatOpts::default()).unwrap();
            assert!(default_open.ai_chat_messages().is_empty());
            assert!(default_open.status_message().contains("--resume"));
        }

        let mut resumed = durable_editor(&file, layout);
        resumed.open_ai_chat(ChatOpts::default()).unwrap();
        let restored = resumed
            .ai_state
            .durable_chat_bindings
            .get(&resumed.ai_chat_conversation_key())
            .unwrap()
            .binding
            .clone();
        assert_eq!(restored.conversation_id, original.conversation_id);
        assert_eq!(restored.run_id, original.run_id);
        let contents: Vec<_> = resumed
            .conversation()
            .unwrap()
            .messages()
            .iter()
            .map(|message| message.content.clone())
            .collect();
        assert_eq!(
            contents,
            [
                "first question",
                "first answer",
                "second question",
                "second answer"
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn context_reset_survives_durable_reopen() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            let turn = editor.begin_ai_runtime_turn("old question").unwrap();
            editor
                .ai_state
                .agent_runtime
                .append_agent_message(&turn, "old answer")
                .unwrap();
            editor.ai_state.agent_runtime.complete_turn(&turn).unwrap();
            editor
                .conversation_mut()
                .unwrap()
                .append_user_message("old question".into());
            editor
                .conversation_mut()
                .unwrap()
                .append_assistant_message("old answer".into(), "Agent".into());
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = "/clear".into();
            chat.input_cursor = chat.input.len();
            editor.submit_ai_chat_message().unwrap();
            assert!(editor.ai_chat_messages().is_empty());
        }

        let mut reopened = durable_editor(&file, layout);
        reopened.open_ai_chat(ChatOpts::default()).unwrap();
        assert!(reopened.ai_chat_messages().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_same_chat_elsewhere_does_not_interrupt_active_run() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let mut first = durable_editor(&file, layout.clone());
        first.set_ai_conversation_resume_enabled(false);
        first.open_ai_chat(ChatOpts::default()).unwrap();
        let turn = first.begin_ai_runtime_turn("unfinished").unwrap();
        let first_run = turn.run_id.clone();

        let mut second = durable_editor(&file, layout.clone());
        second.set_ai_conversation_resume_enabled(false);
        second.open_ai_chat(ChatOpts::default()).unwrap();
        let second_run = second
            .ai_state
            .durable_chat_bindings
            .get(&second.ai_chat_conversation_key())
            .unwrap()
            .binding
            .run_id
            .clone();

        assert_ne!(first_run, second_run);
        first
            .ai_state
            .agent_runtime
            .append_agent_message(&turn, "still running")
            .unwrap();
        first.ai_state.agent_runtime.complete_turn(&turn).unwrap();
        let first_events = first
            .ai_state
            .durable_runs
            .as_ref()
            .unwrap()
            .store
            .events(&first_run)
            .unwrap();
        assert!(first_events.iter().any(|event| matches!(
            &event.kind,
            EventKind::Message(message) if message.content == "still running"
        )));

        let catalog = crate::run_log::RunCatalog::open(&layout).unwrap();
        let discovered = catalog
            .open_conversation(
                second
                    .ai_state
                    .durable_chat_bindings
                    .get(&second.ai_chat_conversation_key())
                    .unwrap()
                    .binding
                    .key
                    .clone(),
                crate::run_log::BranchId::new(),
            )
            .unwrap();
        // The second open never displaces discovery of the run that is still
        // being written; its own run stays process-local.
        assert_eq!(discovered.run_id, first_run);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_does_not_recover_or_mutate_a_possibly_active_run() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let mut first = durable_editor(&file, layout.clone());
        first.set_ai_conversation_resume_enabled(false);
        first.open_ai_chat(ChatOpts::default()).unwrap();
        let turn = first.begin_ai_runtime_turn("still working").unwrap();
        let first_run = turn.run_id.clone();
        let event_count_before = first
            .ai_state
            .durable_runs
            .as_ref()
            .unwrap()
            .store
            .events(&first_run)
            .unwrap()
            .len();

        let mut second = durable_editor(&file, layout.clone());
        second.open_ai_chat(ChatOpts::default()).unwrap();
        let second_run = second
            .ai_state
            .durable_chat_bindings
            .get(&second.ai_chat_conversation_key())
            .unwrap()
            .binding
            .run_id
            .clone();
        assert_ne!(first_run, second_run);
        assert!(second
            .status_message()
            .contains("may still be active; started an independent chat"));
        assert_eq!(
            first
                .ai_state
                .durable_runs
                .as_ref()
                .unwrap()
                .store
                .events(&first_run)
                .unwrap()
                .len(),
            event_count_before
        );

        first
            .ai_state
            .agent_runtime
            .append_agent_message(&turn, "finished safely")
            .unwrap();
        first.ai_state.agent_runtime.complete_turn(&turn).unwrap();

        // The possibly-active run keeps its binding: once its owner is gone
        // a later resume can still discover and restore it.
        let key = second
            .ai_state
            .durable_chat_bindings
            .get(&second.ai_chat_conversation_key())
            .unwrap()
            .binding
            .key
            .clone();
        let discovered = crate::run_log::RunCatalog::open(&layout)
            .unwrap()
            .open_conversation(key, crate::run_log::BranchId::new())
            .unwrap();
        assert_eq!(discovered.run_id, first_run);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_recovers_and_restores_a_run_whose_owner_provably_died() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let crashed_run = {
            let mut editor = durable_editor(&file, layout.clone());
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            let turn = editor.begin_ai_runtime_turn("crashed question").unwrap();
            editor
                .ai_state
                .agent_runtime
                .append_agent_message(&turn, "partial answer")
                .unwrap();
            // The process "crashes" here: the turn never becomes terminal.
            turn.run_id
        };

        // Rewrite the recorded owner marker so the writer is provably dead:
        // the PID exists (this test process) but its start time can never
        // match the marker.
        crate::run_log::RunCatalog::open(&layout)
            .unwrap()
            .register_run_owner(&crashed_run, std::process::id(), Some(1))
            .unwrap();

        let mut resumed = durable_editor(&file, layout);
        resumed.open_ai_chat(ChatOpts::default()).unwrap();
        let restored = resumed
            .ai_state
            .durable_chat_bindings
            .get(&resumed.ai_chat_conversation_key())
            .unwrap()
            .binding
            .clone();
        assert_eq!(restored.run_id, crashed_run);
        assert!(resumed.status_message().contains("Recovered"));
        let messages = resumed.conversation().unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "crashed question");
        assert_eq!(messages[1].content, "partial answer");

        // The crashed turn was sealed append-only rather than rewritten.
        let events = resumed
            .ai_state
            .durable_runs
            .as_ref()
            .unwrap()
            .store
            .events(&crashed_run)
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnLifecycle(lifecycle)
                if lifecycle.state == crate::run_log::TurnLifecycleState::Interrupted
        )));

        // The recovered conversation accepts new turns on the same run.
        let turn = resumed.begin_ai_runtime_turn("continue here").unwrap();
        assert_eq!(turn.run_id, crashed_run);
        resumed.ai_state.agent_runtime.complete_turn(&turn).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_history_starts_a_fresh_chat_and_preserves_the_broken_run() {
        let (_repository, file) = repository_file();
        let storage = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(storage.path().join("runs"));
        let mut editor = durable_editor(&file, layout.clone());
        let snapshot =
            RepositorySnapshot::capture(&file, crate::run_log::RepositoryId::new()).unwrap();
        let worktree = snapshot.local_paths.workdir.unwrap();
        let services = editor.ai_state.durable_runs.as_ref().unwrap();
        let store = services.store.clone();
        let repository = services
            .catalog
            .register_repository(RepositoryRegistration {
                common_git_dir: snapshot
                    .local_paths
                    .common_git_dir
                    .unwrap_or(snapshot.local_paths.git_dir),
                worktree_aliases: vec![worktree.clone()],
            })
            .unwrap();
        let binding = services
            .catalog
            .open_conversation(
                ConversationKey {
                    repository_id: repository.repository_id,
                    scope: conversation_scope(Some(editor.buffer()), &worktree).unwrap(),
                    logical_name: "chat".into(),
                },
                crate::run_log::BranchId::new(),
            )
            .unwrap();
        services
            .store
            .append(crate::run_log::NewRunEvent::new(
                binding.run_id.clone(),
                crate::run_log::EventActor::User,
                EventKind::Unknown {
                    name: "corrupt_test_history".into(),
                    payload: serde_json::json!({}),
                },
            ))
            .unwrap();
        let broken_run_id = binding.run_id;

        let buffer_id = editor.buffer().id();
        editor.prepare_durable_ai_chat(buffer_id, "chat").unwrap();
        let fresh_binding = editor
            .ai_state
            .durable_chat_bindings
            .get(&(buffer_id, "chat".to_string()))
            .unwrap()
            .binding
            .clone();
        assert_ne!(fresh_binding.run_id, broken_run_id);
        assert_eq!(store.events(&broken_run_id).unwrap().len(), 1);
        assert!(store.events(&fresh_binding.run_id).unwrap().is_empty());
        assert!(editor.durable_ai_mutations_available());

        let catalog = crate::run_log::RunCatalog::open(&layout).unwrap();
        assert_eq!(
            catalog
                .open_conversation(fresh_binding.key.clone(), crate::run_log::BranchId::new())
                .unwrap(),
            fresh_binding
        );

        let turn = editor.begin_ai_runtime_turn("start fresh").unwrap();
        assert_eq!(turn.run_id, fresh_binding.run_id);
    }
}
