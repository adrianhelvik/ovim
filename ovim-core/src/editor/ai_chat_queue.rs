use anyhow::Result;

use super::ai_chat_state::{QueuedChatInput, QueuedChatInputKind};
use super::Editor;
use crate::ai::chat_types::ProviderSteerUpdate;

impl Editor {
    /// Tab submits a normal follow-up for the next round while work is active.
    pub fn schedule_ai_chat_message(&mut self) -> Result<()> {
        if !self.ai_chat_round_active() {
            return self.submit_ai_chat_message();
        }
        self.queue_current_ai_chat_input(QueuedChatInputKind::FollowUp);
        Ok(())
    }

    pub fn ai_chat_round_active(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.runtime_turn.is_some())
    }

    pub fn ai_chat_queued_inputs(&self) -> impl Iterator<Item = &QueuedChatInput> {
        self.ai_state
            .chat
            .as_ref()
            .into_iter()
            .flat_map(|chat| chat.queued_inputs.iter())
    }

    /// Queues the composer's current contents. Infallible by construction: every
    /// step is an in-memory state update, and the steer notification is
    /// best-effort (a closed receiver means the round already ended).
    ///
    /// The return type is deliberately `()` rather than `Result<()>`. It used to
    /// be the latter without a single failing path, which invited callers to
    /// take state out of a pending struct *before* calling and then bail on an
    /// error that could never arrive — leaving that state dropped. Letting the
    /// compiler prove infallibility removes the temptation and the dead
    /// handling that came with it.
    pub(crate) fn queue_current_ai_chat_input(&mut self, requested_kind: QueuedChatInputKind) {
        let input = self.ai_chat_input().trim().to_string();
        let has_images = !self.ai_chat_pending_images().is_empty();
        if input.is_empty() && !has_images {
            return;
        }
        let kind = if has_images {
            // Codex steering currently accepts text items only. Keep image
            // messages intact for the next complete provider round.
            QueuedChatInputKind::FollowUp
        } else if input.starts_with('/') {
            QueuedChatInputKind::Command
        } else {
            requested_kind
        };

        if let Some(chat) = self.ai_state.chat.as_mut() {
            let id = chat.next_queued_input_id;
            chat.next_queued_input_id = chat.next_queued_input_id.saturating_add(1);
            chat.input.clear();
            chat.input_cursor = 0;
            let images = std::mem::take(&mut chat.pending_images);
            chat.queued_inputs.push_back(QueuedChatInput {
                id,
                kind,
                content: input.clone(),
                images,
            });
            if kind == QueuedChatInputKind::Steer {
                if let Some(tx) = chat
                    .pending_job
                    .as_ref()
                    .and_then(|job| job.steer_tx.as_ref())
                {
                    let _ = tx.send(ProviderSteerUpdate::Queue {
                        id,
                        content: input.clone(),
                    });
                }
            }
        }

        let status = match kind {
            QueuedChatInputKind::Steer => "Steer queued for the next tool call",
            QueuedChatInputKind::FollowUp => "Message queued for the next round",
            QueuedChatInputKind::Command => "Slash command queued for the end of this round",
        };
        self.set_status_message(status.to_string());
    }

    /// Apply steers at ovim's post-tool continuation boundary (OpenAI,
    /// Anthropic, Ollama). Codex app-server acknowledges these asynchronously.
    pub(crate) fn apply_local_ai_chat_steers(&mut self) -> Result<()> {
        let steers = self.take_queued_ai_chat_inputs(QueuedChatInputKind::Steer);
        for content in steers {
            self.record_accepted_ai_chat_steer(content)?;
        }
        Ok(())
    }

    pub(crate) fn accept_provider_ai_chat_steer(&mut self, id: u64, content: String) -> Result<()> {
        self.remove_queued_ai_chat_input(id);
        self.record_accepted_ai_chat_steer(content)
    }

    pub(crate) fn reject_provider_ai_chat_steer(&mut self, id: u64, error: &str) {
        if self.ai_code_explanation_answering() {
            let content = self
                .ai_state
                .chat
                .as_ref()
                .and_then(|chat| chat.queued_inputs.iter().find(|item| item.id == id))
                .map(|item| item.content.clone());
            self.remove_queued_ai_chat_input(id);
            if let Some(content) = content {
                if let Err(record_error) = self.record_accepted_ai_chat_steer(content) {
                    self.set_status_message(format!(
                        "Walkthrough question reached the agent through the tool result, but its durable user message could not be recorded: {record_error}"
                    ));
                    return;
                }
            }
            self.set_status_message(format!(
                "Provider steering was unavailable ({error}); the walkthrough question was delivered through the tool result"
            ));
            return;
        }
        if let Some(chat) = self.ai_state.chat.as_mut() {
            if let Some(item) = chat.queued_inputs.iter_mut().find(|item| item.id == id) {
                item.kind = QueuedChatInputKind::FollowUp;
            }
        }
        self.set_status_message(format!(
            "Could not steer the active turn; queued for next round: {error}"
        ));
    }

    fn record_accepted_ai_chat_steer(&mut self, content: String) -> Result<()> {
        let turn = self
            .active_ai_runtime_turn()
            .ok_or_else(|| anyhow::anyhow!("AI turn ended before steer was recorded"))?;
        let event = self
            .ai_state
            .agent_runtime
            .append_user_steering(&turn, content.clone())?;
        let node = self
            .conversation_mut()
            .map(|conversation| conversation.append_user_message(content));
        if let Some(node) = node {
            self.record_ai_chat_node(node, event.event_id);
        }
        Ok(())
    }

    fn take_queued_ai_chat_inputs(&mut self, kind: QueuedChatInputKind) -> Vec<String> {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return Vec::new();
        };
        let mut selected = Vec::new();
        chat.queued_inputs.retain(|item| {
            if item.kind == kind {
                selected.push(item.content.clone());
                false
            } else {
                true
            }
        });
        selected
    }

    fn remove_queued_ai_chat_input(&mut self, id: u64) -> Option<QueuedChatInput> {
        let chat = self.ai_state.chat.as_mut()?;
        if let Some(index) = chat.queued_inputs.iter().position(|item| item.id == id) {
            let removed = chat.queued_inputs.remove(index);
            if chat.history.selected_queued_id == Some(id) {
                chat.history.selected_queued_id = chat
                    .queued_inputs
                    .get(index.min(chat.queued_inputs.len().saturating_sub(1)))
                    .map(|item| item.id);
            }
            return removed;
        }
        None
    }

    pub fn recall_queued_ai_chat_input(&mut self, id: u64) -> bool {
        let Some(item) = self.remove_queued_ai_chat_input(id) else {
            return false;
        };
        if item.kind == QueuedChatInputKind::Steer {
            if let Some(tx) = self
                .ai_state
                .chat
                .as_ref()
                .and_then(|chat| chat.pending_job.as_ref())
                .and_then(|job| job.steer_tx.as_ref())
            {
                let _ = tx.send(ProviderSteerUpdate::Cancel { id });
            }
        }
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.input = item.content;
            chat.input_cursor = chat.input.len();
            chat.pending_images = item.images;
            chat.history.selected_queued_id = None;
            chat.history.selected_node_id = None;
            chat.focus = crate::ai::chat_types::ChatFocus::TextInput;
        }
        self.set_status_message("Queued message moved back to the composer".to_string());
        true
    }

    /// Execute queued commands in order, then start at most one queued user
    /// follow-up. Any unconsumed steers become ordinary follow-ups when a turn
    /// completes without reaching another tool boundary.
    pub(crate) fn start_next_queued_ai_chat_input(&mut self) -> Result<bool> {
        let draft = self.ai_state.chat.as_mut().map(|chat| {
            (
                std::mem::take(&mut chat.input),
                chat.input_cursor,
                std::mem::take(&mut chat.pending_images),
            )
        });

        let result = (|| {
            loop {
                let item = self
                    .ai_state
                    .chat
                    .as_mut()
                    .and_then(|chat| chat.queued_inputs.pop_front());
                let Some(mut item) = item else {
                    return Ok(false);
                };
                if item.kind == QueuedChatInputKind::Steer {
                    item.kind = QueuedChatInputKind::FollowUp;
                }
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.input = item.content;
                    chat.input_cursor = chat.input.len();
                    chat.pending_images = item.images;
                }
                match item.kind {
                    QueuedChatInputKind::Command => {
                        let command = self.ai_chat_input().to_string();
                        self.try_execute_ai_chat_slash_command(&command)?;
                        // A scheduled command has already been consumed. Clear
                        // its temporary composer value before the next item.
                        if let Some(chat) = self.ai_state.chat.as_mut() {
                            chat.input.clear();
                            chat.input_cursor = 0;
                        }
                    }
                    QueuedChatInputKind::FollowUp | QueuedChatInputKind::Steer => {
                        self.submit_ai_chat_message()?;
                        return Ok(true);
                    }
                }
            }
        })();

        if let (Some(chat), Some((input, cursor, images))) = (self.ai_state.chat.as_mut(), draft) {
            chat.input = input;
            chat.input_cursor = cursor.min(chat.input.len());
            chat.pending_images = images;
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::{ChatOpts, ImageAttachment, ToolCallInfo};
    use crate::editor::ai_chat_state::{
        CodeExplanationExchange, CodeExplanationInteraction, PendingCodeExplanation,
    };
    use crate::editor::code_explanation::CodeExplanationStep;
    use std::path::PathBuf;

    fn editor_with_active_round() -> Editor {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let turn = editor.begin_ai_runtime_turn("initial request").unwrap();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));
        editor
    }

    #[test]
    fn enter_queues_steer_and_local_tool_boundary_accepts_it() {
        let mut editor = editor_with_active_round();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "change direction".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().unwrap();
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().queued_inputs.len(),
            1
        );
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().queued_inputs[0].kind,
            QueuedChatInputKind::Steer
        );

        editor.apply_local_ai_chat_steers().unwrap();
        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .queued_inputs
            .is_empty());
        assert_eq!(
            editor.ai_chat_messages().last().unwrap().content,
            "change direction"
        );
    }

    #[test]
    fn rejected_walkthrough_steer_is_recorded_without_duplicate_follow_up() {
        let mut editor = editor_with_active_round();
        let content = "> Walkthrough step 1 · demo.rs:1\n> Context\n\nWhy?".to_string();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.queued_inputs.push_back(QueuedChatInput {
            id: 7,
            kind: QueuedChatInputKind::Steer,
            content: content.clone(),
            images: Vec::new(),
        });
        chat.pending_code_explanation = Some(PendingCodeExplanation {
            tool_call: ToolCallInfo {
                id: "walkthrough".into(),
                name: "explain_with_codebase".into(),
                arguments: serde_json::json!({}),
            },
            steps: vec![CodeExplanationStep::Code {
                path: "demo.rs".into(),
                absolute_path: PathBuf::from("demo.rs"),
                snapshot: Some(std::sync::Arc::from("fn demo() {}\n")),
                start_line: 1,
                end_line: 1,
                comment: "Context".into(),
            }],
            current: 0,
            answer_scroll: 0,
            threads: vec![vec![CodeExplanationExchange {
                question: "Why?".into(),
                answer: String::new(),
                failed: false,
            }]],
            interaction: CodeExplanationInteraction::Answering {
                step: 0,
                exchange: 0,
            },
            original_active_buffer_id: chat.active_buffer_id,
            presentation_buffer_id: None,
            continuation: None,
        });

        editor.reject_provider_ai_chat_steer(7, "unsupported");

        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .queued_inputs
            .is_empty());
        assert_eq!(editor.ai_chat_messages().last().unwrap().content, content);
    }

    #[test]
    fn tab_queues_follow_up_and_slash_command_is_visually_typed_as_command() {
        let mut editor = editor_with_active_round();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "after this".into();
        chat.input_cursor = chat.input.len();
        editor.schedule_ai_chat_message().unwrap();

        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/clear".into();
        chat.input_cursor = chat.input.len();
        editor.schedule_ai_chat_message().unwrap();

        let queued = &editor.ai_state.chat.as_ref().unwrap().queued_inputs;
        assert_eq!(queued[0].kind, QueuedChatInputKind::FollowUp);
        assert_eq!(queued[1].kind, QueuedChatInputKind::Command);
        assert_eq!(queued[1].content, "/clear");
    }

    #[test]
    fn images_are_queued_for_the_next_round_instead_of_steered() {
        let mut editor = editor_with_active_round();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.pending_images.push(ImageAttachment {
            path: PathBuf::from("/tmp/screenshot.png"),
            mime_type: "image/png".to_string(),
            data: vec![1, 2, 3],
        });

        editor.submit_ai_chat_message().unwrap();

        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(chat.pending_images.is_empty());
        assert_eq!(chat.queued_inputs.len(), 1);
        assert_eq!(chat.queued_inputs[0].kind, QueuedChatInputKind::FollowUp);
        assert_eq!(chat.queued_inputs[0].images.len(), 1);
    }

    #[tokio::test]
    async fn submitted_image_moves_from_composer_to_user_message() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "inspect this".into();
        chat.input_cursor = chat.input.len();
        chat.pending_images.push(ImageAttachment {
            path: PathBuf::from("/tmp/sent.png"),
            mime_type: "image/png".to_string(),
            data: vec![1, 2, 3],
        });

        editor.submit_ai_chat_message().unwrap();

        assert!(editor.ai_chat_pending_images().is_empty());
        let message = editor.ai_chat_messages().first().unwrap();
        assert_eq!(message.content, "inspect this");
        assert_eq!(message.images.len(), 1);
        assert_eq!(message.images[0].path, PathBuf::from("/tmp/sent.png"));
    }

    #[tokio::test]
    async fn starting_older_queued_follow_up_preserves_recalled_composer_draft() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "newer recalled draft".into();
        chat.input_cursor = chat.input.len();
        chat.queued_inputs.push_back(QueuedChatInput {
            id: 1,
            kind: QueuedChatInputKind::FollowUp,
            content: "older queued follow-up".into(),
            images: Vec::new(),
        });

        assert!(editor.start_next_queued_ai_chat_input().unwrap());

        assert_eq!(editor.ai_chat_input(), "newer recalled draft");
        assert_eq!(
            editor.ai_chat_messages().last().unwrap().content,
            "older queued follow-up"
        );
        editor.cancel_ai_chat_generation();
    }
}
