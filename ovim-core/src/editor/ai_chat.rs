use crate::ai::chat_types::{ChatOpts, ConversationTree};
use crate::buffer::BufferId;
use crate::mode::Mode;
use anyhow::Result;

use super::ai_chat_state::AiChatState;
use super::Editor;

impl Editor {
    // -----------------------------------------------------------------
    // Open / Close
    // -----------------------------------------------------------------

    /// Open or resume an AI chat panel.
    pub fn open_ai_chat(&mut self, opts: ChatOpts) -> Result<()> {
        if self
            .ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.opts.name == opts.name)
        {
            let mode_before = self.mode();
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.mode_before_chat = mode_before;
            }
            self.set_mode(Mode::AiChat);
            self.maybe_prompt_codex_auth_on_chat_open();
            self.maybe_prompt_exa_on_chat_open();
            return Ok(());
        }

        // Switching to another named conversation replaces the live panel.
        // Its projected message history remains stored under its own key.
        if self.ai_state.chat.is_some() {
            self.discard_active_ai_chat("chat replaced");
        }
        let buffer_id = self.buffer().id();
        let mode_before = self.mode();

        if let Err(error) = self.prepare_durable_ai_chat(buffer_id, &opts.name) {
            self.set_status_message(format!(
                "Durable AI history unavailable; agent edits are disabled: {error}"
            ));
        }

        // Ensure conversation exists
        let key = (buffer_id, opts.name.clone());
        self.ai_state.conversations.entry(key.clone()).or_default();

        // Send initial message if provided and conversation is empty
        let initial = opts.initial_message.clone();
        let buffer_clean = !self.buffer().is_modified();
        let branch_generation = self
            .ai_state
            .conversations
            .get(&key)
            .map(ConversationTree::branch_generation)
            .unwrap_or_default();
        let mut chat = AiChatState::new(opts, buffer_id, mode_before);
        let runtime_locator = self
            .ai_state
            .durable_chat_bindings
            .get(&key)
            .map(|binding| binding.locator.clone())
            .unwrap_or_else(|| {
                crate::agent_runtime::ConversationLocator(format!(
                    "buffer:{buffer_id}:conversation:{}",
                    chat.opts.name
                ))
            });
        chat.runtime_branch = self
            .ai_state
            .agent_runtime
            .selected_branch(&runtime_locator)
            .map(|(locator, _)| locator.clone())
            .unwrap_or_else(|| {
                crate::agent_runtime::BranchLocator(format!("branch-{branch_generation}"))
            });
        chat.buffer_was_clean_at_chat_start = buffer_clean;
        self.ai_state.chat = Some(chat);
        self.set_mode(Mode::AiChat);
        self.maybe_prompt_no_repo_session_folder_access_on_chat_open();
        self.maybe_prompt_codex_auth_on_chat_open();
        self.maybe_prompt_exa_on_chat_open();

        if let Some(msg) = initial {
            if let Some(conv) = self.conversation() {
                if conv.is_empty() && !msg.is_empty() {
                    // Will be handled as if user typed and submitted
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.input = msg;
                        chat.input_cursor = chat.input.len();
                    }
                }
            }
        }

        Ok(())
    }

    /// Hide the AI chat panel without clearing or interrupting it.
    pub fn close_ai_chat(&mut self) {
        self.render_cache.ai_chat_text_selection = None;
        self.render_cache.ai_chat_text_selecting = false;
        let mode_before = self
            .ai_state
            .chat
            .as_ref()
            .map(|chat| chat.mode_before_chat);
        if let Some(mode) = mode_before {
            self.set_mode(mode);
        }
    }

    /// Stop the active AI round without hiding the panel or discarding chat state.
    pub fn cancel_ai_chat_generation(&mut self) -> bool {
        if !self.ai_chat_has_pending_work() {
            return false;
        }

        let model_name = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_job.as_ref())
            .map(|job| job.model_name.clone())
            .unwrap_or_default();
        let had_agent_work = self.ai_state.chat.as_ref().is_some_and(|chat| {
            chat.waiting
                || chat.pending_job.is_some()
                || chat.pending_tool_approval.is_some()
                || chat.pending_auto_mode_classification.is_some()
                || chat.pending_shell_execution.is_some()
                || chat.pending_web_execution.is_some()
                || chat.pending_subagent_control.is_some()
                || chat.pending_code_explanation.is_some()
        });

        self.flush_ai_runtime_stream_segments();
        self.commit_partial_streaming(&model_name);

        let (
            pending_job,
            pending_approval,
            pending_classification,
            pending_shell,
            pending_web,
            pending_subagent,
            pending_explanation,
        ) = {
            let chat = self.ai_state.chat.as_mut().expect("pending chat exists");
            (
                chat.pending_job.take(),
                chat.pending_tool_approval.take(),
                chat.pending_auto_mode_classification.take(),
                chat.pending_shell_execution.take(),
                chat.pending_web_execution.take(),
                chat.pending_subagent_control.take(),
                chat.pending_code_explanation.take(),
            )
        };

        if let Some(job) = pending_job {
            job.task.abort();
        }
        if let Some(pending) = pending_approval {
            if let (Some(turn), Some(tool)) =
                (pending.dynamic_turn.as_ref(), pending.runtime_tool.as_ref())
            {
                if let Err(error) =
                    self.ai_state
                        .agent_runtime
                        .fail_tool(turn, tool, "cancelled by user")
                {
                    crate::log_warn!("agent_runtime", "failed to cancel pending tool: {error}");
                }
            } else if let (Some(turn), Some(tool)) =
                (self.active_ai_runtime_turn(), pending.runtime_tool.as_ref())
            {
                if let Err(error) =
                    self.ai_state
                        .agent_runtime
                        .fail_tool(&turn, tool, "cancelled by user")
                {
                    crate::log_warn!("agent_runtime", "failed to cancel pending tool: {error}");
                }
            }
            match pending.dynamic_response {
                Some(response) => {
                    let _ = response.send(Err("cancelled by user".into()));
                }
                None => {
                    // Batch approvals pause after the assistant tool_use blocks
                    // were committed; close out the paused call and the rest of
                    // the batch so the next provider request stays well-formed.
                    let mut unresolved = vec![pending.tool_call.clone()];
                    unresolved.extend(pending.remaining_tool_calls);
                    self.append_synthetic_tool_results(&unresolved, "Execution cancelled");
                }
            }
        }
        if let Some(pending) = pending_classification {
            if let Err(error) = self.ai_state.agent_runtime.fail_tool(
                &pending.runtime_turn,
                &pending.runtime_tool,
                "cancelled by user",
            ) {
                crate::log_warn!("agent_runtime", "failed to cancel classified tool: {error}");
            }
            let _ = pending
                .dynamic_response
                .send(Err("cancelled by user".into()));
        }
        if let Some(pending) = pending_shell {
            // Aborting a started `spawn_blocking` task does not stop it; the
            // command itself must be killed or it keeps mutating the workspace
            // after the UI reports the cancellation.
            pending.kill.cancel();
            pending.task.abort();
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.retire_shell_transcript(
                    &pending.tool_call.id,
                    super::ai_chat_state::ShellTranscriptPhase::Interrupted,
                );
            }
            let (runtime_turn, runtime_tool, response, unresolved) = match pending.continuation {
                super::ai_chat_state::ShellExecutionContinuation::Dynamic {
                    runtime_turn,
                    runtime_tool,
                    response,
                } => (
                    Some(runtime_turn),
                    Some(runtime_tool),
                    Some(response),
                    Vec::new(),
                ),
                super::ai_chat_state::ShellExecutionContinuation::Batch {
                    runtime_turn,
                    runtime_tool,
                    remaining_tool_calls,
                    ..
                } => {
                    // The tool_use blocks for this shell call and the rest of
                    // the batch are already committed; close them out so the
                    // next provider request stays well-formed.
                    let mut unresolved = vec![pending.tool_call.clone()];
                    unresolved.extend(remaining_tool_calls);
                    (runtime_turn, runtime_tool, None, unresolved)
                }
            };
            if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref()) {
                if let Err(error) = self.ai_state.agent_runtime.mark_tool_outcome_unknown(
                    turn,
                    tool,
                    "cancelled by user before the shell result was observed",
                ) {
                    crate::log_warn!("agent_runtime", "failed to cancel shell tool: {error}");
                }
            }
            if let Some(response) = response {
                let _ = response.send(Err("cancelled by user".into()));
            }
            self.append_synthetic_tool_results(&unresolved, "Execution cancelled");
        }
        if let Some(pending) = pending_web {
            pending.task.abort();
            if let (Some(turn), Some(tool)) =
                (pending.runtime_turn.as_ref(), pending.runtime_tool.as_ref())
            {
                if let Err(error) =
                    self.ai_state
                        .agent_runtime
                        .fail_tool(turn, tool, "cancelled by user")
                {
                    crate::log_warn!("agent_runtime", "failed to cancel web tool: {error}");
                }
            }
            // Web execution only parks from the batch path, so this call and
            // the remainder of the batch are already committed as tool_use
            // blocks; close them out.
            let mut unresolved = vec![pending.tool_call.clone()];
            unresolved.extend(pending.remaining_tool_calls);
            self.append_synthetic_tool_results(&unresolved, "Execution cancelled");
        }
        if let Some(pending) = pending_subagent {
            pending.task.abort();
            match pending.continuation {
                super::ai_chat_state::SubagentControlContinuation::Dynamic {
                    runtime_tool,
                    runtime_turn,
                    response,
                } => {
                    if let Err(error) = self.ai_state.agent_runtime.fail_tool(
                        &runtime_turn,
                        &runtime_tool,
                        "cancelled by user",
                    ) {
                        crate::log_warn!(
                            "agent_runtime",
                            "failed to cancel delegated-agent control: {error}"
                        );
                    }
                    let _ = response.send(Err("cancelled by user".into()));
                }
                super::ai_chat_state::SubagentControlContinuation::Batch {
                    runtime_tool,
                    runtime_turn,
                    remaining_tool_calls,
                    ..
                } => {
                    if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref())
                    {
                        if let Err(error) =
                            self.ai_state
                                .agent_runtime
                                .fail_tool(turn, tool, "cancelled by user")
                        {
                            crate::log_warn!(
                                "agent_runtime",
                                "failed to cancel delegated-agent control: {error}"
                            );
                        }
                    }
                    let mut unresolved = vec![pending.tool_call];
                    unresolved.extend(remaining_tool_calls);
                    self.append_synthetic_tool_results(&unresolved, "Execution cancelled");
                }
            }
        }
        if let Some(pending) = pending_explanation {
            self.discard_code_explanation_presentation_buffer(
                pending.presentation_buffer_id,
                pending.original_active_buffer_id,
            );
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.active_buffer_id = pending.original_active_buffer_id;
            }
            self.ai_state.active_selection = None;
            match pending.continuation {
                None => {}
                Some(super::ai_chat_state::CodeExplanationContinuation::Dynamic {
                    runtime_tool,
                    runtime_turn,
                    response,
                }) => {
                    if let Err(error) = self.ai_state.agent_runtime.fail_tool(
                        &runtime_turn,
                        &runtime_tool,
                        "cancelled by user",
                    ) {
                        crate::log_warn!(
                            "agent_runtime",
                            "failed to cancel code walkthrough: {error}"
                        );
                    }
                    let _ = response.send(Err("cancelled by user".into()));
                }
                Some(super::ai_chat_state::CodeExplanationContinuation::Batch {
                    runtime_tool,
                    runtime_turn,
                    remaining_tool_calls,
                    ..
                }) => {
                    if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref())
                    {
                        if let Err(error) =
                            self.ai_state
                                .agent_runtime
                                .fail_tool(turn, tool, "cancelled by user")
                        {
                            crate::log_warn!(
                                "agent_runtime",
                                "failed to cancel code walkthrough: {error}"
                            );
                        }
                    }
                    // The walkthrough call and the rest of its batch are
                    // committed tool_use blocks; close them out.
                    let mut unresolved = vec![pending.tool_call.clone()];
                    unresolved.extend(remaining_tool_calls);
                    self.append_synthetic_tool_results(&unresolved, "Execution cancelled");
                }
                Some(super::ai_chat_state::CodeExplanationContinuation::Replay) => {}
            }
        }

        self.ai_runtime_interrupt_turn("cancelled by user");
        self.clear_streaming_state();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.pending_no_repo_folder_approval = None;
        }
        if had_agent_work {
            if let Some(conv) = self.conversation_mut() {
                conv.append_error("Generation stopped by user.".to_string());
            }
            self.set_status_message("AI generation stopped".to_string());
        } else {
            self.set_status_message("AI folder access prompt cancelled".to_string());
        }
        true
    }

    /// Interrupt only the active shell tool, leaving its agent turn alive so
    /// the resulting failure can be observed and the agent can continue.
    pub fn interrupt_ai_shell_process(&mut self, force: bool) -> bool {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        let Some(pending) = chat.pending_shell_execution.as_ref() else {
            return false;
        };
        let tool_call_id = pending.tool_call.id.clone();
        if chat
            .shell_transcripts
            .get(&tool_call_id)
            .is_some_and(|transcript| {
                matches!(
                    transcript.phase,
                    super::ai_chat_state::ShellTranscriptPhase::CapturingChanges
                        | super::ai_chat_state::ShellTranscriptPhase::Succeeded
                        | super::ai_chat_state::ShellTranscriptPhase::Failed
                        | super::ai_chat_state::ShellTranscriptPhase::Interrupted
                        | super::ai_chat_state::ShellTranscriptPhase::OutcomeUnknown
                        | super::ai_chat_state::ShellTranscriptPhase::Archived
                )
            })
        {
            self.set_status_message("Shell process has already exited");
            return false;
        }
        if force {
            pending.kill.cancel();
        } else {
            pending.kill.interrupt();
        }
        if let Some(transcript) = chat.shell_transcripts.get_mut(&tool_call_id) {
            transcript.phase = super::ai_chat_state::ShellTranscriptPhase::InterruptRequested;
        }
        self.set_status_message(if force {
            "Force-stopping agent shell program"
        } else {
            "Interrupting agent shell program"
        });
        true
    }

    /// Permanently discard the live panel state when replacing conversations.
    fn discard_active_ai_chat(&mut self, reason: &str) {
        if let Some(job) = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_job.as_ref())
        {
            job.task.abort();
        }
        if let Some(web) = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_web_execution.as_ref())
        {
            web.task.abort();
        }
        if let Some(shell) = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_shell_execution.as_ref())
        {
            shell.kill.cancel();
            shell.task.abort();
        }
        self.ai_runtime_interrupt_turn(reason);
        if let Some(mut chat) = self.ai_state.chat.take() {
            chat.pending_job.take();
        }
    }

    // -----------------------------------------------------------------
    // Context profile
    // -----------------------------------------------------------------

    pub fn ai_chat_context_profile(&self, context: &str) -> Option<String> {
        // Look up in contexts table first
        if let Some(profile) = self.ai_state.config.contexts.get(context) {
            if self.ai_state.config.profiles.contains_key(profile) {
                return Some(profile.clone());
            }
        }
        // Fallback to active profile
        Some(self.ai_state.active_profile.clone())
    }

    /// Effective profile currently used by the active chat session.
    pub fn ai_chat_effective_profile(&self) -> String {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.clone())
            .unwrap_or_else(|| self.ai_state.active_profile.clone())
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    pub(crate) fn ai_chat_conversation_key(&self) -> (BufferId, String) {
        if let Some(chat) = &self.ai_state.chat {
            (chat.origin_buffer_id, chat.opts.name.clone())
        } else {
            (self.buffer().id(), "chat".to_string())
        }
    }

    /// Shorthand for getting the current conversation (read-only).
    pub fn conversation(&self) -> Option<&ConversationTree> {
        let key = self.ai_chat_conversation_key();
        self.ai_state.conversations.get(&key)
    }

    /// Shorthand for getting the current conversation (mutable).
    pub(crate) fn conversation_mut(&mut self) -> Option<&mut ConversationTree> {
        let key = self.ai_chat_conversation_key();
        self.ai_state.conversations.get_mut(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::{ChatOpts, ChatRole, NodeId, StreamChunk, ToolCallInfo};
    use crate::buffer::Buffer;
    use crate::run_log::{EventKind, TurnLifecycleState};

    fn open_test_chat(editor: &mut Editor) {
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
    }

    fn attach_pending_runtime_job(
        editor: &mut Editor,
        turn: crate::agent_runtime::PendingTurnRef,
        branch_generation: u64,
    ) -> tokio::task::AbortHandle {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let abort_handle = task.abort_handle();
        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn),
            branch_generation,
            steer_tx: None,
        });
        chat.waiting = true;
        abort_handle
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_chat_keeps_provider_and_live_state_running() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("inspect").unwrap();
        let run_id = turn.run_id.clone();
        let abort_handle = attach_pending_runtime_job(&mut editor, turn, 0);
        editor.ai_state.chat.as_mut().unwrap().input = "follow up".into();

        editor.close_ai_chat();
        tokio::task::yield_now().await;

        assert!(!abort_handle.is_finished());
        assert_ne!(editor.mode(), Mode::AiChat);
        let chat = editor.ai_state.chat.as_ref().expect("hidden chat retained");
        assert_eq!(chat.input, "follow up");
        assert!(chat.pending_job.is_some());
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(!matches!(
            &events.last().unwrap().kind,
            EventKind::TurnLifecycle(event) if event.state == TurnLifecycleState::Interrupted
        ));

        editor.discard_active_ai_chat("test cleanup");
    }

    #[test]
    fn activity_tracks_runtime_ownership_even_without_waiting_flag() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        assert_eq!(
            editor.ai_chat_activity(),
            crate::editor::AiChatActivity::Idle
        );

        let turn = editor.begin_ai_runtime_turn("inspect").unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn));
        chat.waiting = false;

        assert_eq!(
            editor.ai_chat_activity(),
            crate::editor::AiChatActivity::Inference
        );
        assert!(editor.ai_chat_has_pending_work());
        editor.ai_runtime_interrupt_turn("test cleanup");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_generation_stops_provider_but_preserves_chat() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("inspect").unwrap();
        let run_id = turn.run_id.clone();
        let abort_handle = attach_pending_runtime_job(&mut editor, turn, 0);
        {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = "keep this draft".into();
            chat.streaming_content = Some("Partial answer".into());
        }

        assert!(editor.cancel_ai_chat_generation());
        tokio::task::yield_now().await;

        assert!(abort_handle.is_finished());
        assert_eq!(editor.mode(), Mode::AiChat);
        assert_eq!(editor.ai_chat_input(), "keep this draft");
        assert!(!editor.ai_chat_waiting());
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "Partial answer"));
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "Generation stopped by user."));
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            &events.last().unwrap().kind,
            EventKind::TurnLifecycle(event)
                if event.state == TurnLifecycleState::Interrupted
        ));
    }

    /// Committed tool_use blocks must all have a closing tool_result after a
    /// cancel, or the next provider request is rejected as malformed.
    fn assert_cancelled_tool_results(editor: &Editor, expected_ids: &[&str]) {
        let messages = editor.conversation().unwrap().messages();
        let tool_ids: Vec<_> = messages
            .iter()
            .filter(|m| m.role == ChatRole::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(tool_ids, expected_ids);
        assert!(messages
            .iter()
            .filter(|m| m.role == ChatRole::Tool)
            .all(|m| m.content.contains("Execution cancelled")));
    }

    fn committed_tool_batch(editor: &mut Editor) -> (ToolCallInfo, ToolCallInfo) {
        let first = ToolCallInfo {
            id: "tool-1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "true" }),
        };
        let follow_up = ToolCallInfo {
            id: "tool-2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message_with_tools(
                String::new(),
                "test".into(),
                vec![first.clone(), follow_up.clone()],
            );
        (first, follow_up)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_pending_batch_approval_closes_committed_tool_calls() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (first, follow_up) = committed_tool_batch(&mut editor);
        editor.ai_state.chat.as_mut().unwrap().pending_tool_approval =
            Some(super::super::ai_chat_state::PendingToolApproval {
                tool_call: first,
                reason: "sensitive command".into(),
                runtime_tool: None,
                runtime_tool_started: false,
                remaining_tool_calls: vec![follow_up],
                model_name: "test".into(),
                requested_path: std::path::PathBuf::from("."),
                approval_root: std::path::PathBuf::from("."),
                dynamic_response: None,
                dynamic_turn: None,
            });

        assert!(editor.cancel_ai_chat_generation());

        assert_cancelled_tool_results(&editor, &["tool-1", "tool-2"]);
        editor.close_ai_chat();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_web_execution_closes_committed_tool_calls() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (first, follow_up) = committed_tool_batch(&mut editor);
        let (_result_tx, result_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async {});
        editor.ai_state.chat.as_mut().unwrap().pending_web_execution =
            Some(super::super::ai_chat_state::PendingWebExecution {
                tool_call: first,
                runtime_tool: None,
                runtime_turn: None,
                remaining_tool_calls: vec![follow_up],
                model_name: "test".into(),
                receiver: result_rx,
                task,
            });

        assert!(editor.cancel_ai_chat_generation());

        assert_cancelled_tool_results(&editor, &["tool-1", "tool-2"]);
        editor.close_ai_chat();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_parked_code_explanation_closes_committed_tool_calls() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (first, follow_up) = committed_tool_batch(&mut editor);
        let buffer_id = editor.ai_state.chat.as_ref().unwrap().active_buffer_id;
        editor
            .ai_state
            .chat
            .as_mut()
            .unwrap()
            .pending_code_explanation = Some(super::super::ai_chat_state::PendingCodeExplanation {
            tool_call: first,
            steps: Vec::new(),
            current: 0,
            answer_scroll: 0,
            threads: Vec::new(),
            interaction: super::super::ai_chat_state::CodeExplanationInteraction::Navigating,
            original_active_buffer_id: buffer_id,
            presentation_buffer_id: None,
            continuation: Some(
                super::super::ai_chat_state::CodeExplanationContinuation::Batch {
                    runtime_tool: None,
                    runtime_turn: None,
                    remaining_tool_calls: vec![follow_up],
                    model_name: "test".into(),
                },
            ),
        });

        assert!(editor.cancel_ai_chat_generation());

        assert_cancelled_tool_results(&editor, &["tool-1", "tool-2"]);
        editor.close_ai_chat();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_shell_execution_kills_the_running_command() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let runs = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .unwrap();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        let turn = editor
            .begin_ai_runtime_turn("run the slow command")
            .unwrap();
        let call = ToolCallInfo {
            id: "slow-shell".into(),
            name: "bash".into(),
            arguments: serde_json::json!({
                "command": "touch shell-started; sleep 5; touch cancelled-marker"
            }),
        };
        let tool = editor.ai_runtime_record_tool_intent(&turn, &call).unwrap();
        let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
        editor.execute_dynamic_tool_after_policy(turn, tool, call, response_tx, None, false);

        let kill = editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .as_ref()
            .unwrap()
            .kill
            .clone();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let pid = loop {
            if let Some(pid) = kill.published_child() {
                break pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell child never spawned"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };

        assert!(editor.cancel_ai_chat_generation());

        // The blocking task reaps the killed child; once the pid is gone the
        // command (and its trailing `touch cancelled-marker`) can never run.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell child survived cancellation"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!dir.path().join("cancelled-marker").exists());
    }

    /// True while `pid` names a terminated-but-unreaped (zombie) process.
    /// Used to observe the reap-last ordering: the shell leader must stay a
    /// zombie — reserving its pid and pgid for `killpg` — for as long as
    /// the execution is parked on the output drain.
    #[cfg(unix)]
    fn leader_is_zombie(pid: u32) -> bool {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains('Z'))
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_shell_execution_kills_descendants_after_leader_exits() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let runs = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .unwrap();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        let turn = editor
            .begin_ai_runtime_turn("run the backgrounded command")
            .unwrap();
        let call = ToolCallInfo {
            id: "bg-shell".into(),
            name: "bash".into(),
            arguments: serde_json::json!({
                // The shell leader exits immediately; the backgrounded
                // descendant inherits the output pipes, so the execution
                // stays blocked on the drain long after the leader is gone.
                "command": "(sleep 5; touch cancelled-marker) & echo started"
            }),
        };
        let tool = editor.ai_runtime_record_tool_intent(&turn, &call).unwrap();
        let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
        editor.execute_dynamic_tool_after_policy(turn, tool, call, response_tx, None, false);

        let kill = editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .as_ref()
            .unwrap()
            .kill
            .clone();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let pid = loop {
            if let Some(pid) = kill.published_child() {
                break pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell child never spawned"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        // Wait until the leader has exited. It must remain an un-reaped
        // zombie with its pid still published: the kernel then reserves the
        // pid/pgid, which is what lets cancellation still reach the
        // descendant through `killpg`.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !leader_is_zombie(pid) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell leader never became an un-reaped zombie"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            kill.published_child(),
            Some(pid),
            "pid must stay published while the drain is parked"
        );

        assert!(editor.cancel_ai_chat_generation());

        // The whole process group must die, so the descendant's trailing
        // `touch` can never run.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid as i32), None).is_ok() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell descendants survived cancellation"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // Only the blocking execution reaps the leader, and it does so as
        // its final step — so the zombie disappearing proves the pending
        // execution resolved (and did not leak the zombie).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "cancelled shell execution never resolved (leader still un-reaped)"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!dir.path().join("cancelled-marker").exists());
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_shell_drain_resolves_after_group_kill() {
        let dir = tempfile::tempdir().unwrap();
        let kill = std::sync::Arc::new(super::super::ai_chat_state::ShellKillHandle::default());
        let task_kill = kill.clone();
        let workdir = dir.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            super::super::ai_tool_execution::run_bash_program(
                "(sleep 5; touch cancelled-marker) & echo started",
                &workdir,
                Some(&task_kill),
                None,
            )
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let pid = loop {
            if let Some(pid) = kill.published_child() {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shell child never spawned"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // The leader exits but stays an un-reaped zombie while the
        // backgrounded descendant holds the pipes: the execution is parked
        // on the output drain and reaping is deferred to the very end.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !leader_is_zombie(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "shell leader never became an un-reaped zombie"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !worker.is_finished(),
            "execution finished while the descendant still held the pipes"
        );

        kill.cancel();

        // Cancellation SIGKILLs the group, which closes the pipes and lets
        // the drain (and thus the whole execution) resolve well before the
        // descendant's 5s sleep would have ended.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled shell drain did not resolve promptly"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        worker.join().unwrap();
        // The final reap ran: no zombie leader leaks.
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err(),
            "shell leader was never reaped"
        );
        assert!(!dir.path().join("cancelled-marker").exists());
    }

    #[cfg(unix)]
    #[test]
    fn graceful_shell_interrupt_finishes_without_cancelling_the_execution_task() {
        let dir = tempfile::tempdir().unwrap();
        let kill = std::sync::Arc::new(super::super::ai_chat_state::ShellKillHandle::default());
        let task_kill = kill.clone();
        let workdir = dir.path().to_path_buf();
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = std::thread::spawn(move || {
            super::super::ai_tool_execution::run_bash_program(
                "trap 'exit 130' INT; echo ready; while :; do sleep 1; done",
                &workdir,
                Some(&task_kill),
                Some(progress_tx),
            )
        });

        let is_ready_output = |event: &super::super::ai_chat_state::ShellProgressEvent| {
            matches!(
                event,
                super::super::ai_chat_state::ShellProgressEvent::Output { bytes, .. }
                    if bytes.windows(b"ready".len()).any(|window| window == b"ready")
            )
        };

        // Wait for the script to actually emit "ready" before interrupting,
        // rather than a fixed sleep after spawn. The child pid is published at
        // fork, before the login shell (`-lc`) has sourced its profile and run
        // `echo ready`; on a loaded runner that startup can outlast any fixed
        // delay, so a timed interrupt could land before the trap is even armed
        // and race the "ready" output away entirely.
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !events.iter().any(is_ready_output) {
            assert!(
                std::time::Instant::now() < deadline,
                "shell never emitted ready output"
            );
            while let Ok(event) = progress_rx.try_recv() {
                events.push(event);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        kill.interrupt();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "SIGINT did not stop the shell process group"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            worker.join().unwrap(),
            crate::ai::tools::ToolResult::Error(_)
        ));
        while let Ok(event) = progress_rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            super::super::ai_chat_state::ShellProgressEvent::Spawned { .. }
        )));
        assert!(events.iter().any(is_ready_output));
    }

    // macOS-only: the killpg(pgid, 0) probe below detects the "group empty
    // while leader is an un-reaped zombie" window, and only BSD/macOS drop
    // zombies from process-group signalling. Linux keeps the zombie leader
    // signalable, so the probe never observes the window there (the kernel
    // also keeps the pgid reserved, so the underlying hazard cannot occur).
    #[cfg(target_os = "macos")]
    #[test]
    fn cancelled_drain_with_group_escaped_descendant_resolves_without_stray_signals() {
        // Needs an interpreter that can setsid() away from the shell's
        // process group while keeping the inherited pipe ends open.
        let perl_available = std::process::Command::new("perl")
            .args(["-e", "1"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !perl_available {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let kill = std::sync::Arc::new(super::super::ai_chat_state::ShellKillHandle::default());
        let task_kill = kill.clone();
        let workdir = dir.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            // The perl descendant leaves the process group via setsid() but
            // keeps the pipes. Once the shell leader exits, the group has NO
            // living members — the window where the pgid would be reusable
            // if the leader were reaped. sleep 15 so only the bounded
            // post-cancel drain (a few seconds) can explain a prompt return.
            super::super::ai_tool_execution::run_bash_program(
                "perl -e 'use POSIX (); POSIX::setsid(); sleep 15' & echo started",
                &workdir,
                Some(&task_kill),
                None,
            )
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let pid = loop {
            if let Some(pid) = kill.published_child() {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shell child never spawned"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // The un-reaped zombie leader is what keeps the pid/pgid reserved
        // even though the group has no living members left.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !leader_is_zombie(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "shell leader never became an un-reaped zombie"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Wait until perl has actually setsid() away: only then is the group
        // empty (killpg probe fails) while the pipes are still held — the
        // exact window where a reaped leader's pgid would be reusable.
        // Cancelling earlier would kill perl while it is still in the group
        // and skip the scenario under test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid as i32), None).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "descendant never left the shell's process group"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !worker.is_finished(),
            "execution finished while the escaped descendant still held the pipes"
        );

        // killpg on the reserved-but-empty group fails with ESRCH harmlessly;
        // the requirement is that nothing else ever gets signalled and the
        // execution still resolves via the bounded drain.
        kill.cancel();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled drain with an escaped descendant did not resolve"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        worker.join().unwrap();
        // The final reap ran: no zombie leader leaks.
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err(),
            "shell leader was never reaped"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn submitting_new_turn_clears_stale_chat_notice() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor.set_status_message("Queued message moved back to the composer");
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "new request".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().unwrap();

        assert_eq!(editor.status_message(), "");
        editor.cancel_ai_chat_generation();
    }

    #[test]
    fn effective_profile_controls_optional_tool_call_limit() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        assert_eq!(editor.ai_chat_tool_call_limit(), None);

        editor
            .ai_state
            .config
            .profiles
            .get_mut(crate::ai::PROFILE_LOCAL)
            .unwrap()
            .agent_loop
            .max_tool_calls = Some(75);

        // ChatOpts has no explicit profile, so this also proves the active
        // profile is used rather than a hard-coded fallback.
        assert_eq!(editor.ai_chat_tool_call_limit(), Some(75));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reopening_chat_resumes_live_state_and_preserves_underlying_mode() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("inspect").unwrap();
        let abort_handle = attach_pending_runtime_job(&mut editor, turn, 0);
        editor.ai_state.chat.as_mut().unwrap().input = "still here".into();

        editor.close_ai_chat();
        assert_ne!(editor.mode(), Mode::AiChat);

        open_test_chat(&mut editor);
        tokio::task::yield_now().await;

        assert!(!abort_handle.is_finished());
        assert_eq!(editor.mode(), Mode::AiChat);
        assert_eq!(editor.ai_chat_input(), "still here");
        editor.close_ai_chat();
        assert_ne!(editor.mode(), Mode::AiChat);

        editor.discard_active_ai_chat("test cleanup");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_provider_branch_is_aborted_before_output_is_applied() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("inspect").unwrap();
        let run_id = turn.run_id.clone();
        let abort_handle = attach_pending_runtime_job(&mut editor, turn, 0);
        {
            let conv = editor.conversation_mut().unwrap();
            conv.append_user_message("root".into());
            let root = conv.active_leaf_id().unwrap();
            conv.append_assistant_message("old branch".into(), "test".into());
            conv.fork_from(root);
        }

        assert!(editor.poll_pending_ai_chat_job());
        tokio::task::yield_now().await;

        assert!(abort_handle.is_finished());
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            &events.last().unwrap().kind,
            EventKind::TurnLifecycle(event)
                if event.state == TurnLifecycleState::Interrupted
        ));
        assert!(editor
            .conversation()
            .unwrap()
            .messages()
            .iter()
            .any(|message| message.content.contains("Discarded stale response")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn dynamic_tool_events_are_terminal_before_codex_receives_result() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("check diagnostics").unwrap();
        let run_id = turn.run_id.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let abort_handle = task.abort_handle();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn.clone()));
        editor.ai_state.chat.as_mut().unwrap().streaming_content = Some(String::new());
        editor.ai_state.chat.as_mut().unwrap().pending_job =
            Some(super::super::ai_chat_state::PendingAiChatJob {
                receiver: rx,
                task,
                profile_name: "test".into(),
                model_name: "test".into(),
                turn: Box::new(turn),
                branch_generation: 0,
                steer_tx: None,
            });

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tx.send(StreamChunk::Content("Before tool. ".into()))
            .unwrap();
        tx.send(StreamChunk::DynamicToolRequest {
            call: ToolCallInfo {
                id: "provider-call-1".into(),
                name: "read_diagnostics".into(),
                arguments: serde_json::json!({}),
            },
            response: result_tx,
        })
        .unwrap();

        assert!(editor.poll_pending_ai_chat_job());
        let events_before_provider_result = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            &events_before_provider_result.last().unwrap().kind,
            EventKind::ToolResult(_)
        ));
        let pre_tool_message = events_before_provider_result
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    EventKind::Message(crate::run_log::MessageEvent {
                        role: crate::run_log::MessageRole::Agent,
                        content,
                    }) if content == "Before tool. "
                )
            })
            .unwrap();
        let tool_intent = events_before_provider_result
            .iter()
            .position(|event| matches!(event.kind, EventKind::ToolIntent(_)))
            .unwrap();
        assert!(pre_tool_message < tool_intent);
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.role == ChatRole::Tool
                && message.tool_call_id.as_deref() == Some("provider-call-1")));
        let _provider_result = result_rx.await.unwrap();

        tx.send(StreamChunk::Content("After tool.".into())).unwrap();
        tx.send(StreamChunk::Done).unwrap();
        assert!(editor.poll_pending_ai_chat_job());
        abort_handle.abort();

        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            &events.last().unwrap().kind,
            EventKind::TurnLifecycle(event)
                if event.state == TurnLifecycleState::Completed
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn dynamic_code_walkthrough_blocks_provider_until_user_finishes() {
        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        let file = repo.path().join("main.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        let profile = editor.ai_state.active_profile.clone();
        editor
            .ai_state
            .config
            .profiles
            .get_mut(&profile)
            .unwrap()
            .scope
            .files = crate::ai::FileScope::Project;
        editor.set_viewport_height(24);

        let turn = editor
            .begin_ai_runtime_turn("explain the entry point")
            .unwrap();
        let run_id = turn.run_id.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let abort_handle = task.abort_handle();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn),
            branch_generation: 0,
            steer_tx: None,
        });

        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        tx.send(StreamChunk::DynamicToolRequest {
            call: ToolCallInfo {
                id: "walkthrough-call".into(),
                name: "explain_with_codebase".into(),
                arguments: serde_json::json!({
                    "steps": [{
                        "path": "main.rs",
                        "start_line": 1,
                        "end_line": 3,
                        "comment": "This is the executable entry point."
                    }]
                }),
            },
            response: response_tx,
        })
        .unwrap();

        assert!(editor.poll_pending_ai_chat_job());
        assert_eq!(
            editor.ai_chat_activity(),
            crate::editor::AiChatActivity::WaitingCodeExplanation
        );
        assert!(editor.ai_chat_has_pending_code_explanation());
        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::ToolStarted(_)
        ));

        assert!(editor.finish_code_explanation(false));
        let provider_result = response_rx.await.unwrap().unwrap();
        assert!(provider_result.contains("completed the walkthrough"));
        assert!(!editor.ai_chat_has_pending_code_explanation());
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::ToolResult(_)
        ));

        abort_handle.abort();
        editor.discard_active_ai_chat("test cleanup");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn auto_mode_unauthorized_deploy_is_sent_to_terra_before_user_escalation() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        editor.ai_state.config.tool_approval_mode = crate::ai::ToolApprovalMode::Auto;
        let turn = editor.begin_ai_runtime_turn("inspect the project").unwrap();
        let run_id = turn.run_id.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let abort_handle = task.abort_handle();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.streaming_content = Some(String::new());
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn),
            branch_generation: 0,
            steer_tx: None,
        });

        let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
        tx.send(StreamChunk::DynamicToolRequest {
            call: ToolCallInfo {
                id: "deploy-call".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "./deploy production"}),
            },
            response: result_tx,
        })
        .unwrap();

        assert!(editor.poll_pending_ai_chat_job());
        assert!(!editor.ai_chat_has_pending_tool_approval());
        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_auto_mode_classification
            .is_some());
        assert!(matches!(
            result_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolIntent(_))));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolStarted(_))));

        // The classifier task may still be connecting to app-server. Dropping
        // its receiver is sufficient here; this test covers routing, while
        // verdict handling is exercised by the focused classifier tests.
        editor
            .ai_state
            .chat
            .as_mut()
            .unwrap()
            .pending_auto_mode_classification = None;
        abort_handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn dynamic_path_tool_uses_the_same_paused_approval_flow() {
        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        let file = repo.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let external = tempfile::tempdir().unwrap();
        let external_file = external.path().join("outside.txt");
        std::fs::write(&external_file, "outside\n").unwrap();

        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        let turn = editor
            .begin_ai_runtime_turn("inspect outside file")
            .unwrap();
        let run_id = turn.run_id.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let abort_handle = task.abort_handle();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn),
            branch_generation: 0,
            steer_tx: None,
        });

        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        tx.send(StreamChunk::DynamicToolRequest {
            call: ToolCallInfo {
                id: "outside-read".into(),
                name: "read_file_at_path".into(),
                arguments: serde_json::json!({"path": external_file}),
            },
            response: response_tx,
        })
        .unwrap();

        assert!(editor.poll_pending_ai_chat_job());
        assert_eq!(
            editor.ai_chat_activity(),
            crate::editor::AiChatActivity::WaitingToolApproval
        );
        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(editor.ai_chat_resolve_pending_tool_approval(false, false));
        assert!(response_rx.await.unwrap().is_err());

        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ToolStarted(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
                .count(),
            1
        );
        abort_handle.abort();
        editor.discard_active_ai_chat("test cleanup");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn completed_agent_items_become_separate_chat_messages() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("work in stages").unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.streaming_content = Some(String::new());
        chat.waiting = true;
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn),
            branch_generation: 0,
            steer_tx: None,
        });

        tx.send(StreamChunk::Thinking("Inspecting first.".into()))
            .unwrap();
        tx.send(StreamChunk::Content("I found the cause.".into()))
            .unwrap();
        tx.send(StreamChunk::AgentMessageComplete).unwrap();
        tx.send(StreamChunk::Content("The fix is verified.".into()))
            .unwrap();
        tx.send(StreamChunk::AgentMessageComplete).unwrap();
        tx.send(StreamChunk::Done).unwrap();

        assert!(editor.poll_pending_ai_chat_job());
        let messages = editor.conversation().unwrap().messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, ChatRole::Thinking);
        assert_eq!(messages[0].content, "Inspecting first.");
        assert_eq!(messages[1].role, ChatRole::Assistant);
        assert_eq!(messages[1].content, "I found the cause.");
        assert_eq!(messages[2].role, ChatRole::Assistant);
        assert_eq!(messages[2].content, "The fix is verified.");
        assert!(!editor.ai_chat_waiting());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_provider_job_is_detached_before_async_tool_work() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("search the web").unwrap();
        attach_pending_runtime_job(&mut editor, turn, 0);

        editor.finish_provider_stream_before_tools();

        assert!(editor.ai_state.chat.as_ref().unwrap().pending_job.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn another_editor_opening_the_chat_does_not_block_an_active_shell_effect() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();
        let file = repo.join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let runs = crate::run_log::RunStorageLayout::new(dir.path().join("runs"));
        let mut editor = Editor::default();
        *editor.ai_state =
            super::super::ai_state::AiState::with_run_storage_layout(runs.clone()).unwrap();
        editor.open_file(&file).unwrap();
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("create the marker").unwrap();
        let call = ToolCallInfo {
            id: "write-marker".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "touch effect-marker"}),
        };
        let tool = editor.ai_runtime_record_tool_intent(&turn, &call).unwrap();
        let first_run = editor
            .ai_state
            .durable_chat_bindings
            .get(&editor.ai_chat_conversation_key())
            .unwrap()
            .binding
            .run_id
            .clone();

        let mut second = Editor::default();
        *second.ai_state = super::super::ai_state::AiState::with_run_storage_layout(runs).unwrap();
        second.open_file(&file).unwrap();
        open_test_chat(&mut second);
        let second_run = second
            .ai_state
            .durable_chat_bindings
            .get(&second.ai_chat_conversation_key())
            .unwrap()
            .binding
            .run_id
            .clone();
        assert_ne!(first_run, second_run);

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        editor.execute_dynamic_tool_after_policy(turn, tool, call, response_tx, None, false);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .is_some()
        {
            editor.poll_pending_ai_chat_job();
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(response_rx.await.unwrap().is_ok());
        assert!(repo.join("effect-marker").exists());
        let events = editor.ai_state.agent_runtime.events(&first_run).unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolStarted(_))));
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolResult(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorized_shell_runs_in_background_while_editor_poll_stays_responsive() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n// second line\n").unwrap();
        let runs = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .unwrap();
        editor.open_file(&file).unwrap();
        editor.set_mode(crate::mode::Mode::Normal);
        open_test_chat(&mut editor);
        let turn = editor.begin_ai_runtime_turn("run the gated check").unwrap();
        let run_id = turn.run_id.clone();
        let shell_quote = |path: &std::path::Path| {
            format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
        };
        let release_gate = dir.path().join("release-gate");
        let agent_marker = dir.path().join("agent-marker");
        let release_gate_arg = shell_quote(&release_gate);
        let command = format!(
            "attempts=0; while [ ! -f {release_gate_arg} ] && [ \"$attempts\" -lt 500 ]; do sleep 0.01; attempts=$((attempts + 1)); done; [ -f {release_gate_arg} ] || exit 124; touch {}; echo completed",
            shell_quote(&agent_marker)
        );
        let call = ToolCallInfo {
            id: "gated-shell".into(),
            name: "bash".into(),
            arguments: serde_json::json!({
                "command": command
            }),
        };
        let tool = editor.ai_runtime_record_tool_intent(&turn, &call).unwrap();
        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();

        let started = std::time::Instant::now();
        editor.execute_dynamic_tool_after_policy(turn, tool, call, response_tx, None, false);
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .is_some());
        assert_eq!(
            editor.ai_chat_activity(),
            super::super::AiChatActivity::RunningShell
        );

        // A live tool belongs to the chat, not to the chat panel. Hiding the
        // panel must return input ownership to the editor while the tool keeps
        // running in the background.
        crate::editor::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Esc, crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.mode(), crate::mode::Mode::Normal);
        crate::editor::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Char('j'), crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.cursor_position().line, 1);
        assert_eq!(
            editor.ai_chat_activity(),
            super::super::AiChatActivity::RunningShell
        );
        assert!(!editor.poll_pending_ai_chat_job());
        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        std::fs::write(release_gate, "go").unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .is_some()
        {
            // Streaming output is itself a visible state change, so a `true`
            // poll result no longer implies the shell has completed.
            editor.poll_pending_ai_chat_job();
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let result = response_rx.await.unwrap().unwrap();
        assert!(result.contains("completed"), "{result}");
        assert!(editor.status_message().is_empty());
        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        let start_index = events
            .iter()
            .position(|event| matches!(event.kind, EventKind::ToolStarted(_)))
            .unwrap();
        let result_index = events
            .iter()
            .position(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .unwrap();
        let mutation_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    EventKind::FileMutation(mutation) if mutation.path == "agent-marker"
                )
            })
            .unwrap();
        assert!(start_index < mutation_index && mutation_index < result_index);
        assert_eq!(
            events[mutation_index].operation_id,
            events[start_index].operation_id
        );
    }

    fn attach_finished_classifier(
        editor: &mut Editor,
        result: Result<crate::ai::auto_mode::ClassifierVerdict, String>,
    ) -> tokio::sync::oneshot::Receiver<Result<String, String>> {
        let turn = editor.begin_ai_runtime_turn("review command").unwrap();
        let call = ToolCallInfo {
            id: "classifier-call".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "custom-tool"}),
        };
        let tool = editor.ai_runtime_record_tool_intent(&turn, &call).unwrap();
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let (classification_tx, classification_rx) = tokio::sync::oneshot::channel();
        classification_tx.send(result).unwrap();
        editor
            .ai_state
            .chat
            .as_mut()
            .unwrap()
            .pending_auto_mode_classification =
            Some(super::super::ai_chat_state::PendingAutoModeClassification {
                tool_call: call,
                runtime_tool: tool,
                runtime_turn: turn,
                dynamic_response: response_tx,
                receiver: classification_rx,
            });
        response_rx
    }

    #[tokio::test]
    async fn classifier_failure_escalates_to_paused_user_approval() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let mut response = attach_finished_classifier(&mut editor, Err("protocol failed".into()));

        assert_eq!(editor.ai_chat_attention_generation(), 0);
        assert!(editor.poll_pending_auto_mode_classification());
        assert!(editor.ai_chat_has_pending_tool_approval());
        assert_eq!(editor.ai_chat_attention_generation(), 1);
        assert!(matches!(
            response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(editor.set_ai_chat_yolo_mode(true));
        assert!(!editor.ai_chat_has_pending_tool_approval());
    }

    #[tokio::test]
    async fn enabling_yolo_releases_pending_terra_review_without_prompt() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let _response = attach_finished_classifier(&mut editor, Err("still reviewing".into()));

        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_auto_mode_classification
            .is_some());
        assert!(editor.set_ai_chat_yolo_mode(true));

        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(chat.pending_auto_mode_classification.is_none());
        assert!(chat.pending_tool_approval.is_none());
        assert!(editor.ai_chat_yolo_mode());
    }

    #[tokio::test]
    async fn classifier_ask_escalates_to_paused_user_approval() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let verdict = crate::ai::auto_mode::ClassifierVerdict::parse_strict(
            r#"{"policy_version":"ovim.auto-mode.v2","decision":"ask","scope":{"project_root":"/repo"},"reason":"the user did not authorize credential access","confidence":0.96,"expiry":{"kind":"after_command"}}"#,
        )
        .unwrap();
        let mut response = attach_finished_classifier(&mut editor, Ok(verdict));

        assert!(editor.poll_pending_auto_mode_classification());
        assert!(editor.ai_chat_has_pending_tool_approval());
        assert!(editor
            .status_message()
            .contains("the user did not authorize credential access"));
        assert!(matches!(
            response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn shell_approval_summary_contains_full_command_and_terra_reason() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor.ai_state.chat.as_mut().unwrap().pending_tool_approval =
            Some(super::super::ai_chat_state::PendingToolApproval {
                tool_call: ToolCallInfo {
                    id: "approval-summary".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({
                        "command": "git diff --check && cargo test\nprintf 'complete\\n'"
                    }),
                },
                reason: "the requested write is not clearly authorized".into(),
                runtime_tool: None,
                runtime_tool_started: false,
                remaining_tool_calls: Vec::new(),
                model_name: "test".into(),
                requested_path: std::path::PathBuf::from("/repo"),
                approval_root: std::path::PathBuf::from("/repo"),
                dynamic_response: None,
                dynamic_turn: None,
            });

        let summary = editor.ai_chat_pending_tool_approval_summary().unwrap();
        assert!(summary.contains("git diff --check && cargo test"));
        assert!(summary.contains("printf 'complete\\n'"));
        assert!(summary.contains("Terra: the requested write is not clearly authorized"));
        assert!(summary.contains("Working directory: /repo"));
    }

    #[tokio::test]
    async fn classifier_deny_returns_terminal_error_without_execution() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let verdict = crate::ai::auto_mode::ClassifierVerdict::parse_strict(
            r#"{"policy_version":"ovim.auto-mode.v2","decision":"deny","scope":{"project_root":"/repo"},"reason":"conflicts with objective","confidence":0.99,"expiry":{"kind":"after_command"}}"#,
        )
        .unwrap();
        let response = attach_finished_classifier(&mut editor, Ok(verdict));

        assert!(editor.poll_pending_auto_mode_classification());
        assert!(!editor.ai_chat_has_pending_tool_approval());
        assert!(response.await.unwrap().is_err());
    }

    fn append_recorded_test_turn(
        editor: &mut Editor,
        user: &str,
        assistant: &str,
    ) -> (NodeId, NodeId, crate::agent_runtime::PendingTurnRef) {
        let turn = editor.begin_ai_runtime_turn(user).unwrap();
        let user_event = turn.initiating_event.caused_by.clone().unwrap();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn.clone()));
        let user_node = editor
            .conversation_mut()
            .unwrap()
            .append_user_message(user.into());
        editor.record_ai_chat_node(user_node, user_event);
        let assistant_event = editor.ai_runtime_append_agent_message(assistant).unwrap();
        let assistant_node = editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message(assistant.into(), "test".into());
        editor.record_ai_chat_node(assistant_node, assistant_event);
        editor.ai_runtime_complete_turn();
        (user_node, assistant_node, turn)
    }

    #[test]
    fn shell_authorization_projection_is_chronological_durable_and_bounded() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        for index in 0..10 {
            append_recorded_test_turn(&mut editor, &format!("instruction {index}"), "ack");
        }
        let context = editor.shell_authorization_context(std::path::Path::new("/repo"));
        assert_eq!(context.explicit_user_instructions.len(), 8);
        assert_eq!(
            context.explicit_user_instructions[0].instruction,
            "instruction 2"
        );
        assert_eq!(
            context.explicit_user_instructions[7].instruction,
            "instruction 9"
        );
        assert!(context
            .explicit_user_instructions
            .iter()
            .all(|authorization| authorization.source_id.starts_with("evt_")));
        assert_eq!(context.authorized_objectives[0].objective, "instruction 9");
        assert_eq!(
            context.authorized_objectives[0].source_id,
            context.explicit_user_instructions[7].source_id
        );
    }

    #[test]
    fn ui_fork_gets_distinct_runtime_branch_and_switch_back_resumes_main() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (_, first_reply, first_turn) = append_recorded_test_turn(&mut editor, "one", "a1");
        let (_, main_leaf, _) = append_recorded_test_turn(&mut editor, "two", "a2");

        assert!(editor.fork_ai_chat_runtime_from(first_reply));
        let fork_turn = editor.begin_ai_runtime_turn("forked").unwrap();
        assert_ne!(fork_turn.branch_id, first_turn.branch_id);
        let fork_user_event_id = fork_turn.initiating_event.caused_by.clone().unwrap();
        let events = editor
            .ai_state
            .agent_runtime
            .events(&fork_turn.run_id)
            .unwrap();
        let fork_user_event = events
            .iter()
            .find(|event| event.event_id == fork_user_event_id)
            .unwrap();
        let selected_event = events
            .iter()
            .find(|event| Some(&event.event_id) == fork_user_event.caused_by.as_ref())
            .unwrap();
        let durable_fork_event = events
            .iter()
            .find(|event| Some(&event.event_id) == selected_event.caused_by.as_ref())
            .unwrap();
        assert!(matches!(
            durable_fork_event.kind,
            EventKind::BranchLifecycle(_)
        ));
        assert_eq!(
            durable_fork_event.caused_by,
            editor
                .ai_state
                .conversation_runtime_nodes
                .get(&editor.ai_chat_conversation_key())
                .unwrap()
                .get(&first_reply)
                .map(|node| node.event_id.clone())
        );
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(fork_turn));
        editor.ai_runtime_complete_turn();

        assert!(editor.switch_ai_chat_runtime_branch(main_leaf));
        let resumed = editor.begin_ai_runtime_turn("back").unwrap();
        assert_eq!(resumed.branch_id, first_turn.branch_id);
    }

    #[test]
    fn sibling_fork_messages_switch_between_both_durable_branches() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (_, first_reply, _) = append_recorded_test_turn(&mut editor, "one", "a1");
        let (main_user, _, _) = append_recorded_test_turn(&mut editor, "main", "a2");

        assert!(editor.fork_ai_chat_runtime_from(first_reply));
        let (fork_user, _, _) = append_recorded_test_turn(&mut editor, "fork", "b2");
        assert_eq!(
            editor.conversation().unwrap().sibling_navigation(fork_user),
            Some((1, 2, main_user, main_user))
        );

        {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.viewport.follow_latest = false;
            chat.viewport.row_scroll_from_bottom = 12;
            chat.history.selected_node_id = Some(fork_user);
        }

        assert!(editor.switch_ai_chat_runtime_branch(main_user));
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(chat.viewport.follow_latest);
        assert_eq!(chat.viewport.row_scroll_from_bottom, 0);
        assert!(chat.history.selected_node_id.is_none());
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "main"));
        assert!(!editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "fork"));

        assert!(editor.switch_ai_chat_runtime_branch(fork_user));
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "fork"));
    }

    #[test]
    fn history_selection_tracks_node_identity_across_appends() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.append_user_message("u1".to_string());
            conv.append_assistant_message("a1".to_string(), "m".to_string());
            conv.append_user_message("u2".to_string());
        }

        editor.ai_chat_reset_history_cursor();
        editor.ai_chat_history_cursor_move_older(1); // select a1

        let idx_before = editor
            .ai_chat_history_selected_index()
            .expect("selected index");
        assert_eq!(editor.ai_chat_messages()[idx_before].content, "a1");

        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.append_assistant_message("a2".to_string(), "m".to_string());
        }

        let idx_after = editor
            .ai_chat_history_selected_index()
            .expect("selected index");
        assert_eq!(editor.ai_chat_messages()[idx_after].content, "a1");
    }

    #[test]
    fn history_cursor_visibility_scrolls_viewport_when_selection_offscreen() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.append_user_message("u1".to_string());
            conv.append_assistant_message("a1".to_string(), "m".to_string());
            conv.append_user_message("u2".to_string());
            conv.append_assistant_message("a2".to_string(), "m".to_string());
        }

        editor.render_cache.ai_chat_last_total_rows = 8;
        editor.render_cache.ai_chat_last_visible_start_row = 6;
        editor.render_cache.ai_chat_last_visible_end_row = 8;
        editor.render_cache.ai_chat_last_message_row_spans = vec![(0, 2), (2, 4), (4, 6), (6, 8)];

        editor.ai_chat_reset_history_cursor(); // latest (a2)
        editor.ai_chat_history_cursor_move_older(2); // target a1, above visible region

        let chat = editor.ai_state.chat.as_ref().expect("chat");
        assert!(!chat.viewport.follow_latest);
        assert_eq!(chat.viewport.pinned_base_total_rows, Some(8));
        assert!(chat.viewport.row_scroll_from_bottom > 0);
    }

    #[test]
    fn history_selection_falls_back_to_latest_when_node_leaves_branch() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        let root_id;
        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.append_user_message("u1".to_string());
            root_id = conv.node_ids_for_active_branch()[0];
            conv.append_assistant_message("a1".to_string(), "m".to_string());
            conv.append_user_message("u2".to_string());
        }

        editor.ai_chat_reset_history_cursor();
        editor.ai_chat_history_cursor_move_older(1); // select a1 on original branch

        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.fork_from(root_id);
            conv.append_assistant_message("alt".to_string(), "m".to_string());
        }

        let idx = editor
            .ai_chat_history_selected_index()
            .expect("selected index");
        assert_eq!(editor.ai_chat_messages()[idx].content, "alt");
    }

    #[test]
    fn chat_view_mode_toggles_between_docked_and_review() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        assert!(!editor.ai_chat_review_mode());
        editor.ai_chat_enter_review_mode();
        assert!(editor.ai_chat_review_mode());
        editor.ai_chat_exit_review_mode();
        assert!(!editor.ai_chat_review_mode());
    }

    #[test]
    fn accept_review_clears_markers_and_returns_to_docked_chat() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let buffer_id = editor.buffer().id();

        editor.ai_chat_enter_review_mode();
        {
            let chat = editor.ai_state.chat.as_mut().expect("chat");
            chat.agent_edits.record_edit(buffer_id, 0, 0);
            assert_eq!(chat.agent_edits.total_edit_count(), 1);
        }

        editor.ai_chat_accept_review();

        assert!(!editor.ai_chat_review_mode());
        let edits = editor
            .ai_state
            .chat
            .as_ref()
            .expect("chat")
            .agent_edits
            .total_edit_count();
        assert_eq!(edits, 0);
    }

    #[test]
    fn effective_message_scroll_is_clamped_to_viewport_window() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.viewport.follow_latest = false;
            chat.viewport.row_scroll_from_bottom = 10_000;
            chat.viewport.pinned_base_total_rows = Some(50);
        }

        // With 50 rows and a viewport of 12, max safe scroll is 38.
        let effective = editor.ai_chat_effective_message_scroll(50, 12);
        assert_eq!(effective, 38);
    }

    #[test]
    fn pinned_message_scroll_tracks_both_stream_growth_and_shrinkage() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.viewport.follow_latest = false;
            chat.viewport.row_scroll_from_bottom = 20;
            chat.viewport.pinned_base_total_rows = Some(100);
        }

        assert_eq!(editor.ai_chat_effective_message_scroll(115, 20), 35);
        assert_eq!(editor.ai_chat_effective_message_scroll(90, 20), 10);
    }

    #[test]
    fn scrolling_down_during_streaming_does_not_jump_to_latest() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        editor.render_cache.ai_chat_last_total_rows = 120;
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.viewport.follow_latest = false;
            chat.viewport.row_scroll_from_bottom = 3;
            chat.viewport.pinned_base_total_rows = Some(100);
        }

        assert!(!editor.ai_chat_scroll_viewport_down(3));
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(!chat.viewport.follow_latest);
        assert_eq!(chat.viewport.row_scroll_from_bottom, 20);
        assert_eq!(chat.viewport.pinned_base_total_rows, Some(120));
        assert_eq!(editor.ai_chat_effective_message_scroll(120, 20), 20);

        assert!(editor.ai_chat_scroll_viewport_down(20));
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(chat.viewport.follow_latest);
        assert_eq!(chat.viewport.row_scroll_from_bottom, 0);
    }

    #[test]
    fn scrolling_up_during_streaming_rebases_the_pinned_offset() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        editor.render_cache.ai_chat_last_total_rows = 120;
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.viewport.follow_latest = false;
            chat.viewport.row_scroll_from_bottom = 3;
            chat.viewport.pinned_base_total_rows = Some(100);
        }

        editor.ai_chat_scroll_viewport_up(3);
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert_eq!(chat.viewport.row_scroll_from_bottom, 26);
        assert_eq!(chat.viewport.pinned_base_total_rows, Some(120));
        assert_eq!(editor.ai_chat_effective_message_scroll(120, 20), 26);
    }

    #[test]
    fn conversation_history_survives_buffer_index_shift() {
        let mut editor = Editor::default();

        // Seed two buffers so deleting one will shift indices.
        editor.add_buffer(Buffer::new_from_str("second\n"));
        open_test_chat(&mut editor);

        {
            let conv = editor.conversation_mut().expect("conversation");
            conv.append_user_message("hello".to_string());
        }

        // Delete the first buffer so the chat buffer index changes.
        editor.switch_to_buffer(0);
        let should_quit = editor.delete_current_buffer();
        assert!(!should_quit);

        // Conversation should still resolve through stable BufferId keying.
        let messages = editor.ai_chat_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello");
    }
}
