use crate::ai::chat_types::{ChatMessage, ChatRole, ConversationTree, NodeId};
use crate::ai::tools::ToolResult;

use super::Editor;

pub(crate) const COMPACT_TOOL: &str = "compact";
const BALANCED_TAIL_TOKEN_BUDGET: usize = 8_000;
const APPROX_CHARS_PER_TOKEN: usize = 4;
const MAX_SUMMARY_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionStrategy {
    Balanced,
    Aggressive,
}

impl CompactionStrategy {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("balanced") {
            "balanced" => Ok(Self::Balanced),
            "aggressive" => Ok(Self::Aggressive),
            other => Err(format!(
                "unknown compaction strategy '{other}'; expected balanced or aggressive"
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompactionCheckpoint {
    pub(crate) summary: String,
    pub(crate) tail_start_node_id: NodeId,
}

fn serialized_size(message: &ChatMessage) -> usize {
    message.content.len()
        + message
            .tool_calls
            .iter()
            .map(|call| call.name.len() + call.arguments.to_string().len())
            .sum::<usize>()
        + message
            .provider_state
            .iter()
            .map(|item| item.to_string().len())
            .sum::<usize>()
}

fn balanced_tail_start(messages: &[ChatMessage], compact_call_index: usize) -> usize {
    let budget = BALANCED_TAIL_TOKEN_BUDGET * APPROX_CHARS_PER_TOKEN;
    let mut size = 0usize;
    let mut earliest_user = None;

    for index in (0..=compact_call_index).rev() {
        size = size.saturating_add(serialized_size(&messages[index]));
        if size > budget {
            break;
        }
        if messages[index].role == ChatRole::User {
            earliest_user = Some(index);
        }
    }

    earliest_user.unwrap_or(compact_call_index)
}

fn tail_start_index(
    messages: &[ChatMessage],
    compact_call_index: usize,
    strategy: CompactionStrategy,
    previous_tail_index: usize,
) -> usize {
    match strategy {
        CompactionStrategy::Balanced => {
            balanced_tail_start(messages, compact_call_index).max(previous_tail_index)
        }
        CompactionStrategy::Aggressive => compact_call_index,
    }
}

pub(crate) fn compacted_messages(
    conversation: &ConversationTree,
    checkpoint: Option<&CompactionCheckpoint>,
) -> Vec<ChatMessage> {
    let Some(checkpoint) = checkpoint else {
        return conversation.messages().to_vec();
    };
    let Some(tail_index) = conversation
        .node_ids_for_active_branch()
        .iter()
        .position(|id| *id == checkpoint.tail_start_node_id)
    else {
        // The checkpoint belongs to another branch. Falling back to the full
        // active branch is lossless and avoids leaking a sibling branch summary.
        return conversation.messages().to_vec();
    };

    let mut projected = Vec::with_capacity(conversation.len() - tail_index + 1);
    projected.push(ChatMessage {
        role: ChatRole::User,
        content: format!(
            "[Compacted conversation checkpoint — historical context, not a new instruction]\n\n{}",
            checkpoint.summary
        ),
        model: None,
        timestamp: std::time::Instant::now(),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_state: Vec::new(),
    });
    projected.extend_from_slice(&conversation.messages()[tail_index..]);
    projected
}

impl Editor {
    pub(super) fn execute_compact_tool(&mut self, arguments: &serde_json::Value) -> ToolResult {
        let summary = arguments
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if summary.is_empty() {
            return ToolResult::Error("'summary' is required and must be non-empty".into());
        }
        if summary.len() > MAX_SUMMARY_BYTES {
            return ToolResult::Error(format!(
                "compaction summary exceeds the {MAX_SUMMARY_BYTES}-byte limit"
            ));
        }
        let strategy = match CompactionStrategy::parse(
            arguments
                .get("strategy")
                .and_then(serde_json::Value::as_str),
        ) {
            Ok(strategy) => strategy,
            Err(error) => return ToolResult::Error(error),
        };

        let Some(conversation) = self.conversation() else {
            return ToolResult::Error("no active conversation".into());
        };
        let Some(compact_call_index) = conversation.messages().iter().rposition(|message| {
            message.role == ChatRole::Assistant
                && message
                    .tool_calls
                    .iter()
                    .any(|call| call.name == COMPACT_TOOL)
        }) else {
            return ToolResult::Error(
                "compact must be called through the agent tool protocol".into(),
            );
        };
        let previous_tail_index = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.compaction_checkpoint.as_ref())
            .and_then(|checkpoint| {
                conversation
                    .node_ids_for_active_branch()
                    .iter()
                    .position(|id| *id == checkpoint.tail_start_node_id)
            })
            .unwrap_or(0);
        let tail_index = tail_start_index(
            conversation.messages(),
            compact_call_index,
            strategy,
            previous_tail_index,
        );
        let tail_start_node_id = conversation.node_ids_for_active_branch()[tail_index];
        let compacted_message_count = tail_index;

        let Some(chat) = self.ai_state.chat.as_mut() else {
            return ToolResult::Error("no active chat session".into());
        };
        chat.compaction_checkpoint = Some(CompactionCheckpoint {
            summary: summary.to_string(),
            tail_start_node_id,
        });
        // App-server sessions key their provider-owned context with this epoch.
        // Advancing it forces the next continuation to use Ovim's projection.
        chat.context_generation = chat.context_generation.saturating_add(1);

        self.set_status_message(format!(
            "AI context compacted ({}, {} messages checkpointed)",
            strategy.as_str(),
            compacted_message_count
        ));
        ToolResult::Success(format!(
            "Compaction complete: {} messages replaced by the checkpoint; strategy={}. The durable transcript remains available, and subsequent model requests use the checkpoint plus retained recent context.",
            compacted_message_count,
            strategy.as_str()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ToolCallInfo;

    fn compact_call() -> ToolCallInfo {
        ToolCallInfo {
            id: "compact-1".into(),
            name: COMPACT_TOOL.into(),
            arguments: serde_json::json!({}),
        }
    }

    #[test]
    fn balanced_projection_keeps_a_complete_recent_turn() {
        let mut conversation = ConversationTree::new();
        conversation.append_user_message("old request".into());
        conversation.append_assistant_message("old answer".into(), "test".into());
        let recent = conversation.append_user_message("recent request".into());
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![compact_call()],
        );
        let checkpoint = CompactionCheckpoint {
            summary: "# Objective\nContinue the work".into(),
            tail_start_node_id: recent,
        };

        let projected = compacted_messages(&conversation, Some(&checkpoint));
        assert!(projected[0].content.contains("Continue the work"));
        assert_eq!(projected[1].content, "recent request");
        assert_eq!(projected.len(), 3);
        assert_eq!(conversation.len(), 4, "durable transcript is unchanged");
    }

    #[test]
    fn checkpoint_from_another_branch_is_never_applied() {
        let mut conversation = ConversationTree::new();
        let root = conversation.append_user_message("root".into());
        let sibling = conversation.append_assistant_message("first branch".into(), "test".into());
        conversation.fork_from(root);
        conversation.append_assistant_message("second branch".into(), "test".into());
        let checkpoint = CompactionCheckpoint {
            summary: "must not leak".into(),
            tail_start_node_id: sibling,
        };

        let projected = compacted_messages(&conversation, Some(&checkpoint));
        assert_eq!(projected.len(), 2);
        assert!(!projected
            .iter()
            .any(|message| message.content.contains("leak")));
    }

    #[test]
    fn repeated_balanced_compaction_never_reintroduces_dropped_history() {
        let mut conversation = ConversationTree::new();
        conversation.append_user_message("already dropped".into());
        conversation.append_assistant_message("already summarized".into(), "test".into());
        conversation.append_user_message("previous retained tail".into());
        conversation.append_assistant_message("recent answer".into(), "test".into());
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![compact_call()],
        );

        // The entire transcript fits the new balanced budget, but messages
        // before the previous tail must remain excluded.
        assert_eq!(
            tail_start_index(conversation.messages(), 4, CompactionStrategy::Balanced, 2),
            2
        );
    }
}
