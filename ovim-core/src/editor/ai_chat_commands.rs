use anyhow::Result;

use crate::ai::chat_types::{ChatFocus, ConversationTree};

use super::Editor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiChatSlashCommandKind {
    Clear,
    Exa,
    Model,
    Effort,
    Comprehension,
    Yolo,
}

/// Presentation metadata shared by parsing, composer completion, and the TUI.
/// Keeping one registry prevents newly added commands from silently missing
/// autocomplete or documentation in the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiChatSlashCompletion {
    pub command: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    kind: AiChatSlashCommandKind,
}

const AI_CHAT_SLASH_COMMANDS: &[AiChatSlashCompletion] = &[
    AiChatSlashCompletion {
        command: "/clear",
        usage: "/clear",
        description: "Clear this conversation",
        kind: AiChatSlashCommandKind::Clear,
    },
    AiChatSlashCompletion {
        command: "/exa",
        usage: "/exa",
        description: "Configure Exa web search",
        kind: AiChatSlashCommandKind::Exa,
    },
    AiChatSlashCompletion {
        command: "/model",
        usage: "/model [profile]",
        description: "Choose the chat model",
        kind: AiChatSlashCommandKind::Model,
    },
    AiChatSlashCompletion {
        command: "/effort",
        usage: "/effort [default|none|low|medium|high|xhigh]",
        description: "Choose the chat reasoning effort",
        kind: AiChatSlashCommandKind::Effort,
    },
    AiChatSlashCompletion {
        command: "/comprehension",
        usage: "/comprehension [off|publish|commit]",
        description: "Require demonstrated understanding at a boundary",
        kind: AiChatSlashCommandKind::Comprehension,
    },
    AiChatSlashCompletion {
        command: "/yolo",
        usage: "/yolo [on|off]",
        description: "Set tool approval policy",
        kind: AiChatSlashCommandKind::Yolo,
    },
];

#[derive(Debug, PartialEq, Eq)]
enum AiChatSlashCommand {
    Clear,
    Exa,
    Model { profile: Option<String> },
    Effort { effort: Option<String> },
    Comprehension { policy: super::ComprehensionPolicy },
    Yolo { enabled: Option<bool> },
}

impl AiChatSlashCommand {
    fn parse(input: &str) -> Option<std::result::Result<Self, String>> {
        let command = input.strip_prefix('/')?;
        let mut parts = command.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let arguments = parts.collect::<Vec<_>>();

        let Some(spec) = AI_CHAT_SLASH_COMMANDS
            .iter()
            .find(|spec| spec.command.strip_prefix('/') == Some(name))
        else {
            return Some(if name.is_empty() {
                Err("Enter a slash command after /".to_string())
            } else {
                Err(format!("Unknown AI chat command: /{name}"))
            });
        };

        let parsed = match spec.kind {
            AiChatSlashCommandKind::Clear if arguments.is_empty() => Ok(Self::Clear),
            AiChatSlashCommandKind::Clear => Err(format!("Usage: {}", spec.usage)),
            AiChatSlashCommandKind::Exa if arguments.is_empty() => Ok(Self::Exa),
            AiChatSlashCommandKind::Exa => Err(format!("Usage: {}", spec.usage)),
            AiChatSlashCommandKind::Model if arguments.len() <= 1 => Ok(Self::Model {
                profile: arguments.first().map(|value| (*value).to_string()),
            }),
            AiChatSlashCommandKind::Model => Err(format!("Usage: {}", spec.usage)),
            AiChatSlashCommandKind::Effort if arguments.len() <= 1 => {
                let effort = arguments.first().copied();
                if effort.is_some_and(|effort| {
                    !super::ai_chat_session::AI_CHAT_REASONING_EFFORTS.contains(&effort)
                }) {
                    return Some(Err(format!("Usage: {}", spec.usage)));
                }
                Ok(Self::Effort {
                    effort: effort.map(str::to_string),
                })
            }
            AiChatSlashCommandKind::Effort => Err(format!("Usage: {}", spec.usage)),
            AiChatSlashCommandKind::Comprehension if arguments.len() <= 1 => {
                let policy = match arguments.first().copied() {
                    None | Some("publish") => super::ComprehensionPolicy::Publish,
                    Some("off") => super::ComprehensionPolicy::Off,
                    Some("commit") => super::ComprehensionPolicy::Commit,
                    Some(_) => return Some(Err(format!("Usage: {}", spec.usage))),
                };
                Ok(Self::Comprehension { policy })
            }
            AiChatSlashCommandKind::Comprehension => Err(format!("Usage: {}", spec.usage)),
            AiChatSlashCommandKind::Yolo if arguments.is_empty() => {
                Ok(Self::Yolo { enabled: None })
            }
            AiChatSlashCommandKind::Yolo if arguments == ["on"] => Ok(Self::Yolo {
                enabled: Some(true),
            }),
            AiChatSlashCommandKind::Yolo if arguments == ["off"] => Ok(Self::Yolo {
                enabled: Some(false),
            }),
            AiChatSlashCommandKind::Yolo => Err(format!("Usage: {}", spec.usage)),
        };
        Some(parsed)
    }
}

impl Editor {
    /// Slash commands matching the command-name fragment at the cursor.
    /// Exact commands and argument text intentionally hide the popup so Enter
    /// retains its normal execute/submit behavior.
    pub fn ai_chat_slash_completions(&self) -> Vec<AiChatSlashCompletion> {
        let Some(chat) = self.ai_state.chat.as_ref() else {
            return Vec::new();
        };
        if chat.focus != ChatFocus::TextInput
            || chat.input_cursor != chat.input.len()
            || chat.input.contains('\n')
        {
            return Vec::new();
        }
        let Some(fragment) = chat.input.strip_prefix('/') else {
            return Vec::new();
        };
        if fragment.chars().any(char::is_whitespace)
            || AI_CHAT_SLASH_COMMANDS
                .iter()
                .any(|spec| spec.command.strip_prefix('/') == Some(fragment))
        {
            return Vec::new();
        }
        AI_CHAT_SLASH_COMMANDS
            .iter()
            .filter(|spec| {
                spec.command
                    .strip_prefix('/')
                    .is_some_and(|name| name.starts_with(fragment))
            })
            .copied()
            .collect()
    }

    pub fn ai_chat_slash_completion_selected(&self) -> usize {
        let len = self.ai_chat_slash_completions().len();
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| chat.slash_completion_selected.min(len.saturating_sub(1)))
            .unwrap_or(0)
    }

    pub fn move_ai_chat_slash_completion(&mut self, forward: bool) -> bool {
        let len = self.ai_chat_slash_completions().len();
        if len == 0 {
            return false;
        }
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        let current = chat.slash_completion_selected.min(len - 1);
        chat.slash_completion_selected = if forward {
            (current + 1) % len
        } else if current == 0 {
            len - 1
        } else {
            current - 1
        };
        true
    }

    pub fn accept_ai_chat_slash_completion(&mut self, selected: Option<usize>) -> bool {
        let completions = self.ai_chat_slash_completions();
        if completions.is_empty() {
            return false;
        }
        let index = selected
            .unwrap_or_else(|| self.ai_chat_slash_completion_selected())
            .min(completions.len() - 1);
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        chat.input = completions[index].command.to_string();
        chat.input_cursor = chat.input.len();
        chat.slash_completion_selected = 0;
        true
    }

    pub fn reset_ai_chat_slash_completion(&mut self) {
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.slash_completion_selected = 0;
        }
    }

    /// Parse and execute editor-owned chat commands before provider submission.
    /// Returns `true` when the input was a slash command, including invalid ones.
    pub(super) fn try_execute_ai_chat_slash_command(&mut self, input: &str) -> Result<bool> {
        let Some(command) = AiChatSlashCommand::parse(input) else {
            return Ok(false);
        };
        match command {
            Ok(AiChatSlashCommand::Clear) => self.clear_ai_chat_conversation()?,
            Ok(AiChatSlashCommand::Exa) => {
                self.clear_ai_chat_input();
                self.open_exa_setup_dialog(None);
            }
            Ok(AiChatSlashCommand::Model { profile: None }) => {
                self.clear_ai_chat_input();
                self.open_ai_chat_model_picker(super::ChatModelPickerSection::Model);
            }
            Ok(AiChatSlashCommand::Model {
                profile: Some(profile),
            }) => {
                if self.ai_set_profile(&profile) {
                    self.clear_ai_chat_input();
                }
            }
            Ok(AiChatSlashCommand::Effort { effort: None }) => {
                self.clear_ai_chat_input();
                self.open_ai_chat_model_picker(super::ChatModelPickerSection::Effort);
            }
            Ok(AiChatSlashCommand::Effort {
                effort: Some(effort),
            }) => {
                if self.set_ai_chat_reasoning_effort(&effort) {
                    self.clear_ai_chat_input();
                }
            }
            Ok(AiChatSlashCommand::Comprehension { policy }) => {
                self.clear_ai_chat_input();
                self.set_ai_chat_comprehension_policy(policy);
            }
            Ok(AiChatSlashCommand::Yolo { enabled }) => {
                self.clear_ai_chat_input();
                let enabled = enabled.unwrap_or_else(|| !self.ai_chat_yolo_mode());
                self.set_ai_chat_yolo_mode(enabled);
            }
            Err(message) => self.set_status_message(message),
        }
        Ok(true)
    }

    pub(crate) fn clear_ai_chat_input(&mut self) {
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.input.clear();
            chat.input_cursor = 0;
            chat.pending_images.clear();
            chat.slash_completion_selected = 0;
        }
    }

    fn clear_ai_chat_conversation(&mut self) -> Result<()> {
        let key = self.ai_chat_conversation_key();
        let locator = self
            .ai_state
            .durable_chat_bindings
            .get(&key)
            .map(|binding| binding.locator.clone())
            .unwrap_or_else(|| {
                crate::agent_runtime::ConversationLocator(format!(
                    "buffer:{}:conversation:{}",
                    key.0, key.1
                ))
            });
        self.ai_state.agent_runtime.record_context_reset(&locator)?;
        self.reset_durable_ai_chat_provider_session()?;

        self.ai_state
            .conversations
            .insert(key.clone(), ConversationTree::new());
        self.ai_state.conversation_runtime_nodes.remove(&key);

        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.input.clear();
            chat.input_cursor = 0;
            chat.pending_images.clear();
            chat.focus = ChatFocus::TextInput;
            chat.context_generation = chat.context_generation.saturating_add(1);
            chat.viewport = Default::default();
            chat.history = Default::default();
            chat.expanded_thinking.clear();
            chat.expanded_tool_events.clear();
            chat.streaming_content = None;
            chat.streaming_thinking = None;
            chat.streaming_tool_calls.clear();
            chat.tool_event_summaries.clear();
            chat.tool_call_count = 0;
            chat.agent_edits.clear();
        }
        self.render_cache.ai_chat_last_total_rows = 0;
        self.render_cache.ai_chat_last_visible_start_row = 0;
        self.render_cache.ai_chat_last_visible_end_row = 0;
        self.render_cache.ai_chat_last_message_row_spans.clear();
        self.render_cache.ai_chat_rendered_text_rows.clear();
        self.render_cache.ai_chat_text_selection = None;
        self.render_cache.ai_chat_text_selecting = false;
        self.set_status_message("AI chat cleared".to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::{ChatOpts, ChatRole};

    fn open_test_chat(editor: &mut Editor) {
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".into(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
    }

    #[test]
    fn parses_supported_commands_and_rejects_bad_shapes() {
        assert_eq!(
            AiChatSlashCommand::parse("/clear"),
            Some(Ok(AiChatSlashCommand::Clear))
        );
        assert_eq!(
            AiChatSlashCommand::parse("/effort high"),
            Some(Ok(AiChatSlashCommand::Effort {
                effort: Some("high".into())
            }))
        );
        assert!(matches!(
            AiChatSlashCommand::parse("/effort enormous"),
            Some(Err(_))
        ));
        assert_eq!(
            AiChatSlashCommand::parse("/exa"),
            Some(Ok(AiChatSlashCommand::Exa))
        );
        assert_eq!(
            AiChatSlashCommand::parse("/model fast"),
            Some(Ok(AiChatSlashCommand::Model {
                profile: Some("fast".into())
            }))
        );
        assert_eq!(
            AiChatSlashCommand::parse("/yolo on"),
            Some(Ok(AiChatSlashCommand::Yolo {
                enabled: Some(true)
            }))
        );
        assert_eq!(
            AiChatSlashCommand::parse("/yolo"),
            Some(Ok(AiChatSlashCommand::Yolo { enabled: None }))
        );
        assert_eq!(
            AiChatSlashCommand::parse("/comprehension commit"),
            Some(Ok(AiChatSlashCommand::Comprehension {
                policy: crate::editor::ComprehensionPolicy::Commit
            }))
        );
        assert!(matches!(
            AiChatSlashCommand::parse("/comprehension eventually"),
            Some(Err(_))
        ));
        assert!(AiChatSlashCommand::parse("hello").is_none());
        assert!(matches!(
            AiChatSlashCommand::parse("/clear now"),
            Some(Err(_))
        ));
        assert!(matches!(
            AiChatSlashCommand::parse("/unknown"),
            Some(Err(_))
        ));
    }

    #[test]
    fn completion_is_derived_from_an_unfinished_command_name() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);

        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/".into();
        chat.input_cursor = 1;
        assert_eq!(editor.ai_chat_slash_completions().len(), 6);

        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/cl".into();
        chat.input_cursor = 3;
        assert_eq!(editor.ai_chat_slash_completions()[0].command, "/clear");

        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/clear".into();
        chat.input_cursor = chat.input.len();
        assert!(editor.ai_chat_slash_completions().is_empty());

        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/model f".into();
        chat.input_cursor = chat.input.len();
        assert!(editor.ai_chat_slash_completions().is_empty());
    }

    #[test]
    fn completion_acceptance_is_clamped_and_replaces_only_valid_fragments() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/".into();
        chat.input_cursor = 1;

        assert!(editor.accept_ai_chat_slash_completion(Some(usize::MAX)));
        assert_eq!(editor.ai_chat_input(), "/yolo");
        assert_eq!(editor.ai_chat_input_cursor(), "/yolo".len());
        assert!(editor.ai_chat_slash_completions().is_empty());
    }

    #[test]
    fn clear_resets_conversation_and_provider_context_generation() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor
            .conversation_mut()
            .expect("conversation")
            .append_user_message("old context".into());
        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/clear".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().expect("clear command");

        assert!(editor.ai_chat_messages().is_empty());
        assert!(editor.ai_chat_input().is_empty());
        assert_eq!(editor.ai_chat_state().expect("chat").context_generation, 1);
    }

    #[test]
    fn unknown_slash_command_is_not_submitted_as_a_message() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/cler".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().expect("unknown command");

        assert!(editor
            .ai_chat_messages()
            .iter()
            .all(|message| message.role != ChatRole::User));
        assert_eq!(editor.ai_chat_input(), "/cler");
    }

    #[test]
    fn yolo_command_toggles_per_chat_policy() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/yolo on".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().expect("enable yolo");
        assert!(editor.ai_chat_yolo_mode());
        assert!(editor.ai_chat_input().is_empty());

        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/yolo off".into();
        chat.input_cursor = chat.input.len();
        editor.submit_ai_chat_message().expect("disable yolo");
        assert!(!editor.ai_chat_yolo_mode());
    }

    #[test]
    fn effort_command_sets_and_clears_the_per_chat_override() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/effort high".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().expect("set effort");
        assert_eq!(editor.ai_chat_reasoning_effort(), "high");
        assert_eq!(
            editor
                .ai_state
                .chat
                .as_ref()
                .and_then(|chat| chat.reasoning_effort_override.as_deref()),
            Some("high")
        );

        let chat = editor.ai_state.chat.as_mut().expect("chat");
        chat.input = "/effort default".into();
        chat.input_cursor = chat.input.len();
        editor.submit_ai_chat_message().expect("inherit effort");
        assert!(editor
            .ai_state
            .chat
            .as_ref()
            .expect("chat")
            .reasoning_effort_override
            .is_none());
    }
}
