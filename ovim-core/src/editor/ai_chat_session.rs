use crate::ai::chat_types::{ChatFocus, ChatMessage};

use super::ai_chat_state::{AiChatState, ChatModelPickerSection};
use super::Editor;

pub const AI_CHAT_REASONING_EFFORTS: &[&str] =
    &["default", "none", "low", "medium", "high", "xhigh", "max"];

impl Editor {
    /// Get a reference to the active chat state.
    pub fn ai_chat_state(&self) -> Option<&AiChatState> {
        self.ai_state.chat.as_ref()
    }

    pub fn open_ai_chat_model_picker(&mut self, section: ChatModelPickerSection) -> bool {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        chat.model_picker_section = section;
        chat.focus = ChatFocus::ModelSelector;
        true
    }

    pub fn ai_chat_model_picker_section(&self) -> ChatModelPickerSection {
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| chat.model_picker_section)
            .unwrap_or_default()
    }

    pub fn ai_chat_reasoning_effort(&self) -> String {
        let Some(chat) = self.ai_state.chat.as_ref() else {
            return "default".into();
        };
        chat.reasoning_effort_override
            .clone()
            .or_else(|| {
                self.ai_state
                    .config
                    .resolve_profile(&self.ai_chat_effective_profile())
                    .and_then(|profile| profile.reasoning_effort.clone())
            })
            .unwrap_or_else(|| "default".into())
    }

    /// Picker value, kept separate from the effective profile-derived effort.
    pub fn ai_chat_reasoning_effort_selection(&self) -> String {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.reasoning_effort_override.clone())
            .unwrap_or_else(|| "default".into())
    }

    pub fn set_ai_chat_reasoning_effort(&mut self, effort: &str) -> bool {
        if !AI_CHAT_REASONING_EFFORTS.contains(&effort) {
            self.set_status_message(format!("Unknown reasoning effort: {effort}"));
            return false;
        }
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        chat.reasoning_effort_override = (effort != "default").then(|| effort.to_string());
        let effective = self.ai_chat_reasoning_effort();
        self.set_status_message(if effort == "default" {
            format!("AI reasoning effort: {effective} (profile default)")
        } else {
            format!("AI reasoning effort: {effective}")
        });
        true
    }

    pub fn cycle_ai_chat_reasoning_effort(&mut self, forward: bool) -> bool {
        let current = self.ai_chat_reasoning_effort_selection();
        let current = AI_CHAT_REASONING_EFFORTS
            .iter()
            .position(|effort| *effort == current)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % AI_CHAT_REASONING_EFFORTS.len()
        } else if current == 0 {
            AI_CHAT_REASONING_EFFORTS.len() - 1
        } else {
            current - 1
        };
        self.set_ai_chat_reasoning_effort(AI_CHAT_REASONING_EFFORTS[next])
    }

    /// Get the messages for the current chat conversation.
    pub fn ai_chat_messages(&self) -> &[ChatMessage] {
        self.conversation().map(|c| c.messages()).unwrap_or(&[])
    }

    /// Get the current chat focus zone.
    pub fn ai_chat_focus(&self) -> ChatFocus {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.focus)
            .unwrap_or(ChatFocus::TextInput)
    }

    /// Get chat input text.
    pub fn ai_chat_input(&self) -> &str {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.input.as_str())
            .unwrap_or("")
    }

    /// Get chat input cursor position.
    pub fn ai_chat_input_cursor(&self) -> usize {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.input_cursor)
            .unwrap_or(0)
    }

    /// Whether chat is waiting for a response.
    pub fn ai_chat_waiting(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.waiting)
            .unwrap_or(false)
    }

    /// Single lifecycle projection shared by the TUI, headless API, and
    /// cancellation/queue logic.
    pub fn ai_chat_activity(&self) -> super::AiChatActivity {
        self.ai_state
            .chat
            .as_ref()
            .map(AiChatState::activity)
            .unwrap_or(super::AiChatActivity::Idle)
    }

    /// Explicit tool-call guardrail for the effective chat profile. `None`
    /// means the turn may continue for as many tool calls as it needs.
    pub fn ai_chat_tool_call_limit(&self) -> Option<u64> {
        let profile = self.ai_chat_effective_profile();
        self.ai_state
            .config
            .resolve_profile(&profile)
            .and_then(|profile| profile.agent_loop.max_tool_calls)
    }

    /// Whether an AI turn still has pending work that can affect review flow.
    pub fn ai_chat_has_pending_work(&self) -> bool {
        self.ai_chat_activity().has_pending_work()
    }

    /// Whether this chat bypasses model and user tool-approval gates.
    pub fn ai_chat_yolo_mode(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| chat.yolo_mode)
            .unwrap_or(false)
    }

    /// Set the per-chat approval bypass. Enabling it also releases work that
    /// is already blocked on a folder, tool, or Terra decision.
    pub fn set_ai_chat_yolo_mode(&mut self, enabled: bool) -> bool {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        if chat.yolo_mode == enabled {
            return false;
        }
        chat.yolo_mode = enabled;

        if enabled {
            if self.ai_chat_has_pending_no_repo_folder_approval() {
                self.ai_chat_resolve_pending_no_repo_folder_approval(true);
            }
            if self.ai_chat_has_pending_tool_approval() {
                self.ai_chat_resolve_pending_tool_approval(true, false);
            }
            let pending_classifier = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_auto_mode_classification.take());
            if let Some(pending) = pending_classifier {
                self.execute_dynamic_tool_after_policy(
                    pending.runtime_turn,
                    pending.runtime_tool,
                    pending.tool_call,
                    pending.dynamic_response,
                    None,
                    false,
                );
            }
        }

        self.set_status_message(if enabled {
            "YOLO mode enabled for this chat; tool approvals are bypassed"
        } else {
            "YOLO mode disabled; normal tool approval policy restored"
        });
        true
    }

    pub fn toggle_ai_chat_yolo_mode(&mut self) -> bool {
        if self.ai_state.chat.is_none() {
            return false;
        }
        let enabled = !self.ai_chat_yolo_mode();
        self.set_ai_chat_yolo_mode(enabled);
        enabled
    }

    /// Monotonic signal updated whenever an active agent pauses for approval.
    /// UI and headless clients can use this to notify once per new prompt.
    pub fn ai_chat_attention_generation(&self) -> u64 {
        self.ai_state
            .ai_attention_generation
            .saturating_add(self.ai_state.subagents.attention_generation())
    }

    /// Whether a tool call is currently paused pending user approval.
    pub fn ai_chat_has_pending_tool_approval(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.pending_tool_approval.is_some())
            .unwrap_or(false)
    }

    /// Whether chat is waiting for first-time no-repo folder approval.
    pub fn ai_chat_has_pending_no_repo_folder_approval(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.pending_no_repo_folder_approval.is_some())
            .unwrap_or(false)
    }

    /// Human-readable summary of the pending no-repo folder approval, if any.
    pub fn ai_chat_pending_no_repo_folder_approval_summary(&self) -> Option<String> {
        let pending = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|c| c.pending_no_repo_folder_approval.as_ref())?;
        Some(format!(
            "Not in a git repo. Allow tool access to folder: {}",
            pending.display()
        ))
    }

    /// Human-readable summary of the pending approval, if any.
    pub fn ai_chat_pending_tool_approval_summary(&self) -> Option<String> {
        let pending = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|c| c.pending_tool_approval.as_ref())?;
        if pending.tool_call.name == "bash" {
            let command = pending
                .tool_call
                .arguments
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<missing shell program>");
            Some(format!(
                "Command:\n{command}\n\nTerra: {}\nWorking directory: {}",
                pending.reason,
                pending.requested_path.display()
            ))
        } else {
            Some(format!(
                "Tool: {}\nReason: {}\nPath: {}",
                pending.tool_call.name,
                pending.reason,
                pending.requested_path.display()
            ))
        }
    }

    /// Whether chat allows edits.
    pub fn ai_chat_allow_edits(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .map(|c| c.allow_edits)
            .unwrap_or(true)
    }

    /// Preferred width of the docked chat, as a percentage of the shared
    /// buffer/chat area. The renderer still enforces its minimum widths.
    pub fn ai_chat_panel_width_percent(&self) -> Option<u16> {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.panel_width_percent)
    }

    /// Resize the docked chat from a separator position captured in the last
    /// rendered split. Storing a percentage makes the choice survive terminal
    /// resizes without pinning the panel to a stale column count.
    pub fn resize_ai_chat_panel(&mut self, separator_column: u16, split_area: crate::Rect) -> bool {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        if split_area.width == 0 {
            return false;
        }
        let right = split_area.x.saturating_add(split_area.width);
        let separator = separator_column.clamp(split_area.x, right);
        let chat_width = right.saturating_sub(separator);
        let percent = ((u32::from(chat_width) * 100 + u32::from(split_area.width) / 2)
            / u32::from(split_area.width)) as u16;
        let percent = percent.clamp(1, 99);
        if chat.panel_width_percent == Some(percent) {
            return false;
        }
        chat.panel_width_percent = Some(percent);
        true
    }

    /// Human-readable save policy for AI chat mutations.
    pub fn ai_chat_save_policy_label(&self) -> Option<&'static str> {
        self.ai_state
            .chat
            .as_ref()
            .map(|_| "only_if_clean_at_start")
    }

    /// Effective save mode for current AI target buffer.
    pub fn ai_chat_save_mode_label(&self) -> Option<&'static str> {
        let chat = self.ai_state.chat.as_ref()?;
        let has_path = self
            .get_buffer_by_id(chat.active_buffer_id)
            .and_then(|b| b.file_path())
            .is_some();
        if !has_path {
            return Some("unsaved-buffer");
        }
        if chat.buffer_was_clean_at_chat_start {
            Some("auto")
        } else {
            Some("manual")
        }
    }

    /// Most recent save outcome message for this chat session.
    pub fn ai_chat_last_save_outcome(&self) -> Option<&str> {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|c| c.last_save_outcome.as_deref())
    }
}
