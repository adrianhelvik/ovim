use anyhow::Result;

use crate::ai::chat_types::{ChatRole, ConversationTree, StreamChunk, ToolCallInfo};

use super::Editor;

impl Editor {
    /// Submit the current chat input as a user message and spawn the AI request.
    pub fn submit_ai_chat_message(&mut self) -> Result<()> {
        self.submit_ai_chat_message_inner(true)
    }

    pub(super) fn submit_ai_chat_message_without_slash_dispatch(&mut self) -> Result<()> {
        self.submit_ai_chat_message_inner(false)
    }

    fn submit_ai_chat_message_inner(&mut self, dispatch_slash_commands: bool) -> Result<()> {
        let (input, has_pending_images, has_code_attachment) = match self.ai_state.chat.as_ref() {
            Some(chat) => (
                chat.input.trim().to_string(),
                !chat.pending_images.is_empty(),
                chat.pending_code_attachment.is_some(),
            ),
            None => return Ok(()),
        };
        if input.is_empty() && !has_pending_images && !has_code_attachment {
            return Ok(());
        }

        // A prior terminal event may have failed only because persistence was
        // unavailable. Retry it before deciding whether this input is a steer
        // for an active turn or a new message.
        if !self.retry_pending_ai_runtime_termination() {
            return Ok(());
        }

        if self
            .ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.runtime_turn.is_some())
        {
            self.queue_current_ai_chat_input(super::ai_chat_state::QueuedChatInputKind::Steer);
            return Ok(());
        }

        if dispatch_slash_commands
            && !has_pending_images
            && !has_code_attachment
            && self.try_execute_ai_chat_slash_command(&input)?
        {
            return Ok(());
        }

        if !self.ensure_codex_auth_for_chat_submit() {
            return Ok(());
        }

        let message_input = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_code_attachment.as_ref())
            .map(|attachment| {
                super::ai_chat_code_attachment::compose_code_attachment_message(attachment, &input)
            })
            .unwrap_or_else(|| input.clone());
        let runtime_input = if message_input.is_empty() {
            "[Image attachment]".to_string()
        } else {
            message_input.clone()
        };

        // Queue, approval, and slash-command notices describe the previous
        // interaction. Do not let them masquerade as the status of a newly
        // submitted agent turn, especially in headless snapshots.
        self.set_status_message(String::new());

        // Allocate stable ovim run/agent/turn identity before provider work.
        let runtime_turn = match self.begin_ai_runtime_turn(&runtime_input) {
            Ok(turn) => turn,
            Err(error) => {
                let message = format!("Unable to start AI turn: {error}");
                crate::log_warn!("agent_runtime", "{message}");
                self.set_status_message(message);
                // Runtime persistence errors are recoverable UI failures. Keep
                // the draft intact and never let them terminate the editor's
                // top-level input loop.
                return Ok(());
            }
        };
        let user_event_id = runtime_turn.initiating_event.caused_by.clone();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.runtime_turn = Some(Box::new(runtime_turn));
        }

        // Append user message to the UI projection.
        let images = self
            .ai_state
            .chat
            .as_mut()
            .map(|chat| std::mem::take(&mut chat.pending_images))
            .unwrap_or_default();
        let user_node = self
            .conversation_mut()
            .map(|conv| conv.append_user_message_with_images(message_input, images));
        if let (Some(node_id), Some(event_id)) = (user_node, user_event_id) {
            self.record_ai_chat_node(node_id, event_id);
        }

        // Clear input and mark as waiting
        let chat = self.ai_state.chat.as_mut().unwrap();
        chat.input.clear();
        chat.input_cursor = 0;
        chat.pending_code_attachment = None;
        chat.waiting = true;
        chat.viewport.row_scroll_from_bottom = 0;
        chat.viewport.follow_latest = true;
        chat.viewport.pinned_base_total_rows = None;
        chat.history.selected_node_id = None;
        chat.tool_call_count = 0;
        chat.pending_tool_approval = None;

        // Spawn the streaming request
        if let Err(e) = self.spawn_streaming_request() {
            self.ai_runtime_fail_turn(e.to_string());
            if let Some(conv) = self.conversation_mut() {
                conv.append_error(e.to_string());
            }
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.waiting = false;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------
    // Poll
    // -----------------------------------------------------------------

    /// Drain available streaming chunks. Returns true if state changed.
    pub fn poll_pending_ai_chat_job(&mut self) -> bool {
        if self.poll_pending_ai_subagent_control() {
            return true;
        }
        if self.poll_pending_auto_mode_classification() {
            return true;
        }
        if self.poll_pending_shell_execution() {
            return true;
        }
        if self.poll_pending_background_tool() {
            return true;
        }
        let current_branch_generation = self
            .conversation()
            .map(ConversationTree::branch_generation)
            .unwrap_or_default();
        let pending_branch_generation = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_job.as_ref())
            .map(|job| job.branch_generation);
        if pending_branch_generation
            .is_some_and(|generation| generation != current_branch_generation)
        {
            if let Some(job) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_job.take())
            {
                job.task.abort();
            }
            self.ai_runtime_interrupt_turn("conversation branch changed during provider turn");
            if let Some(conv) = self.conversation_mut() {
                conv.append_error("Discarded stale response from a previous branch".into());
            }
            self.clear_streaming_state();
            return true;
        }

        let chat = match self.ai_state.chat.as_mut() {
            Some(c) => c,
            None => return false,
        };

        let job = match chat.pending_job.as_mut() {
            Some(j) => j,
            None => return false,
        };

        // Phase 1: Drain all available chunks into a local vec.
        let mut chunks = Vec::new();
        let mut disconnected = false;
        loop {
            match job.receiver.try_recv() {
                Ok(chunk) => chunks.push(chunk),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if chunks.is_empty() && !disconnected {
            return false;
        }

        // Extract model_name before processing.
        let model_name = chat
            .pending_job
            .as_ref()
            .map(|j| j.model_name.clone())
            .unwrap_or_default();
        let runtime_turn = chat.pending_job.as_ref().map(|job| (*job.turn).clone());

        // Phase 2: Process collected chunks.
        let mut changed = false;
        for chunk in chunks {
            match chunk {
                StreamChunk::Content(text) => {
                    self.append_code_explanation_answer(&text);
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        if let Some(ref mut s) = chat.streaming_content {
                            s.push_str(&text);
                        }
                    }
                    changed = true;
                }
                StreamChunk::Thinking(text) => {
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        match chat.streaming_thinking.as_mut() {
                            Some(s) => s.push_str(&text),
                            None => chat.streaming_thinking = Some(text),
                        }
                    }
                    changed = true;
                }
                StreamChunk::AgentMessageComplete => {
                    // Codex turns may contain multiple agentMessage items.
                    // Commit each completed item independently while leaving
                    // the turn in its working state for subsequent tools or
                    // messages.
                    self.flush_ai_runtime_stream_segments();
                    self.commit_partial_streaming(&model_name);
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.streaming_content = Some(String::new());
                        chat.streaming_thinking = None;
                        chat.streaming_provider_state.clear();
                        chat.runtime_recorded_content_bytes = 0;
                        chat.runtime_recorded_thinking_bytes = 0;
                        chat.runtime_last_content_event = None;
                        chat.runtime_last_reasoning_event = None;
                    }
                    changed = true;
                }
                StreamChunk::ToolCallComplete {
                    id,
                    name,
                    arguments,
                } => {
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.streaming_tool_calls.push(ToolCallInfo {
                            id,
                            name,
                            arguments,
                        });
                    }
                    changed = true;
                }
                StreamChunk::ProviderState(items) => {
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.streaming_provider_state.extend(items);
                    }
                    changed = true;
                }
                StreamChunk::DynamicToolRequest { call, response } => {
                    self.flush_ai_runtime_stream_segments();
                    let Some(turn) = runtime_turn.as_ref() else {
                        let _ = response.send(Err("agent turn identity is missing".into()));
                        continue;
                    };
                    let tool = match self.ai_runtime_record_tool_intent(turn, &call) {
                        Ok(tool) => tool,
                        Err(error) => {
                            let message = format!("failed to record tool intent: {error}");
                            let _ = response.send(Err(message.clone()));
                            self.ai_runtime_fail_turn(message);
                            self.clear_streaming_state();
                            return true;
                        }
                    };
                    if call.name == "bash" {
                        if self.ai_chat_yolo_mode() {
                            self.execute_dynamic_tool_after_policy(
                                turn.clone(),
                                tool,
                                call,
                                response,
                                None,
                                false,
                            );
                        } else if self.ai_state.config.tool_approval_mode
                            == crate::ai::ToolApprovalMode::Auto
                        {
                            self.begin_dynamic_bash_auto_mode(call, response, turn.clone(), tool);
                        } else {
                            self.pause_dynamic_tool_for_approval(
                                turn.clone(),
                                tool,
                                call,
                                response,
                                "configured approval policy requires confirmation".into(),
                            );
                        }
                        changed = true;
                        continue;
                    }
                    if let Err(error) = self.ai_runtime_start_tool(turn, &tool) {
                        let message = format!("failed to record tool start: {error}");
                        let _ = response.send(Err(message.clone()));
                        self.ai_runtime_fail_turn(message);
                        self.clear_streaming_state();
                        return true;
                    }
                    if self.is_background_ai_tool(&call.name) {
                        let continuation =
                            super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                                runtime_tool: tool.clone(),
                                runtime_turn: turn.clone(),
                                response,
                            };
                        match self.begin_pending_background_tool(call.clone(), continuation) {
                            Ok(()) => {
                                changed = true;
                                continue;
                            }
                            Err((result, continuation)) => {
                                let super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                                    runtime_tool,
                                    runtime_turn,
                                    response,
                                } = *continuation
                                else {
                                    unreachable!()
                                };
                                self.finish_dynamic_tool(
                                    &runtime_turn,
                                    &runtime_tool,
                                    &call,
                                    response,
                                    result,
                                );
                                changed = true;
                                continue;
                            }
                        }
                    }
                    if matches!(
                        call.name.as_str(),
                        crate::ai::tools::subagents::WAIT_AGENT_TOOL
                            | crate::ai::tools::subagents::INTERRUPT_AGENT_TOOL
                            | crate::ai::tools::subagents::FOLLOWUP_AGENT_TOOL
                    ) {
                        let continuation =
                            super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                                runtime_tool: tool.clone(),
                                runtime_turn: turn.clone(),
                                response,
                            };
                        match self.begin_pending_ai_subagent_control(call.clone(), continuation) {
                            Ok(()) => {
                                changed = true;
                                continue;
                            }
                            Err((result, continuation)) => {
                                let super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                                    runtime_tool,
                                    runtime_turn,
                                    response,
                                } = *continuation
                                else {
                                    unreachable!()
                                };
                                self.finish_dynamic_tool(
                                    &runtime_turn,
                                    &runtime_tool,
                                    &call,
                                    response,
                                    result,
                                );
                                changed = true;
                                continue;
                            }
                        }
                    }
                    if call.name == "explain_with_codebase" {
                        let continuation =
                            super::ai_chat_state::CodeExplanationContinuation::Dynamic {
                                runtime_tool: tool.clone(),
                                runtime_turn: turn.clone(),
                                response,
                            };
                        match self.begin_code_explanation(call.clone(), continuation) {
                            Ok(()) => {
                                changed = true;
                                continue;
                            }
                            Err((result, continuation)) => {
                                let super::ai_chat_state::CodeExplanationContinuation::Dynamic {
                                    runtime_tool,
                                    runtime_turn,
                                    response,
                                } = *continuation
                                else {
                                    unreachable!("dynamic walkthrough retained batch continuation")
                                };
                                self.finish_dynamic_tool(
                                    &runtime_turn,
                                    &runtime_tool,
                                    &call,
                                    response,
                                    result,
                                );
                                changed = true;
                                continue;
                            }
                        }
                    }
                    let outcome = self.dispatch_tool_call_with_approval(&call, None);
                    let result = match outcome {
                        super::ai_chat_tools::ToolDispatchOutcome::Completed(result) => result,
                        super::ai_chat_tools::ToolDispatchOutcome::ApprovalRequired(req) => {
                            self.pause_dynamic_tool_for_approval_request(
                                turn.clone(),
                                tool,
                                call,
                                response,
                                req,
                                true,
                            );
                            changed = true;
                            continue;
                        }
                    };
                    if let Err(error) = self.ai_runtime_finish_tool(turn, &tool, &result) {
                        let message = format!("failed to record tool result: {error}");
                        let _ = response.send(Err(message.clone()));
                        self.ai_runtime_fail_turn(message);
                        self.clear_streaming_state();
                        return true;
                    }
                    self.record_tool_event_summary(&call, &result);
                    let result_content = self.format_tool_result_with_target(&call, &result);
                    if let Some(conv) = self.conversation_mut() {
                        conv.append_tool_result(call.id.clone(), result_content);
                    }
                    let wire_result = match &result {
                        crate::ai::tools::ToolResult::Success(text) => Ok(text.clone()),
                        crate::ai::tools::ToolResult::Error(text) => Err(text.clone()),
                    };
                    let _ = response.send(wire_result);
                    changed = true;
                }
                StreamChunk::SteerAccepted { id, content } => {
                    if let Err(error) = self.accept_provider_ai_chat_steer(id, content) {
                        self.set_status_message(format!(
                            "Failed to record accepted steer: {error}"
                        ));
                    }
                    changed = true;
                }
                StreamChunk::SteerRejected { id, error } => {
                    self.reject_provider_ai_chat_steer(id, &error);
                    changed = true;
                }
                StreamChunk::Done => {
                    self.flush_ai_runtime_stream_segments();
                    // Commit thinking (if any) as a Thinking message.
                    let thinking = self
                        .ai_state
                        .chat
                        .as_mut()
                        .and_then(|c| c.streaming_thinking.take());
                    if let Some(thinking_text) = thinking {
                        if !thinking_text.is_empty() {
                            let event_id = self
                                .ai_state
                                .chat
                                .as_ref()
                                .and_then(|chat| chat.runtime_last_reasoning_event.clone());
                            let node_id = self.conversation_mut().map(|conv| {
                                conv.append_thinking_message(thinking_text, model_name.clone())
                            });
                            if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                                self.record_ai_chat_node(node_id, event_id);
                            }
                        }
                    }

                    // Take tool calls and content
                    let tool_calls = self
                        .ai_state
                        .chat
                        .as_mut()
                        .map(|c| std::mem::take(&mut c.streaming_tool_calls))
                        .unwrap_or_default();
                    let provider_state = self
                        .ai_state
                        .chat
                        .as_mut()
                        .map(|c| std::mem::take(&mut c.streaming_provider_state))
                        .unwrap_or_default();
                    let content = self
                        .ai_state
                        .chat
                        .as_mut()
                        .and_then(|c| c.streaming_content.take())
                        .unwrap_or_default();

                    if !tool_calls.is_empty() {
                        // The provider stream has completed. Detach its job
                        // before a local tool starts: asynchronous tools (Exa
                        // in particular) can span event-loop ticks, and the
                        // completed receiver would otherwise be observed as a
                        // disconnected active stream on the next tick.
                        self.finish_provider_stream_before_tools();
                        return self.process_tool_calls(
                            tool_calls,
                            content,
                            provider_state,
                            &model_name,
                        );
                    }

                    // No tool calls — normal text-only commit
                    if !content.is_empty() {
                        // The visible message may contain text streamed before a
                        // dynamic tool. Anchor the node at the current causal tip
                        // so forking from it includes the observed tool result.
                        let event_id = self.ai_runtime_current_tip();
                        let node_id = self.conversation_mut().map(|conv| {
                            conv.append_assistant_message_with_tools_and_state(
                                content,
                                model_name.clone(),
                                Vec::new(),
                                provider_state,
                            )
                        });
                        if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                            self.record_ai_chat_node(node_id, event_id);
                        }
                    }

                    // Clear undo group (agent turn is done)
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.current_undo_group = None;
                    }

                    self.ai_runtime_complete_turn();
                    self.finish_code_explanation_answer(None);
                    self.clear_streaming_state();
                    if let Err(error) = self.start_next_queued_ai_chat_input() {
                        if let Some(conv) = self.conversation_mut() {
                            conv.append_error(format!("Failed to run queued input: {error}"));
                        }
                    }
                    return true;
                }
                StreamChunk::Error(msg) => {
                    self.flush_ai_runtime_stream_segments();
                    self.commit_partial_streaming(&model_name);

                    // Append the error.
                    if let Some(conv) = self.conversation_mut() {
                        conv.append_error(msg.clone());
                    }

                    self.finish_code_explanation_answer(Some(&msg));
                    self.ai_runtime_fail_turn(msg);
                    self.clear_streaming_state();
                    return true;
                }
                StreamChunk::ToolCall { .. } => {
                    // Progressive tool call updates — currently just wait for ToolCallComplete
                }
            }
        }

        // Handle channel disconnected without Done (task crashed/cancelled).
        if disconnected {
            self.flush_ai_runtime_stream_segments();
            let thinking = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|c| c.streaming_thinking.take());
            if let Some(thinking_text) = thinking.filter(|text| !text.is_empty()) {
                let event_id = self
                    .ai_state
                    .chat
                    .as_ref()
                    .and_then(|chat| chat.runtime_last_reasoning_event.clone());
                let node_id = self
                    .conversation_mut()
                    .map(|conv| conv.append_thinking_message(thinking_text, model_name.clone()));
                if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                    self.record_ai_chat_node(node_id, event_id);
                }
            }
            let content = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|c| c.streaming_content.take());
            if let Some(content_text) = content {
                if !content_text.is_empty() {
                    let event_id = self
                        .ai_state
                        .chat
                        .as_ref()
                        .and_then(|chat| chat.runtime_last_content_event.clone());
                    let node_id = self.conversation_mut().map(|conv| {
                        conv.append_assistant_message(content_text, model_name.clone())
                    });
                    if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                        self.record_ai_chat_node(node_id, event_id);
                    }
                }
            }
            if let Some(conv) = self.conversation_mut() {
                conv.append_error("Stream interrupted".to_string());
            }
            self.finish_code_explanation_answer(Some("The answer stream was interrupted."));
            self.ai_runtime_interrupt_turn("provider stream disconnected");
            self.clear_streaming_state();
            return true;
        }

        changed
    }

    pub(super) fn finish_provider_stream_before_tools(&mut self) {
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.pending_job = None;
        }
    }

    /// Commit any partial thinking/content that was streaming when an error occurred.
    pub(super) fn commit_partial_streaming(&mut self, model_name: &str) {
        let thinking = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|c| c.streaming_thinking.take());
        if let Some(thinking_text) = thinking {
            if !thinking_text.is_empty() {
                let event_id = self
                    .ai_state
                    .chat
                    .as_ref()
                    .and_then(|chat| chat.runtime_last_reasoning_event.clone());
                let node_id = self.conversation_mut().map(|conv| {
                    conv.append_thinking_message(thinking_text, model_name.to_string())
                });
                if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                    self.record_ai_chat_node(node_id, event_id);
                }
            }
        }

        let content = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|c| c.streaming_content.take());
        if let Some(content_text) = content {
            if !content_text.is_empty() {
                let event_id = self
                    .ai_state
                    .chat
                    .as_ref()
                    .and_then(|chat| chat.runtime_last_content_event.clone());
                let node_id = self.conversation_mut().map(|conv| {
                    conv.append_assistant_message(content_text, model_name.to_string())
                });
                if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
                    self.record_ai_chat_node(node_id, event_id);
                }
            }
        }
    }

    pub(super) fn shell_authorization_context(
        &self,
        project_root: &std::path::Path,
    ) -> crate::ai::auto_mode::ConversationAuthorizationContext {
        use crate::ai::auto_mode::{AuthorizedObjective, ExplicitAuthorization};

        let key = self.ai_chat_conversation_key();
        let runtime_nodes = self.ai_state.conversation_runtime_nodes.get(&key);
        let mut recent = self
            .conversation()
            .map(|conversation| {
                conversation
                    .messages()
                    .iter()
                    .zip(conversation.node_ids_for_active_branch())
                    .filter(|(message, _)| message.role == ChatRole::User)
                    .map(|(message, node_id)| {
                        let source_id = runtime_nodes
                            .and_then(|nodes| nodes.get(node_id))
                            .map(|reference| reference.event_id.as_str().to_string())
                            .unwrap_or_else(|| format!("ui-node:{node_id}"));
                        (message.content.clone(), source_id)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if recent.len() > 8 {
            recent.drain(..recent.len() - 8);
        }
        let explicit_user_instructions = recent
            .iter()
            .map(|(instruction, source_id)| ExplicitAuthorization {
                instruction: instruction.clone(),
                project_root: project_root.to_path_buf(),
                source_id: source_id.clone(),
            })
            .collect();
        let authorized_objectives = recent
            .last()
            .map(|(objective, source_id)| {
                vec![AuthorizedObjective {
                    objective: objective.clone(),
                    project_root: project_root.to_path_buf(),
                    source_id: source_id.clone(),
                }]
            })
            .unwrap_or_default();
        crate::ai::auto_mode::ConversationAuthorizationContext {
            explicit_user_instructions,
            authorized_objectives,
        }
    }

    fn begin_dynamic_bash_auto_mode(
        &mut self,
        call: ToolCallInfo,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
        turn: crate::agent_runtime::PendingTurnRef,
        tool: crate::agent_runtime::PendingToolRef,
    ) {
        use crate::ai::auto_classifier::{AutoModeClassifier, CodexAutoModeClassifier};
        use crate::ai::auto_mode::{ClassifierRequest, ShellProposal, StaticDisposition};
        use std::collections::BTreeSet;

        let command = call
            .arguments
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        let Some(project_root) = self.ai_effective_project_root() else {
            self.finish_dynamic_tool(
                &turn,
                &tool,
                &call,
                response,
                crate::ai::tools::ToolResult::Error(self.no_project_root_error()),
            );
            return;
        };
        if self.current_session_authorizes_temp_shell_command(&command) {
            self.execute_dynamic_tool_after_policy(turn, tool, call, response, None, false);
            return;
        }
        let request = ClassifierRequest::new(
            ShellProposal {
                command,
                cwd: project_root.clone(),
                project_root: project_root.clone(),
                requested_capabilities: BTreeSet::new(),
            },
            self.shell_authorization_context(&project_root),
        );

        if request
            .dynamic
            .static_analysis
            .disposition
            .requires_model_review()
        {
            let operation_id = tool.operation_id.clone();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let result = CodexAutoModeClassifier::default()
                    .classify(&request, &operation_id)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = result_tx.send(result);
            });
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.pending_auto_mode_classification =
                    Some(super::ai_chat_state::PendingAutoModeClassification {
                        tool_call: call,
                        runtime_tool: tool,
                        runtime_turn: turn,
                        dynamic_response: response,
                        receiver: result_rx,
                    });
            }
            self.set_status_message("Terra is reviewing the proposed shell program");
        } else {
            debug_assert_eq!(
                request.dynamic.static_analysis.disposition,
                StaticDisposition::LocallySafe
            );
            self.execute_dynamic_tool_after_policy(turn, tool, call, response, None, false);
        }
    }

    pub(super) fn poll_pending_auto_mode_classification(&mut self) -> bool {
        use crate::ai::auto_mode::ClassifierDecision;
        let received = {
            let Some(pending) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_auto_mode_classification.as_mut())
            else {
                return false;
            };
            match pending.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Some(Err("auto-mode classifier stopped without a verdict".into()))
                }
            }
        };
        let pending = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_auto_mode_classification.take())
            .expect("pending classifier exists");
        let project_root = self.ai_effective_project_root();
        match received.expect("classifier result") {
            Ok(verdict)
                if verdict.decision == ClassifierDecision::Allow
                    && project_root.as_ref() == Some(&verdict.scope.project_root) =>
            {
                self.execute_dynamic_tool_after_policy(
                    pending.runtime_turn,
                    pending.runtime_tool,
                    pending.tool_call,
                    pending.dynamic_response,
                    None,
                    false,
                );
            }
            Ok(verdict) if verdict.decision == ClassifierDecision::Deny => {
                self.finish_dynamic_tool(
                    &pending.runtime_turn,
                    &pending.runtime_tool,
                    &pending.tool_call,
                    pending.dynamic_response,
                    crate::ai::tools::ToolResult::Error(format!(
                        "auto mode denied shell program: {}",
                        verdict.reason
                    )),
                );
            }
            Ok(verdict) => self.pause_dynamic_tool_for_approval(
                pending.runtime_turn,
                pending.runtime_tool,
                pending.tool_call,
                pending.dynamic_response,
                if verdict.decision == ClassifierDecision::Allow {
                    "classifier returned an Allow outside the active repository scope".into()
                } else {
                    verdict.reason
                },
            ),
            Err(error) => {
                crate::log_warn!("ai_auto_mode", "classifier unavailable: {error}");
                self.pause_dynamic_tool_for_approval(
                    pending.runtime_turn,
                    pending.runtime_tool,
                    pending.tool_call,
                    pending.dynamic_response,
                    format!("classifier unavailable; explicit confirmation required: {error}"),
                )
            }
        }
        true
    }

    pub(super) fn execute_dynamic_tool_after_policy(
        &mut self,
        turn: crate::agent_runtime::PendingTurnRef,
        tool: crate::agent_runtime::PendingToolRef,
        call: ToolCallInfo,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
        approved_once_root: Option<std::path::PathBuf>,
        tool_already_started: bool,
    ) {
        if call.name == "bash" {
            let authorized =
                self.ai_state
                    .tool_registry
                    .get(&call.name)
                    .is_some_and(|definition| {
                        let capabilities = self.build_chat_capabilities();
                        capabilities.allows_side_effect(definition.side_effect)
                            && capabilities.contains(&definition.required_scope)
                    });
            if !authorized {
                self.finish_dynamic_tool(
                    &turn,
                    &tool,
                    &call,
                    response,
                    crate::ai::tools::ToolResult::Error(
                        "shell access is not authorized for this chat".into(),
                    ),
                );
                return;
            }
        }
        if !tool_already_started {
            if let Err(error) = self.ai_runtime_start_tool(&turn, &tool) {
                let result = crate::ai::tools::ToolResult::Error(format!(
                    "failed to durably record tool start: {error}"
                ));
                self.finish_dynamic_tool(&turn, &tool, &call, response, result);
                return;
            }
        }
        if call.name == "bash" {
            let command = call
                .arguments
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if let Some(reason) = self.comprehension_gate_for_bash(&command) {
                self.finish_dynamic_tool(
                    &turn,
                    &tool,
                    &call,
                    response,
                    crate::ai::tools::ToolResult::Error(reason),
                );
                return;
            }
            let Some(workdir) = self.ai_effective_project_root() else {
                self.finish_dynamic_tool(
                    &turn,
                    &tool,
                    &call,
                    response,
                    crate::ai::tools::ToolResult::Error(self.no_project_root_error()),
                );
                return;
            };
            let artifact_store = match self.ai_state.durable_runs.as_ref().and_then(|services| {
                services
                    .store
                    .layout()
                    .ensure_run_directory(&turn.run_id)
                    .map_err(|error| error.to_string())
                    .and_then(|_| {
                        crate::run_log::ArtifactStore::open(
                            services.store.layout().artifact_directory(&turn.run_id),
                        )
                        .map_err(|error| error.to_string())
                    })
                    .ok()
            }) {
                Some(store) => store,
                None => {
                    self.finish_dynamic_tool(
                        &turn,
                        &tool,
                        &call,
                        response,
                        crate::ai::tools::ToolResult::Error(
                            "shell program was not executed because replay artifact storage is unavailable".into(),
                        ),
                    );
                    return;
                }
            };
            self.start_pending_shell_execution(
                call,
                super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                    runtime_tool: tool,
                    runtime_turn: turn,
                    response,
                },
                command,
                workdir,
                artifact_store,
            );
            return;
        }
        let result = {
            match self.dispatch_tool_call_with_approval(&call, approved_once_root.as_ref()) {
                super::ai_chat_tools::ToolDispatchOutcome::Completed(result) => result,
                super::ai_chat_tools::ToolDispatchOutcome::ApprovalRequired(request) => {
                    crate::ai::tools::ToolResult::Error(format!(
                        "tool policy changed after auto-mode approval: {}",
                        request.message
                    ))
                }
            }
        };
        self.finish_dynamic_tool(&turn, &tool, &call, response, result);
    }

    pub(super) fn start_pending_shell_execution(
        &mut self,
        call: ToolCallInfo,
        continuation: super::ai_chat_state::ToolExecutionContinuation,
        command: String,
        workdir: std::path::PathBuf,
        artifact_store: crate::run_log::ArtifactStore,
    ) {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let kill = std::sync::Arc::new(super::ai_chat_state::ShellKillHandle::default());
        let kill_for_task = kill.clone();
        let task_command = command.clone();
        let task_workdir = workdir.clone();
        let task = tokio::task::spawn_blocking(move || {
            let temp_probe =
                super::ai_session_temp::TempPathProbe::for_shell_command(&task_command);
            let observation = match crate::run_log::capture_workspace(&task_workdir, &artifact_store) {
                Ok(before) => {
                    let result = super::ai_tool_execution::run_bash_program(
                        &task_command,
                        &task_workdir,
                        Some(&kill_for_task),
                        Some(progress_tx.clone()),
                    );
                    let _ = progress_tx
                        .send(super::ai_chat_state::ShellProgressEvent::CapturingChanges);
                    match crate::run_log::capture_workspace(&task_workdir, &artifact_store) {
                        Ok(after) => super::ai_chat_state::ShellExecutionObservation {
                            result,
                            delta: Some(before.diff(after)),
                            created_temp_files: temp_probe.created_files(),
                            capture_error: None,
                            outcome_unknown: false,
                        },
                        Err(error) => super::ai_chat_state::ShellExecutionObservation {
                            result,
                            delta: None,
                            created_temp_files: Vec::new(),
                            capture_error: Some(format!(
                                "shell completed, but after-state capture failed: {error}"
                            )),
                            outcome_unknown: true,
                        },
                    }
                }
                Err(error) => super::ai_chat_state::ShellExecutionObservation {
                    result: crate::ai::tools::ToolResult::Error(format!(
                        "shell program was not executed because before-state capture failed: {error}"
                    )),
                    delta: None,
                    created_temp_files: Vec::new(),
                    capture_error: Some(error),
                    outcome_unknown: false,
                },
            };
            let _ = result_tx.send(observation);
        });
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.shell_transcripts.insert(
                call.id.clone(),
                super::ai_chat_state::ShellTranscript::new(call.clone(), command, workdir),
            );
            chat.pending_shell_execution = Some(super::ai_chat_state::PendingShellExecution {
                tool_call: call,
                continuation,
                receiver: result_rx,
                progress: progress_rx,
                task,
                kill,
            });
            chat.waiting = true;
        }
        self.set_status_message("Agent shell program is running");
    }

    fn poll_pending_shell_execution(&mut self) -> bool {
        let received = {
            let Some(chat) = self.ai_state.chat.as_mut() else {
                return false;
            };
            let Some(pending) = chat.pending_shell_execution.as_mut() else {
                return false;
            };
            let tool_call_id = pending.tool_call.id.clone();
            let mut progress_changed = false;
            while let Ok(event) = pending.progress.try_recv() {
                progress_changed = true;
                let Some(transcript) = chat.shell_transcripts.get_mut(&tool_call_id) else {
                    continue;
                };
                match event {
                    super::ai_chat_state::ShellProgressEvent::Spawned { pid } => {
                        transcript.pid = Some(pid);
                        if transcript.phase
                            != super::ai_chat_state::ShellTranscriptPhase::InterruptRequested
                        {
                            transcript.phase = super::ai_chat_state::ShellTranscriptPhase::Running;
                        }
                    }
                    super::ai_chat_state::ShellProgressEvent::Output { stream, bytes } => {
                        transcript.append(stream, bytes);
                    }
                    super::ai_chat_state::ShellProgressEvent::CapturingChanges => {
                        transcript.phase =
                            super::ai_chat_state::ShellTranscriptPhase::CapturingChanges;
                    }
                }
            }
            match pending.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    return progress_changed;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Some(super::ai_chat_state::ShellExecutionObservation {
                        result: crate::ai::tools::ToolResult::Error(
                            "shell execution task stopped without a result".into(),
                        ),
                        delta: None,
                        created_temp_files: Vec::new(),
                        capture_error: Some(
                            "shell execution and workspace result are unknown".into(),
                        ),
                        outcome_unknown: true,
                    })
                }
            }
        };
        let pending = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_shell_execution.take())
            .expect("pending shell exists");
        let observation = received.expect("shell result");
        for path in &observation.created_temp_files {
            self.record_current_session_temp_file(path);
        }
        if let Some(chat) = self.ai_state.chat.as_mut() {
            let phase = if observation.outcome_unknown {
                super::ai_chat_state::ShellTranscriptPhase::OutcomeUnknown
            } else if pending.kill.is_interrupted() {
                super::ai_chat_state::ShellTranscriptPhase::Interrupted
            } else {
                match &observation.result {
                    crate::ai::tools::ToolResult::Success(_) => {
                        super::ai_chat_state::ShellTranscriptPhase::Succeeded
                    }
                    crate::ai::tools::ToolResult::Error(_) => {
                        super::ai_chat_state::ShellTranscriptPhase::Failed
                    }
                }
            };
            if let Some(transcript) = chat.shell_transcripts.get_mut(&pending.tool_call.id) {
                transcript.finish(phase);
            }
            chat.shell_transcript_lru
                .retain(|id| id != &pending.tool_call.id);
            chat.shell_transcript_lru
                .push_back(pending.tool_call.id.clone());
            chat.evict_old_shell_transcripts();
        }
        let (runtime_turn, runtime_tool) = match &pending.continuation {
            super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                runtime_turn,
                runtime_tool,
                ..
            } => (Some(runtime_turn), Some(runtime_tool)),
            super::ai_chat_state::ToolExecutionContinuation::Batch {
                runtime_turn,
                runtime_tool,
                ..
            } => (runtime_turn.as_ref(), runtime_tool.as_ref()),
        };
        if let Some(delta) = observation.delta {
            if let (Some(turn), Some(tool)) = (runtime_turn, runtime_tool) {
                for mutation in delta.mutations {
                    if let Err(error) = self
                        .ai_state
                        .agent_runtime
                        .record_tool_file_mutation(turn, tool, mutation)
                    {
                        crate::log_warn!(
                            "agent_runtime",
                            "failed to record shell mutation: {error}"
                        );
                    }
                }
                for issue in delta.issues {
                    if let Err(error) = self
                        .ai_state
                        .agent_runtime
                        .record_tool_capture_issue(turn, tool, issue)
                    {
                        crate::log_warn!(
                            "agent_runtime",
                            "failed to record capture issue: {error}"
                        );
                    }
                }
            }
        }
        if let Some(error) = observation.capture_error.as_ref() {
            if let (Some(turn), Some(tool)) = (runtime_turn, runtime_tool) {
                if let Err(record_error) =
                    self.ai_state
                        .agent_runtime
                        .record_tool_capture_issue(turn, tool, error.clone())
                {
                    crate::log_warn!(
                        "agent_runtime",
                        "failed to record capture failure: {record_error}"
                    );
                }
            }
        }
        if observation.outcome_unknown {
            let detail = observation
                .capture_error
                .unwrap_or_else(|| "shell result and workspace effects are unknown".into());
            if let (Some(turn), Some(tool)) = (runtime_turn, runtime_tool) {
                if let Err(error) = self.ai_state.agent_runtime.mark_tool_outcome_unknown(
                    turn,
                    tool,
                    detail.clone(),
                ) {
                    crate::log_warn!(
                        "agent_runtime",
                        "failed to record unknown shell outcome: {error}"
                    );
                }
            }
            match pending.continuation {
                super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                    runtime_turn,
                    response,
                    ..
                } => {
                    let _ = response.send(Err(detail));
                    self.fail_specific_dynamic_turn(
                        &runtime_turn,
                        "shell execution could not be durably observed".into(),
                    );
                }
                super::ai_chat_state::ToolExecutionContinuation::Batch {
                    runtime_turn,
                    remaining_tool_calls,
                    ..
                } => {
                    if let Some(turn) = runtime_turn.as_ref() {
                        self.fail_specific_dynamic_turn(
                            turn,
                            "shell execution could not be durably observed".into(),
                        );
                    }
                    // Every committed tool_use must get a tool_result, or the
                    // next provider request is rejected as malformed.
                    let mut unresolved = vec![pending.tool_call.clone()];
                    unresolved.extend(remaining_tool_calls);
                    self.append_synthetic_tool_results(
                        &unresolved,
                        &format!("Outcome unknown: {detail}"),
                    );
                    if let Some(conversation) = self.conversation_mut() {
                        conversation.append_error(detail);
                    }
                    self.clear_streaming_state();
                    self.set_status_message(String::new());
                    // No job, stream, or pending state is left to complete this
                    // turn — falling through would re-arm the waiting spinner
                    // forever. Only the Dynamic arm may fall through (its
                    // provider stream is still live).
                    return true;
                }
            }
        } else {
            match pending.continuation {
                super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                    runtime_turn,
                    runtime_tool,
                    response,
                } => self.finish_dynamic_tool(
                    &runtime_turn,
                    &runtime_tool,
                    &pending.tool_call,
                    response,
                    observation.result,
                ),
                super::ai_chat_state::ToolExecutionContinuation::Batch {
                    runtime_turn,
                    runtime_tool,
                    remaining_tool_calls,
                    model_name,
                } => {
                    if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref())
                    {
                        if let Err(error) =
                            self.ai_runtime_finish_tool(turn, tool, &observation.result)
                        {
                            self.ai_runtime_fail_turn(format!(
                                "failed to record shell tool result: {error}"
                            ));
                            self.clear_streaming_state();
                            return true;
                        }
                    }
                    self.record_tool_event_summary(&pending.tool_call, &observation.result);
                    let result_content = self
                        .format_tool_result_with_target(&pending.tool_call, &observation.result);
                    if let Some(conversation) = self.conversation_mut() {
                        conversation
                            .append_tool_result(pending.tool_call.id.clone(), result_content);
                    }
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                    }
                    self.set_status_message(String::new());
                    return self.execute_tool_call_batch(remaining_tool_calls, model_name);
                }
            }
        }
        self.set_status_message(String::new());
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.waiting = true;
        }
        true
    }

    fn poll_pending_background_tool(&mut self) -> bool {
        let received = {
            let Some(pending) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_background_tool.as_mut())
            else {
                return false;
            };
            match pending.receiver.try_recv() {
                Ok(outcome) => outcome,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    super::ai_chat_state::BackgroundToolOutcome::plain(
                        crate::ai::tools::ToolResult::Error(
                            "background tool stopped without returning a result".into(),
                        ),
                    )
                }
            }
        };
        let pending = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_background_tool.take())
            .expect("pending background tool exists");

        if let super::ai_chat_state::BackgroundToolAftermath::Exa {
            credential_rejected,
            environment_override,
            setup_error,
        } = received.aftermath
        {
            if credential_rejected {
                self.note_exa_credential_rejected(environment_override);
            } else if let Some(error) = setup_error {
                self.open_exa_setup_dialog(Some(error));
            }
        }
        self.set_status_message(String::new());
        match pending.continuation {
            super::ai_chat_state::ToolExecutionContinuation::Dynamic {
                runtime_tool,
                runtime_turn,
                response,
            } => {
                self.finish_dynamic_tool(
                    &runtime_turn,
                    &runtime_tool,
                    &pending.tool_call,
                    response,
                    received.result,
                );
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.waiting = true;
                }
                true
            }
            super::ai_chat_state::ToolExecutionContinuation::Batch {
                runtime_tool,
                runtime_turn,
                remaining_tool_calls,
                model_name,
            } => {
                if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref())
                    && let Err(error) = self.ai_runtime_finish_tool(turn, tool, &received.result)
                {
                    self.ai_runtime_fail_turn(format!(
                        "failed to record background tool result: {error}"
                    ));
                    self.clear_streaming_state();
                    return true;
                }
                self.record_tool_event_summary(&pending.tool_call, &received.result);
                let result_content =
                    self.format_tool_result_with_target(&pending.tool_call, &received.result);
                if let Some(conversation) = self.conversation_mut() {
                    conversation.append_tool_result(pending.tool_call.id, result_content);
                }
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                }
                self.execute_tool_call_batch(remaining_tool_calls, model_name)
            }
        }
    }

    fn fail_specific_dynamic_turn(
        &mut self,
        turn: &crate::agent_runtime::PendingTurnRef,
        detail: String,
    ) {
        match self.ai_state.agent_runtime.fail_turn(turn, detail.clone()) {
            Ok(_) | Err(crate::agent_runtime::AgentRuntimeError::TurnAlreadyTerminal) => {
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    if chat
                        .runtime_turn
                        .as_ref()
                        .is_some_and(|active| active.turn_id == turn.turn_id)
                    {
                        chat.runtime_turn = None;
                        chat.pending_runtime_termination = None;
                    }
                }
            }
            Err(error) => {
                // Keep the editor's turn reference so termination can be
                // retried after transient failures such as a full disk.
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.pending_runtime_termination = Some(
                        super::ai_chat_state::PendingRuntimeTermination::Failed(detail),
                    );
                }
                crate::log_warn!("agent_runtime", "failed to terminate dynamic turn: {error}");
            }
        }
    }

    pub(super) fn finish_dynamic_tool(
        &mut self,
        turn: &crate::agent_runtime::PendingTurnRef,
        tool: &crate::agent_runtime::PendingToolRef,
        call: &ToolCallInfo,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
        result: crate::ai::tools::ToolResult,
    ) {
        if let Err(error) = self.ai_runtime_finish_tool(turn, tool, &result) {
            let _ = response.send(Err(format!("failed to record tool result: {error}")));
            self.ai_runtime_fail_turn(format!("failed to record tool result: {error}"));
            self.clear_streaming_state();
            return;
        }
        self.record_tool_event_summary(call, &result);
        let result_content = self.format_tool_result_with_target(call, &result);
        if let Some(conv) = self.conversation_mut() {
            conv.append_tool_result(call.id.clone(), result_content);
        }
        let wire = match result {
            crate::ai::tools::ToolResult::Success(text) => Ok(text),
            crate::ai::tools::ToolResult::Error(text) => Err(text),
        };
        let _ = response.send(wire);
    }

    fn pause_dynamic_tool_for_approval(
        &mut self,
        turn: crate::agent_runtime::PendingTurnRef,
        tool: crate::agent_runtime::PendingToolRef,
        call: ToolCallInfo,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
        reason: String,
    ) {
        let root = self
            .ai_effective_project_root()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.pause_dynamic_tool_for_approval_request(
            turn,
            tool,
            call,
            response,
            super::ai_chat_tools::ToolApprovalRequest {
                requested_path: root.clone(),
                approval_root: root,
                reason,
                message: String::new(),
            },
            false,
        );
    }

    fn pause_dynamic_tool_for_approval_request(
        &mut self,
        turn: crate::agent_runtime::PendingTurnRef,
        tool: crate::agent_runtime::PendingToolRef,
        call: ToolCallInfo,
        response: tokio::sync::oneshot::Sender<Result<String, String>>,
        request: super::ai_chat_tools::ToolApprovalRequest,
        runtime_tool_started: bool,
    ) {
        let mut installed = false;
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.pending_tool_approval = Some(super::ai_chat_state::PendingToolApproval {
                tool_call: call,
                reason: request.reason.clone(),
                runtime_tool: Some(tool),
                runtime_tool_started,
                remaining_tool_calls: Vec::new(),
                model_name: String::new(),
                requested_path: request.requested_path,
                approval_root: request.approval_root,
                dynamic_response: Some(response),
                dynamic_turn: Some(turn),
            });
            // Keep pending_job alive: its app-server task is blocked on the
            // dynamic response and resumes exactly once after this UI decision.
            chat.waiting = false;
            installed = true;
        }
        if installed {
            self.ai_state.ai_attention_generation =
                self.ai_state.ai_attention_generation.saturating_add(1);
        }
        let status = if request.message.is_empty() {
            format!(
                "Shell approval required: {}. Press Ctrl-Y to allow once or Ctrl-N to deny.",
                request.reason
            )
        } else {
            request.message
        };
        self.set_status_message(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;

    #[test]
    fn stale_runtime_turn_does_not_escape_the_chat_input_loop() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();

        // Simulate a runtime turn whose UI handle was lost after a transient
        // persistence failure.
        let _active_turn = editor.begin_ai_runtime_turn("first message").unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "retry after freeing disk space".into();
        chat.input_cursor = chat.input.len();

        assert!(editor.submit_ai_chat_message().is_ok());
        assert_eq!(editor.ai_chat_input(), "retry after freeing disk space");
        assert_eq!(
            editor.status_message(),
            "Unable to start AI turn: a turn is already active"
        );
    }

    #[test]
    fn pending_runtime_termination_is_retried_before_the_next_turn() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();

        let active_turn = editor.begin_ai_runtime_turn("first message").unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(active_turn));
        chat.pending_runtime_termination = Some(
            super::super::ai_chat_state::PendingRuntimeTermination::Failed("disk was full".into()),
        );
        chat.input = "retry after freeing disk space".into();
        chat.input_cursor = chat.input.len();

        assert!(editor.retry_pending_ai_runtime_termination());
        assert!(editor.active_ai_runtime_turn().is_none());
        assert_eq!(editor.ai_chat_input(), "retry after freeing disk space");
        assert!(editor
            .begin_ai_runtime_turn("retry after freeing disk space")
            .is_ok());
    }
}
