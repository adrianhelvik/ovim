use crate::ai::chat_types::ToolCallInfo;
use crate::ai::tools::ToolResult;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::ai_chat_state::{
    CodeExplanationContinuation, CodeExplanationExchange, CodeExplanationInteraction,
    PendingCodeExplanation, QueuedChatInputKind,
};
use super::code_explanation::{
    comment_rows_for_viewport, concept_body_row_limit, concept_body_rows_for_viewport,
    safe_code_rows, CodeExplanationDiscussionView, CodeExplanationPageView, CodeExplanationStep,
    CodeExplanationView, MAX_WALKTHROUGH_CACHE_BYTES, MAX_WALKTHROUGH_CACHE_ENTRIES,
    MAX_WALKTHROUGH_COMMENT_BYTES, MAX_WALKTHROUGH_COMMENT_ROWS,
    MAX_WALKTHROUGH_CONCEPT_BODY_BYTES, MAX_WALKTHROUGH_CONCEPT_TITLE_CHARS, MAX_WALKTHROUGH_STEPS,
    MIN_UNCACHEABLE_WALKTHROUGH_FILE_BYTES,
};
use super::Editor;

/// A newly submitted walkthrough question follows the newest wrapped answer
/// row. The renderer clamps this sentinel to its layout-dependent scroll bound;
/// the first manual scroll replaces it with an explicit offset.
const FOLLOW_LATEST_ANSWER: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeExplanationSourceMetrics {
    line_count: usize,
    visual_rows: Option<usize>,
    line_visual_rows: Option<Vec<(usize, usize)>>,
}

impl Editor {
    /// Conservative range that remains visible above the walkthrough card.
    /// The upper bound keeps the schema useful before the first render and on
    /// unusually tall terminals without encouraging broad, unfocused blocks.
    pub fn ai_code_explanation_safe_range_lines(&self) -> usize {
        let viewport = self
            .render_cache
            .last_buffer_area
            .map(|area| area.height as usize)
            .unwrap_or_else(|| self.viewport_height());
        safe_code_rows((viewport > 0).then_some(viewport))
    }

    pub fn ai_code_explanation_concept_page_rows(&self) -> usize {
        let viewport = self
            .render_cache
            .last_buffer_area
            .map(|area| area.height as usize)
            .unwrap_or_else(|| self.viewport_height());
        concept_body_row_limit((viewport > 0).then_some(viewport))
    }

    pub fn ai_code_explanation_view(&self) -> Option<CodeExplanationView> {
        let pending = self
            .ai_state
            .chat
            .as_ref()?
            .pending_code_explanation
            .as_ref()?;
        let step = pending.steps.get(pending.current)?;
        let exchanges = pending.threads.get(pending.current)?;
        let discussion = match &pending.interaction {
            CodeExplanationInteraction::Composing { input, cursor } => {
                CodeExplanationDiscussionView::Composing {
                    input: input.clone(),
                    cursor: *cursor,
                    question_count: exchanges.len(),
                }
            }
            CodeExplanationInteraction::Answering { step, exchange }
                if *step == pending.current =>
            {
                let exchange = pending.threads.get(*step)?.get(*exchange)?;
                CodeExplanationDiscussionView::Answering {
                    question: exchange.question.clone(),
                    answer: exchange.answer.clone(),
                    question_count: pending.threads[*step].len(),
                }
            }
            _ => {
                let latest = exchanges.last();
                CodeExplanationDiscussionView::Navigating {
                    question_count: exchanges.len(),
                    latest_question: latest.map(|exchange| exchange.question.clone()),
                    latest_answer: latest.map(|exchange| exchange.answer.clone()),
                    latest_failed: latest.is_some_and(|exchange| exchange.failed),
                }
            }
        };
        let page = match step {
            CodeExplanationStep::Concept { title, body } => CodeExplanationPageView::Concept {
                title: title.clone(),
                body: body.clone(),
            },
            CodeExplanationStep::Code {
                path,
                start_line,
                end_line,
                comment,
                ..
            } => CodeExplanationPageView::Code {
                path: path.clone(),
                start_line: *start_line,
                end_line: *end_line,
                comment: comment.clone(),
            },
        };
        Some(CodeExplanationView {
            current: pending.current + 1,
            total: pending.steps.len(),
            answer_scroll: pending.answer_scroll,
            page,
            discussion,
        })
    }

    pub fn ai_chat_has_pending_code_explanation(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.pending_code_explanation.is_some())
    }

    /// Whether the visible buffer is the detached, read-only source snapshot
    /// created for a walkthrough code page.
    ///
    /// This buffer is deliberately not an LSP document. Diagnostics and inlay
    /// hints cached for the live file must therefore not be projected onto it,
    /// even when line numbers happen to overlap.
    pub fn ai_code_explanation_is_presenting_snapshot(&self) -> bool {
        let current_id = self.buffer().id();
        self.ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_code_explanation.as_ref())
            .and_then(|pending| pending.presentation_buffer_id)
            == Some(current_id)
    }

    fn cached_code_explanation_steps(
        &mut self,
        tool_call_id: &str,
    ) -> Option<Vec<CodeExplanationStep>> {
        let chat = self.ai_state.chat.as_mut()?;
        let position = chat
            .code_explanation_cache
            .iter()
            .position(|entry| entry.tool_call_id == tool_call_id)?;
        let entry = chat.code_explanation_cache.remove(position)?;
        let steps = entry.steps.clone();
        chat.code_explanation_cache.push_back(entry);
        Some(steps)
    }

    fn retain_code_explanation_steps(&mut self, tool_call_id: &str, steps: &[CodeExplanationStep]) {
        let mut seen = HashSet::new();
        let bytes = steps
            .iter()
            .filter_map(|step| match step {
                CodeExplanationStep::Code {
                    absolute_path,
                    snapshot: Some(snapshot),
                    ..
                } if seen.insert(absolute_path.clone()) => Some(snapshot.len()),
                _ => None,
            })
            .sum::<usize>();
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return;
        };

        if let Some(position) = chat
            .code_explanation_cache
            .iter()
            .position(|entry| entry.tool_call_id == tool_call_id)
        {
            if let Some(previous) = chat.code_explanation_cache.remove(position) {
                chat.code_explanation_cache_bytes = chat
                    .code_explanation_cache_bytes
                    .saturating_sub(previous.bytes);
            }
        }
        if bytes > MAX_WALKTHROUGH_CACHE_BYTES {
            return;
        }
        while chat.code_explanation_cache_bytes.saturating_add(bytes) > MAX_WALKTHROUGH_CACHE_BYTES
            || chat.code_explanation_cache.len() >= MAX_WALKTHROUGH_CACHE_ENTRIES
        {
            let Some(evicted) = chat.code_explanation_cache.pop_front() else {
                break;
            };
            chat.code_explanation_cache_bytes = chat
                .code_explanation_cache_bytes
                .saturating_sub(evicted.bytes);
        }
        chat.code_explanation_cache
            .push_back(super::ai_chat_state::CachedCodeExplanation {
                tool_call_id: tool_call_id.to_string(),
                steps: steps.to_vec(),
                bytes,
            });
        chat.code_explanation_cache_bytes = chat.code_explanation_cache_bytes.saturating_add(bytes);
    }

    /// Replay a completed walkthrough from its retained invocation snapshot.
    /// Evicted entries and deliberately uncached large files fall back to the
    /// current workspace and are validated again.
    pub fn replay_code_explanation(&mut self, tool_call_id: &str) -> bool {
        if self.ai_chat_waiting() || self.ai_chat_has_pending_code_explanation() {
            self.set_status_message("Finish the active agent work before replaying a walkthrough");
            return false;
        }
        let Some(tool_call) = self
            .ai_chat_tool_event_call(tool_call_id)
            .filter(|call| call.name == "explain_with_codebase")
            .cloned()
        else {
            self.set_status_message("That walkthrough is no longer available to replay");
            return false;
        };

        match self.begin_code_explanation(tool_call, CodeExplanationContinuation::Replay) {
            Ok(()) => true,
            Err((error, _)) => {
                let message = match error {
                    ToolResult::Success(message) | ToolResult::Error(message) => message,
                };
                self.set_status_message(format!("Could not replay walkthrough: {message}"));
                false
            }
        }
    }

    pub(super) fn begin_code_explanation(
        &mut self,
        tool_call: ToolCallInfo,
        continuation: CodeExplanationContinuation,
    ) -> Result<(), (ToolResult, Box<CodeExplanationContinuation>)> {
        let cached_steps = matches!(continuation, CodeExplanationContinuation::Replay)
            .then(|| self.cached_code_explanation_steps(&tool_call.id))
            .flatten();
        let steps = if let Some(steps) = cached_steps {
            steps
        } else {
            match self.parse_code_explanation_steps(&tool_call.arguments) {
                Ok(steps) => steps,
                Err(error) => return Err((error, Box::new(continuation))),
            }
        };
        self.retain_code_explanation_steps(&tool_call.id, &steps);
        let original_active_buffer_id = self
            .ai_state
            .chat
            .as_ref()
            .map(|chat| chat.active_buffer_id);
        let Some(original_active_buffer_id) = original_active_buffer_id else {
            return Err((
                ToolResult::Error("AI chat is not open".to_string()),
                Box::new(continuation),
            ));
        };

        let Some(chat) = self.ai_state.chat.as_mut() else {
            return Err((
                ToolResult::Error("AI chat is not open".to_string()),
                Box::new(continuation),
            ));
        };
        chat.pending_code_explanation = Some(PendingCodeExplanation {
            tool_call,
            threads: vec![Vec::new(); steps.len()],
            steps,
            current: 0,
            answer_scroll: 0,
            interaction: CodeExplanationInteraction::Navigating,
            original_active_buffer_id,
            presentation_buffer_id: None,
            continuation: Some(continuation),
        });
        chat.waiting = false;

        if let Err(error) = self.show_current_code_explanation_step() {
            if let Some(pending) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_code_explanation.take())
            {
                self.discard_code_explanation_presentation_buffer(
                    pending.presentation_buffer_id,
                    pending.original_active_buffer_id,
                );
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.active_buffer_id = original_active_buffer_id;
                }
                return Err((
                    error,
                    Box::new(
                        pending
                            .continuation
                            .expect("new walkthrough must retain its continuation"),
                    ),
                ));
            } else if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.active_buffer_id = original_active_buffer_id;
            }
            unreachable!("installed walkthrough disappeared before activation");
        }

        self.ai_state.ai_attention_generation =
            self.ai_state.ai_attention_generation.saturating_add(1);
        self.set_status_message(
            "Walkthrough ready — Left/Right pages, Space asks, Enter advances, Esc dismisses",
        );
        Ok(())
    }

    pub fn move_code_explanation(&mut self, forward: bool) -> bool {
        let changed = {
            let Some(pending) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_code_explanation.as_mut())
            else {
                return false;
            };
            let next = if forward {
                (pending.current + 1).min(pending.steps.len().saturating_sub(1))
            } else {
                pending.current.saturating_sub(1)
            };
            if next == pending.current {
                false
            } else {
                pending.current = next;
                pending.answer_scroll = 0;
                true
            }
        };
        if changed {
            if let Err(error) = self.show_current_code_explanation_step() {
                self.set_status_message(format!("Could not show walkthrough step: {error:?}"));
            }
        }
        changed
    }

    pub fn begin_code_explanation_question(&mut self) -> bool {
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        else {
            return false;
        };
        if !matches!(pending.interaction, CodeExplanationInteraction::Navigating) {
            return false;
        }
        pending.interaction = CodeExplanationInteraction::Composing {
            input: String::new(),
            cursor: 0,
        };
        pending.answer_scroll = 0;
        self.set_status_message(
            "Ask about this walkthrough step — Enter sends, Shift-Enter adds a line, Esc cancels",
        );
        true
    }

    pub fn cancel_code_explanation_question(&mut self) -> bool {
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        else {
            return false;
        };
        if !matches!(
            pending.interaction,
            CodeExplanationInteraction::Composing { .. }
        ) {
            return false;
        }
        pending.interaction = CodeExplanationInteraction::Navigating;
        self.set_status_message("Cancelled walkthrough question");
        true
    }

    pub fn insert_code_explanation_question_char(&mut self, character: char) -> bool {
        let Some(CodeExplanationInteraction::Composing { input, cursor }) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
            .map(|pending| &mut pending.interaction)
        else {
            return false;
        };
        input.insert(*cursor, character);
        *cursor += character.len_utf8();
        true
    }

    pub fn backspace_code_explanation_question(&mut self) -> bool {
        let Some(CodeExplanationInteraction::Composing { input, cursor }) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
            .map(|pending| &mut pending.interaction)
        else {
            return false;
        };
        let Some(previous) = input[..*cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
        else {
            return true;
        };
        input.drain(previous..*cursor);
        *cursor = previous;
        true
    }

    pub fn move_code_explanation_question_cursor(&mut self, forward: bool) -> bool {
        let Some(CodeExplanationInteraction::Composing { input, cursor }) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
            .map(|pending| &mut pending.interaction)
        else {
            return false;
        };
        if forward {
            if *cursor < input.len() {
                *cursor = input[*cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(offset, _)| *cursor + offset)
                    .unwrap_or(input.len());
            }
        } else if *cursor > 0 {
            *cursor = input[..*cursor]
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0);
        }
        true
    }

    pub fn submit_code_explanation_question(&mut self) -> Result<bool, String> {
        let (step_index, step, question, tool_call, continuation) = {
            let Some(pending) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.pending_code_explanation.as_mut())
            else {
                return Ok(false);
            };
            let CodeExplanationInteraction::Composing { input, .. } = &pending.interaction else {
                return Ok(false);
            };
            let question = input.trim().to_string();
            if question.is_empty() {
                return Err("walkthrough question cannot be empty".into());
            }
            let step_index = pending.current;
            let step = pending
                .steps
                .get(step_index)
                .cloned()
                .ok_or_else(|| "walkthrough step is no longer available".to_string())?;
            let exchange = pending.threads[step_index].len();
            pending.threads[step_index].push(CodeExplanationExchange {
                question: question.clone(),
                answer: String::new(),
                failed: false,
            });
            pending.interaction = CodeExplanationInteraction::Answering {
                step: step_index,
                exchange,
            };
            pending.answer_scroll = FOLLOW_LATEST_ANSWER;
            (
                step_index,
                step,
                question,
                pending.tool_call.clone(),
                pending.continuation.take(),
            )
        };

        let prompt = walkthrough_question_prompt(step_index, &step, &question);
        let outcome = ToolResult::Success(format!(
            "The user paused the walkthrough at page {} to ask a question. Answer the attached user steering directly and concisely, using read-only investigation if needed. Do not edit files, restart the walkthrough, or continue implementation. Question: {}",
            step_index + 1,
            question
        ));

        match continuation {
            Some(CodeExplanationContinuation::Replay) | None => {
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.input = prompt;
                    chat.input_cursor = chat.input.len();
                }
                self.submit_ai_chat_message()
                    .map_err(|error| error.to_string())?;
            }
            Some(continuation) => {
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.input = prompt;
                    chat.input_cursor = chat.input.len();
                }
                // `continuation` has already been taken out of `pending`, so it
                // must reach resolve_* on every path out of here — a dropped
                // Batch never emits a tool_result for the explain_with_codebase
                // tool_use (malforming the next request) and a dropped Dynamic
                // loses its oneshot sender, with no way back: finish_code_
                // explanation would see `continuation == None`. Queueing is
                // infallible, so that is guaranteed by construction rather than
                // by error handling.
                self.queue_current_ai_chat_input(QueuedChatInputKind::Steer);
                self.resolve_code_explanation_continuation(&tool_call, continuation, outcome);
            }
        }
        self.set_status_message(format!(
            "Answering walkthrough question for step {}",
            step_index + 1
        ));
        Ok(true)
    }

    pub(crate) fn append_code_explanation_answer(&mut self, content: &str) {
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        else {
            return;
        };
        let CodeExplanationInteraction::Answering { step, exchange } = pending.interaction else {
            return;
        };
        if let Some(answer) = pending
            .threads
            .get_mut(step)
            .and_then(|thread| thread.get_mut(exchange))
        {
            answer.answer.push_str(content);
        }
    }

    pub(crate) fn finish_code_explanation_answer(&mut self, error: Option<&str>) {
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        else {
            return;
        };
        let CodeExplanationInteraction::Answering { step, exchange } = pending.interaction else {
            return;
        };
        if let Some(answer) = pending
            .threads
            .get_mut(step)
            .and_then(|thread| thread.get_mut(exchange))
        {
            if let Some(error) = error {
                answer.failed = true;
                if answer.answer.is_empty() {
                    answer.answer = error.to_string();
                }
            } else if answer.answer.trim().is_empty() {
                answer.failed = true;
                answer.answer = "The agent completed without an answer.".into();
            }
        }
        pending.interaction = CodeExplanationInteraction::Navigating;
        self.set_status_message(
            "Walkthrough answer ready — Up/Down reads the reply; Enter continues",
        );
    }

    pub(crate) fn ai_code_explanation_answering(&self) -> bool {
        self.ai_state.chat.as_ref().is_some_and(|chat| {
            chat.pending_code_explanation
                .as_ref()
                .is_some_and(|pending| {
                    matches!(
                        pending.interaction,
                        CodeExplanationInteraction::Answering { .. }
                    )
                })
        })
    }

    /// Scrolls the answer rendered inside the walkthrough card. The renderer
    /// supplies the exact bound because Markdown styling affects wrapping.
    pub fn scroll_code_explanation_answer(&mut self, forward: bool) -> bool {
        let max_scroll = self.render_cache.code_explanation_answer_max_scroll;
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        else {
            return false;
        };
        let current = pending.answer_scroll.min(max_scroll);
        let next = if forward {
            current.saturating_add(1).min(max_scroll)
        } else {
            current.saturating_sub(1)
        };
        pending.answer_scroll = next;
        next != current
    }

    /// Enter advances through a walkthrough and only unblocks the agent from
    /// the final step. This avoids completing a multi-step explanation with
    /// one accidental key press on its first card.
    pub fn advance_or_finish_code_explanation(&mut self) -> bool {
        let is_last = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_code_explanation.as_ref())
            .is_some_and(|pending| pending.current + 1 >= pending.steps.len());
        if is_last {
            self.finish_code_explanation(false)
        } else {
            self.move_code_explanation(true)
        }
    }

    pub fn finish_code_explanation(&mut self, dismissed: bool) -> bool {
        let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.take())
        else {
            return false;
        };

        self.discard_code_explanation_presentation_buffer(
            pending.presentation_buffer_id,
            pending.original_active_buffer_id,
        );
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = pending.original_active_buffer_id;
        }
        self.ai_state.active_selection = None;

        let is_replay = matches!(
            pending.continuation.as_ref(),
            Some(CodeExplanationContinuation::Replay)
        );
        let question_count = pending.threads.iter().map(Vec::len).sum::<usize>();
        let outcome = if is_replay && dismissed {
            format!(
                "Dismissed replay at page {} of {}.",
                pending.current + 1,
                pending.steps.len()
            )
        } else if is_replay {
            format!(
                "Completed walkthrough replay ({} pages).",
                pending.steps.len()
            )
        } else if pending.continuation.is_none() && dismissed {
            format!(
                "Dismissed walkthrough at page {} of {} after {} question(s).",
                pending.current + 1,
                pending.steps.len(),
                question_count
            )
        } else if pending.continuation.is_none() {
            format!(
                "Completed walkthrough ({} pages, {} question(s)).",
                pending.steps.len(),
                question_count
            )
        } else if dismissed {
            format!(
                "User dismissed the walkthrough at page {} of {}.",
                pending.current + 1,
                pending.steps.len()
            )
        } else {
            format!(
                "User completed the walkthrough ({} pages).",
                pending.steps.len()
            )
        };
        let result = ToolResult::Success(outcome.clone());

        if let Some(continuation) = pending.continuation {
            self.resolve_code_explanation_continuation(&pending.tool_call, continuation, result);
        }

        self.set_status_message(outcome);
        true
    }

    fn resolve_code_explanation_continuation(
        &mut self,
        tool_call: &ToolCallInfo,
        continuation: CodeExplanationContinuation,
        result: ToolResult,
    ) {
        match continuation {
            CodeExplanationContinuation::Dynamic {
                runtime_tool,
                runtime_turn,
                response,
            } => {
                self.finish_dynamic_tool(&runtime_turn, &runtime_tool, tool_call, response, result);
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.waiting = true;
                }
            }
            CodeExplanationContinuation::Batch {
                runtime_tool,
                runtime_turn,
                remaining_tool_calls,
                model_name,
            } => {
                if let (Some(turn), Some(tool)) = (runtime_turn.as_ref(), runtime_tool.as_ref()) {
                    if let Err(error) = self.ai_runtime_finish_tool(turn, tool, &result) {
                        self.ai_runtime_fail_turn(format!(
                            "failed to record walkthrough result: {error}"
                        ));
                        self.clear_streaming_state();
                        return;
                    }
                }
                self.record_tool_event_summary(tool_call, &result);
                let result_content = self.format_tool_result_with_target(tool_call, &result);
                if let Some(conversation) = self.conversation_mut() {
                    conversation.append_tool_result(tool_call.id.clone(), result_content);
                }
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                    chat.waiting = true;
                }
                self.execute_tool_call_batch(remaining_tool_calls, model_name);
            }
            CodeExplanationContinuation::Replay => {}
        }
    }

    fn parse_code_explanation_steps(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<Vec<CodeExplanationStep>, ToolResult> {
        let raw_steps = arguments
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ToolResult::Error("'steps' must be a non-empty array".to_string()))?;
        if raw_steps.is_empty() {
            return Err(ToolResult::Error(
                "'steps' must contain at least one walkthrough step".to_string(),
            ));
        }
        if raw_steps.len() > MAX_WALKTHROUGH_STEPS {
            return Err(ToolResult::Error(format!(
                "walkthrough has {} pages; the maximum is {MAX_WALKTHROUGH_STEPS}",
                raw_steps.len()
            )));
        }

        let root = self
            .ai_effective_project_root()
            .map(|root| root.canonicalize().unwrap_or(root));
        let safe_range = self.ai_code_explanation_safe_range_lines();
        let presentation_width = self.ai_code_explanation_presentation_width();
        let presentation_height = self
            .render_cache
            .last_buffer_area
            .map(|area| area.height as usize)
            .filter(|height| *height > 0)
            .or_else(|| (self.viewport_height() > 0).then(|| self.viewport_height()));
        let mut steps = Vec::with_capacity(raw_steps.len());
        let mut snapshots: HashMap<PathBuf, Arc<str>> = HashMap::new();
        // Large sources are shared only during validation, then dropped. Their
        // pages intentionally read the file again when displayed.
        let mut uncacheable_sources: HashMap<PathBuf, String> = HashMap::new();

        for (index, raw) in raw_steps.iter().enumerate() {
            let step_number = index + 1;
            let page_type = raw
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("code");
            if page_type == "concept" {
                let title = required_step_string(raw, "title", step_number)?;
                if title.contains(['\n', '\r']) {
                    return Err(ToolResult::Error(format!(
                        "step {step_number} concept title must be a single line"
                    )));
                }
                if title.chars().count() > MAX_WALKTHROUGH_CONCEPT_TITLE_CHARS {
                    return Err(ToolResult::Error(format!(
                        "step {step_number} concept title exceeds {MAX_WALKTHROUGH_CONCEPT_TITLE_CHARS} characters"
                    )));
                }
                let body = required_step_string(raw, "body", step_number)?;
                if body.len() > MAX_WALKTHROUGH_CONCEPT_BODY_BYTES {
                    return Err(ToolResult::Error(format!(
                        "step {step_number} concept body exceeds {MAX_WALKTHROUGH_CONCEPT_BODY_BYTES} bytes"
                    )));
                }
                let body_rows = concept_body_rows_for_viewport(
                    presentation_width,
                    &body,
                    self.options.tab_width,
                );
                let row_limit = concept_body_row_limit(presentation_height);
                if body_rows > row_limit {
                    let suggested_pages = body_rows.div_ceil(row_limit);
                    return Err(ToolResult::Error(format!(
                        "step {step_number} concept body wraps to {body_rows} rows; the current maximum is {row_limit}. Split it semantically into at least {suggested_pages} focused concept pages rather than truncating or compressing it"
                    )));
                }
                steps.push(CodeExplanationStep::Concept { title, body });
                continue;
            }
            if page_type != "code" {
                return Err(ToolResult::Error(format!(
                    "step {step_number} has unsupported type {page_type:?}; expected 'concept' or 'code'"
                )));
            }
            let path = required_step_string(raw, "path", step_number)?;
            let comment = required_step_string(raw, "comment", step_number)?;
            if comment.len() > MAX_WALKTHROUGH_COMMENT_BYTES {
                return Err(ToolResult::Error(format!(
                    "step {step_number} comment exceeds {MAX_WALKTHROUGH_COMMENT_BYTES} bytes"
                )));
            }
            let comment_rows =
                comment_rows_for_viewport(presentation_width, &comment, self.options.tab_width);
            if comment_rows > MAX_WALKTHROUGH_COMMENT_ROWS {
                return Err(ToolResult::Error(format!(
                    "step {step_number} comment wraps to {comment_rows} rows; keep it within {MAX_WALKTHROUGH_COMMENT_ROWS} rows or split the explanation into focused steps"
                )));
            }
            let start_line = required_step_line(raw, "start_line", step_number)?;
            let end_line = raw
                .get("end_line")
                .and_then(serde_json::Value::as_u64)
                .map(|line| line as usize)
                .unwrap_or(start_line);
            if end_line < start_line {
                return Err(ToolResult::Error(format!(
                    "step {step_number} end_line ({end_line}) must be >= start_line ({start_line})"
                )));
            }
            let range_lines = end_line - start_line + 1;
            if range_lines > safe_range {
                return Err(ToolResult::Error(format!(
                    "step {step_number} '{path}:{start_line}-{end_line}' spans {range_lines} physical lines, but the current maximum is {safe_range}; split it into smaller conceptual steps"
                )));
            }

            let root = root
                .as_ref()
                .ok_or_else(|| ToolResult::Error(self.no_project_root_error()))?;
            let supplied_path = Path::new(&path);
            if supplied_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(ToolResult::Error(format!(
                    "step {step_number} path must not contain '..': {path}"
                )));
            }
            let candidate = if supplied_path.is_absolute() {
                supplied_path.to_path_buf()
            } else {
                root.join(supplied_path)
            };
            let absolute_path = candidate.canonicalize().map_err(|error| {
                ToolResult::Error(format!("step {step_number} cannot open '{path}': {error}"))
            })?;
            if !absolute_path.is_file() {
                return Err(ToolResult::Error(format!(
                    "step {step_number} path is not a file: {path}"
                )));
            }
            if !absolute_path.starts_with(root)
                && !self.ai_chat_yolo_mode()
                && self
                    .current_session_approved_root_for(&absolute_path)
                    .is_none()
            {
                return Err(ToolResult::Error(format!(
                    "step {step_number} path is outside the project and has not been approved for this chat: {path}. Use read_file_at_path or list_files with the absolute path to request access, then retry the walkthrough"
                )));
            }
            let wrap_width =
                (self.options.wrap && self.render_cache.last_text_width > 0).then(|| {
                    self.render_cache.last_text_width.saturating_add(
                        self.render_cache
                            .last_chat_area
                            .map_or(0, |area| area.width as usize),
                    )
                });
            if !snapshots.contains_key(&absolute_path)
                && !uncacheable_sources.contains_key(&absolute_path)
            {
                let content = self
                    .capture_code_explanation_source(&absolute_path)
                    .map_err(|error| {
                        ToolResult::Error(format!(
                            "step {step_number} cannot read '{path}': {error}"
                        ))
                    })?;
                if content.len() >= MIN_UNCACHEABLE_WALKTHROUGH_FILE_BYTES {
                    uncacheable_sources.insert(absolute_path.clone(), content);
                } else {
                    snapshots.insert(absolute_path.clone(), Arc::from(content));
                }
            }
            let snapshot = snapshots.get(&absolute_path).cloned();
            let source = snapshot
                .as_deref()
                .or_else(|| uncacheable_sources.get(&absolute_path).map(String::as_str));
            let source = source.expect("validated source must remain available during parsing");
            let metrics =
                self.code_explanation_source_metrics(source, start_line, end_line, wrap_width);
            if start_line > metrics.line_count {
                return Err(ToolResult::Error(format!(
                    "step {step_number} start_line ({start_line}) exceeds '{path}' line count ({})",
                    metrics.line_count
                )));
            }
            let end_line = end_line.min(metrics.line_count);
            if let Some(visual_rows) = metrics.visual_rows {
                if visual_rows > safe_range {
                    let guidance = metrics
                        .line_visual_rows
                        .as_deref()
                        .map(|rows| visual_overflow_guidance(rows, safe_range))
                        .unwrap_or_default();
                    return Err(ToolResult::Error(format!(
                        "step {step_number} '{path}:{start_line}-{end_line}' occupies {visual_rows} visual rows after soft wrapping (maximum {safe_range}).{guidance} Split by concept rather than truncating the explanation arbitrarily."
                    )));
                }
            }

            steps.push(CodeExplanationStep::Code {
                path,
                absolute_path,
                snapshot,
                start_line,
                end_line,
                comment,
            });
        }
        Ok(steps)
    }

    fn ai_code_explanation_presentation_width(&self) -> Option<u16> {
        self.render_cache.last_buffer_area.map(|buffer| {
            buffer.width.saturating_add(
                self.render_cache
                    .last_chat_area
                    .map_or(0, |chat| chat.width),
            )
        })
    }

    fn capture_code_explanation_source(&self, path: &Path) -> Result<String, String> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(buffer) = self.buffers.iter().find(|buffer| {
            buffer.file_path().is_some_and(|candidate| {
                PathBuf::from(candidate)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(candidate))
                    == canonical
            })
        }) {
            return Ok(buffer.rope().to_string());
        }

        const STABLE_READ_ATTEMPTS: usize = 3;
        for _ in 0..STABLE_READ_ATTEMPTS {
            let first = std::fs::read(path).map_err(|error| error.to_string())?;
            let second = std::fs::read(path).map_err(|error| error.to_string())?;
            if first != second {
                continue;
            }
            let (encoding, bom_offset) = crate::buffer::FileEncoding::detect(&second);
            let content = encoding
                .decode(&second, bom_offset)
                .map_err(|error| error.to_string())?;
            return Ok(crate::buffer::normalize_for_buffer(&content).into_owned());
        }

        Err("file changed repeatedly while it was being captured".to_string())
    }

    fn code_explanation_source_metrics(
        &self,
        content: &str,
        start_line: usize,
        end_line: usize,
        wrap_width: Option<usize>,
    ) -> CodeExplanationSourceMetrics {
        let lines = content.split('\n').collect::<Vec<_>>();
        let line_count = lines.len().max(1);
        let line_visual_rows = wrap_width.map(|width| {
            (start_line.saturating_sub(1)..end_line.min(line_count))
                .map(|line| {
                    (
                        line + 1,
                        crate::wrap::visual_line_count(
                            lines.get(line).copied().unwrap_or(""),
                            width,
                            self.options.tab_width,
                        ),
                    )
                })
                .collect::<Vec<_>>()
        });
        let visual_rows = line_visual_rows
            .as_ref()
            .map(|rows| rows.iter().map(|(_, count)| count).sum());
        CodeExplanationSourceMetrics {
            line_count,
            visual_rows,
            line_visual_rows,
        }
    }

    fn show_current_code_explanation_step(&mut self) -> Result<(), ToolResult> {
        let (step, previous_buffer_id, original_buffer_id) = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_code_explanation.as_ref())
            .and_then(|pending| {
                pending.steps.get(pending.current).cloned().map(|step| {
                    (
                        step,
                        pending.presentation_buffer_id,
                        pending.original_active_buffer_id,
                    )
                })
            })
            .ok_or_else(|| ToolResult::Error("walkthrough has no current page".to_string()))?;

        self.discard_code_explanation_presentation_buffer(previous_buffer_id, original_buffer_id);
        if let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        {
            pending.presentation_buffer_id = None;
        }

        let CodeExplanationStep::Code {
            absolute_path,
            snapshot,
            start_line,
            end_line,
            ..
        } = step
        else {
            self.ai_state.active_selection = None;
            return Ok(());
        };

        let source = match snapshot {
            Some(snapshot) => snapshot,
            None => Arc::from(
                self.capture_code_explanation_source(&absolute_path)
                    .map_err(|error| {
                        ToolResult::Error(format!(
                            "cannot read uncached walkthrough source '{}': {error}",
                            absolute_path.display()
                        ))
                    })?,
            ),
        };
        let mut buffer = crate::buffer::Buffer::new_from_str(&source);
        buffer.set_read_only(true);
        buffer.enable_syntax_highlighting_for_path(&absolute_path.to_string_lossy());
        // Label the snapshot with the source file's name so the tab bar and
        // status line don't show "[No Name]" — without giving it a file path,
        // which would make it an LSP document and a save target.
        if let Some(name) = absolute_path.file_name().and_then(|n| n.to_str()) {
            buffer.set_display_name(name);
        }
        let presentation_buffer_id = buffer.id();
        self.add_buffer(buffer);
        // The virtual snapshot is intentionally not an LSP document or a save target.
        self.lsp.state.needs_lsp_init = false;
        if let Some(pending) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_explanation.as_mut())
        {
            pending.presentation_buffer_id = Some(presentation_buffer_id);
        }

        let selected = self.execute_navigation_tool(
            "select_text",
            &json!({
                "start_line": start_line,
                "end_line": end_line,
            }),
        );
        if let ToolResult::Error(error) = selected {
            self.discard_code_explanation_presentation_buffer(
                Some(presentation_buffer_id),
                original_buffer_id,
            );
            return Err(ToolResult::Error(error));
        }
        // `select_text` centers the midpoint for general navigation. A
        // walkthrough instead owns the bottom rows with its card, so pin the
        // range's first line to the top and let the validated visual-row budget
        // flow downward without being obscured.
        self.buffer_mut().cursor_mut().set_position(
            start_line.saturating_sub(1),
            crate::unicode::GraphemeCol::ZERO,
        );
        self.move_cursor_line_to_top_with_offset(0);
        Ok(())
    }

    pub(super) fn discard_code_explanation_presentation_buffer(
        &mut self,
        buffer_id: Option<crate::buffer::BufferId>,
        fallback_buffer_id: crate::buffer::BufferId,
    ) {
        let Some(buffer_id) = buffer_id else {
            return;
        };
        let Some(index) = self.find_buffer_index_by_id(buffer_id) else {
            return;
        };
        self.buffers.remove(index);
        self.current_buffer_index = self
            .find_buffer_index_by_id(fallback_buffer_id)
            .unwrap_or_else(|| index.min(self.buffers.len().saturating_sub(1)));
        self.lsp.state.needs_lsp_init = self.buffer().file_path().is_some();
        self.mark_dirty();
    }
}

fn visual_overflow_guidance(line_rows: &[(usize, usize)], safe_range: usize) -> String {
    let mut used = 0usize;
    let mut safe_endpoint = None;
    for (line, rows) in line_rows {
        if used.saturating_add(*rows) > safe_range {
            break;
        }
        used = used.saturating_add(*rows);
        safe_endpoint = Some(*line);
    }

    let endpoint = if let Some(line) = safe_endpoint {
        format!(" Suggested safe endpoint from this start: line {line}.")
    } else if let Some((line, rows)) = line_rows.first() {
        format!(" No endpoint from this start fits: line {line} alone occupies {rows} visual rows.")
    } else {
        String::new()
    };

    let mut longest = line_rows
        .iter()
        .copied()
        .filter(|(_, rows)| *rows > 1)
        .collect::<Vec<_>>();
    longest.sort_by_key(|(line, rows)| (std::cmp::Reverse(*rows), *line));
    let longest = longest
        .into_iter()
        .take(3)
        .map(|(line, rows)| format!("{line} ({rows} rows)"))
        .collect::<Vec<_>>();
    let longest = if longest.is_empty() {
        String::new()
    } else {
        format!(" Longest wrapped lines: {}.", longest.join(", "))
    };

    format!("{endpoint}{longest}")
}

fn walkthrough_question_prompt(
    step_index: usize,
    step: &CodeExplanationStep,
    question: &str,
) -> String {
    let (label, teaching_text) = match step {
        CodeExplanationStep::Concept { title, body } => {
            (format!("Concept: {title}"), body.as_str())
        }
        CodeExplanationStep::Code {
            path,
            start_line,
            end_line,
            comment,
            ..
        } => {
            let range = if start_line == end_line {
                format!("{path}:{start_line}")
            } else {
                format!("{path}:{start_line}-{end_line}")
            };
            (range, comment.as_str())
        }
    };
    let quoted_comment = teaching_text
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "> Walkthrough page {} · {label}\n{quoted_comment}\n\n{}\n\nAnswer this question in the context of our existing conversation and the codebase. Do not modify files or perform external actions; use read-only investigation only if needed.",
        step_index + 1,
        question
    )
}

fn required_step_string(
    raw: &serde_json::Value,
    field: &str,
    step: usize,
) -> Result<String, ToolResult> {
    raw.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolResult::Error(format!("step {step} requires non-empty '{field}'")))
}

fn required_step_line(
    raw: &serde_json::Value,
    field: &str,
    step: usize,
) -> Result<usize, ToolResult> {
    raw.get(field)
        .and_then(serde_json::Value::as_u64)
        .filter(|line| *line > 0)
        .map(|line| line as usize)
        .ok_or_else(|| ToolResult::Error(format!("step {step} requires '{field}' >= 1")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ai::chat_types::{ChatOpts, ProviderSteerUpdate, StreamChunk};

    fn setup_editor() -> (tempfile::TempDir, Editor, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("git marker");
        let first = dir.path().join("first.rs");
        let second = dir.path().join("second.rs");
        std::fs::write(
            &first,
            (1..=50)
                .map(|line| format!("// first {line}\n"))
                .collect::<String>(),
        )
        .expect("first file");
        std::fs::write(
            &second,
            (1..=60)
                .map(|line| format!("// second {line}\n"))
                .collect::<String>(),
        )
        .expect("second file");

        let mut editor = Editor::default();
        editor.open_file(&first).expect("open first");
        editor
            .open_ai_chat(ChatOpts {
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        let profile = editor.ai_state.active_profile.clone();
        editor
            .ai_state
            .config
            .profiles
            .get_mut(&profile)
            .expect("active profile")
            .scope
            .files = crate::ai::FileScope::Project;
        editor.set_viewport_height(24);
        (dir, editor, first, second)
    }

    fn call(steps: serde_json::Value) -> ToolCallInfo {
        ToolCallInfo {
            id: "explain-1".into(),
            name: "explain_with_codebase".into(),
            arguments: json!({ "steps": steps }),
        }
    }

    fn batch_continuation() -> CodeExplanationContinuation {
        CodeExplanationContinuation::Batch {
            runtime_tool: None,
            runtime_turn: None,
            remaining_tool_calls: Vec::new(),
            model_name: "test".into(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn walkthrough_accepts_absolute_paths_under_an_approved_external_root() {
        let (_project, mut editor, _first, _second) = setup_editor();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("external.rs");
        std::fs::write(&source, "fn external() {}\n").unwrap();
        editor
            .ai_state
            .chat
            .as_mut()
            .unwrap()
            .approved_external_roots
            .push(external.path().canonicalize().unwrap());

        let steps = editor
            .parse_code_explanation_steps(&json!({ "steps": [{
                "type": "code",
                "path": source,
                "start_line": 1,
                "comment": "This source is in an attached external root."
            }]}))
            .expect("approved absolute path should be accepted");

        assert!(matches!(
            &steps[0],
            CodeExplanationStep::Code { absolute_path, .. } if absolute_path == &source.canonicalize().unwrap()
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn walkthrough_rejects_unapproved_absolute_paths_with_actionable_guidance() {
        let (_project, editor, _first, _second) = setup_editor();
        let external = tempfile::NamedTempFile::new().unwrap();
        let error = editor
            .parse_code_explanation_steps(&json!({ "steps": [{
                "type": "code",
                "path": external.path(),
                "start_line": 1,
                "comment": "This source has not been approved."
            }]}))
            .unwrap_err();

        let ToolResult::Error(message) = error else {
            panic!("expected an error");
        };
        assert!(message.contains("Use read_file_at_path or list_files"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn walkthrough_question_resumes_root_turn_and_remains_in_conversation() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 2,
            "comment": "The history handler maps alternate navigation keys."
        }]));
        let turn = editor
            .begin_ai_runtime_turn("explain the history handler")
            .unwrap();
        let runtime_tool = editor
            .ai_runtime_record_tool_intent(&turn, &tool_call)
            .unwrap();
        editor.ai_runtime_start_tool(&turn, &runtime_tool).unwrap();
        let (stream_tx, stream_rx) = tokio::sync::mpsc::unbounded_channel();
        let (steer_tx, mut steer_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.runtime_turn = Some(Box::new(turn.clone()));
        chat.streaming_content = Some(String::new());
        chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
            receiver: stream_rx,
            task,
            profile_name: "test".into(),
            model_name: "test".into(),
            turn: Box::new(turn.clone()),
            branch_generation: 0,
            steer_tx: Some(steer_tx),
        });
        if let Err((error, _)) = editor.begin_code_explanation(
            tool_call,
            CodeExplanationContinuation::Dynamic {
                runtime_tool,
                runtime_turn: turn,
                response: response_tx,
            },
        ) {
            panic!("could not start walkthrough: {error:?}");
        }

        let input = crate::editor::input::InputHandler::handle_key_event;
        input(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Char(' '), crate::Modifiers::NONE),
        )
        .unwrap();
        for character in "Why is k used here?".chars() {
            input(
                &mut editor,
                crate::KeyEvent::new(crate::KeyCode::Char(character), crate::Modifiers::NONE),
            )
            .unwrap();
        }
        input(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Enter, crate::Modifiers::NONE),
        )
        .unwrap();

        let ProviderSteerUpdate::Queue { id, content } = steer_rx.recv().await.unwrap() else {
            panic!("walkthrough question should steer the root turn")
        };
        assert!(content.contains("> Walkthrough page 1 · first.rs:2"));
        assert!(content.contains("Why is k used here?"));
        assert!(response_rx.await.unwrap().unwrap().contains("paused"));
        assert!(editor.ai_chat_has_pending_code_explanation());
        assert!(editor.ai_code_explanation_answering());
        assert_eq!(
            editor.ai_code_explanation_view().unwrap().answer_scroll,
            FOLLOW_LATEST_ANSWER,
            "a new answer should follow its latest streamed rows"
        );
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .unwrap()
            .clone();
        let schema_has = |schemas: &[serde_json::Value], name: &str| {
            schemas.iter().any(|schema| {
                schema
                    .get("name")
                    .or_else(|| schema.pointer("/function/name"))
                    .and_then(serde_json::Value::as_str)
                    == Some(name)
            })
        };
        assert!(!schema_has(
            &editor.build_tool_schemas_for_chat(&profile),
            "edit_range"
        ));
        let mutation = ToolCallInfo {
            id: "edit-during-answer".into(),
            name: "edit_range".into(),
            arguments: json!({
                "path": "first.rs",
                "start_line": 2,
                "end_line": 2,
                "content": "// changed"
            }),
        };
        assert!(matches!(
            editor.dispatch_tool_call_with_approval(&mutation, None),
            super::super::ai_chat_tools::ToolDispatchOutcome::Completed(
                ToolResult::Error(ref error)
            ) if error.contains("unavailable while answering a walkthrough question")
        ));

        stream_tx
            .send(StreamChunk::SteerAccepted {
                id,
                content: content.clone(),
            })
            .unwrap();
        stream_tx
            .send(StreamChunk::Content(
                "It is an alternate key only while history has focus.".into(),
            ))
            .unwrap();
        stream_tx.send(StreamChunk::Done).unwrap();
        assert!(editor.poll_pending_ai_chat_job());

        let view = editor.ai_code_explanation_view().unwrap();
        assert!(matches!(
            view.discussion,
            CodeExplanationDiscussionView::Navigating {
                question_count: 1,
                latest_question: Some(ref question),
                latest_answer: Some(ref answer),
                latest_failed: false,
            } if question == "Why is k used here?"
                && answer == "It is an alternate key only while history has focus."
        ));
        assert_eq!(
            editor.ai_chat_activity(),
            crate::editor::AiChatActivity::WaitingCodeExplanation
        );
        assert_eq!(
            editor.status_message(),
            "Walkthrough answer ready — Up/Down reads the reply; Enter continues"
        );
        let messages = editor.ai_chat_messages();
        assert!(messages.iter().any(|message| message.role
            == crate::ai::chat_types::ChatRole::User
            && message.content == content));
        assert!(messages.iter().any(|message| {
            message.role == crate::ai::chat_types::ChatRole::Assistant
                && message.content == "It is an alternate key only while history has focus."
        }));
        assert!(schema_has(
            &editor.build_tool_schemas_for_chat(&profile),
            "edit_range"
        ));

        input(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Char(' '), crate::Modifiers::NONE),
        )
        .unwrap();
        for character in "What focus transition enables it?".chars() {
            input(
                &mut editor,
                crate::KeyEvent::new(crate::KeyCode::Char(character), crate::Modifiers::NONE),
            )
            .unwrap();
        }
        input(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Enter, crate::Modifiers::NONE),
        )
        .unwrap();
        let replacement = {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            let previous = chat.pending_job.take().expect("second question job");
            previous.task.abort();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(async { std::future::pending::<()>().await });
            chat.pending_job = Some(super::super::ai_chat_state::PendingAiChatJob {
                receiver: rx,
                task,
                profile_name: previous.profile_name,
                model_name: previous.model_name,
                turn: previous.turn,
                branch_generation: previous.branch_generation,
                steer_tx: None,
            });
            tx
        };
        replacement
            .send(StreamChunk::Content(
                "Up moves focus from the first composer row into history.".into(),
            ))
            .unwrap();
        replacement.send(StreamChunk::Done).unwrap();
        assert!(editor.poll_pending_ai_chat_job());
        assert!(matches!(
            editor.ai_code_explanation_view().unwrap().discussion,
            CodeExplanationDiscussionView::Navigating {
                question_count: 2,
                latest_question: Some(ref question),
                latest_answer: Some(ref answer),
                latest_failed: false,
            } if question == "What focus transition enables it?"
                && answer == "Up moves focus from the first composer row into history."
        ));

        assert!(editor.finish_code_explanation(false));
        assert!(!editor.ai_chat_has_pending_code_explanation());
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content.contains("Why is k used here?")));
    }

    #[test]
    fn safe_range_reserves_space_for_the_walkthrough_card() {
        let mut editor = Editor::default();
        editor.set_viewport_height(0);
        assert_eq!(editor.ai_code_explanation_safe_range_lines(), 40);
        editor.set_viewport_height(24);
        assert_eq!(editor.ai_code_explanation_safe_range_lines(), 14);
        editor.set_viewport_height(6);
        assert_eq!(editor.ai_code_explanation_safe_range_lines(), 1);
        editor.set_last_layout(
            crate::Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 18,
            },
            0,
            72,
            0,
        );
        assert_eq!(editor.ai_code_explanation_safe_range_lines(), 8);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_answer_scroll_controls_are_bounded_and_reset_between_steps() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));
        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }
        editor.render_cache.code_explanation_answer_max_scroll = 2;

        crate::editor::input::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Down, crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.ai_code_explanation_view().unwrap().answer_scroll, 1);
        for _ in 0..2 {
            crate::editor::input::mouse::handle_mouse_event(
                &mut editor,
                crate::MouseEvent {
                    kind: crate::MouseEventKind::ScrollDown,
                    column: 10,
                    row: 5,
                },
            )
            .unwrap();
        }
        assert_eq!(editor.ai_code_explanation_view().unwrap().answer_scroll, 2);
        crate::editor::input::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Up, crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.ai_code_explanation_view().unwrap().answer_scroll, 1);

        assert!(editor.move_code_explanation(true));
        assert_eq!(editor.ai_code_explanation_view().unwrap().answer_scroll, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_opens_and_selects_each_step_without_retargeting_the_agent() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let original_target = editor.ai_state.chat.as_ref().unwrap().active_buffer_id;
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "end_line": 4,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }
        assert!(editor.ai_chat_has_pending_code_explanation());
        assert!(editor.buffer().file_path().is_none());
        assert_eq!(editor.buffer().line_text(1).as_deref(), Some("// first 2"));
        assert_eq!(editor.ai_code_explanation_view().unwrap().current, 1);
        let selection = editor.ai_state.active_selection.as_ref().unwrap();
        assert_eq!((selection.start_line, selection.end_line), (1, 3));
        assert_eq!(editor.scroll_offset(), 1);

        assert!(editor.move_code_explanation(true));
        assert!(editor.buffer().file_path().is_none());
        assert_eq!(editor.buffer().line_text(6).as_deref(), Some("// second 7"));
        assert_eq!(editor.ai_code_explanation_view().unwrap().current, 2);

        editor.finish_code_explanation(true);
        assert!(!editor.ai_chat_has_pending_code_explanation());
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().active_buffer_id,
            original_target
        );
        assert!(editor.ai_state.active_selection.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_snapshot_tab_shows_source_file_name() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }

        // The snapshot buffer stays pathless (no LSP identity, no save
        // target) but presents the source file's name instead of "[No Name]".
        assert!(editor.buffer().file_path().is_none());
        assert_eq!(editor.buffer().display_name(), Some("first.rs"));
        assert_eq!(editor.get_tab_title(editor.current_tab_index()), "first.rs");

        assert!(editor.move_code_explanation(true));
        assert_eq!(editor.buffer().display_name(), Some("second.rs"));
        assert_eq!(
            editor.get_tab_title(editor.current_tab_index()),
            "second.rs"
        );

        editor.finish_code_explanation(true);
        assert_eq!(editor.get_tab_title(editor.current_tab_index()), "first.rs");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_keeps_invocation_snapshot_after_external_edit_and_delete() {
        let (_dir, mut editor, _first, second) = setup_editor();
        let original_buffer_count = editor.buffer_count();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "The first page gives the harness time to edit another file."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "This page must still use the version captured at invocation."
            }
        ]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("walkthrough should start: {error:?}");
        }
        std::fs::write(&second, "replacement line 1\nreplacement line 2\n")
            .expect("replace source after capture");
        std::fs::remove_file(&second).expect("delete live source after replacement");

        assert!(editor.move_code_explanation(true));
        assert_eq!(editor.buffer().line_text(6).as_deref(), Some("// second 7"));
        assert_eq!(editor.buffer_count(), original_buffer_count + 1);

        assert!(editor.finish_code_explanation(false));
        assert_eq!(editor.buffer_count(), original_buffer_count);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_snapshot_does_not_follow_later_unsaved_buffer_edits() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let source_buffer_id = editor.ai_state.chat.as_ref().unwrap().active_buffer_id;
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 2,
            "comment": "The walkthrough renders an immutable buffer snapshot."
        }]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("walkthrough should start: {error:?}");
        }
        assert!(editor.ai_code_explanation_is_presenting_snapshot());
        editor
            .get_buffer_by_id_mut(source_buffer_id)
            .unwrap()
            .replace_all("changed after invocation\n");

        assert_eq!(editor.buffer().line_text(1).as_deref(), Some("// first 2"));
        assert_eq!(
            editor
                .get_buffer_by_id(source_buffer_id)
                .unwrap()
                .line_text(0)
                .as_deref(),
            Some("changed after invocation")
        );

        assert!(editor.finish_code_explanation(false));
        assert!(!editor.ai_code_explanation_is_presenting_snapshot());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concept_pages_and_code_pages_share_one_linear_sequence() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let original_target = editor.ai_state.chat.as_ref().unwrap().active_buffer_id;
        let tool_call = call(json!([
            {
                "type": "concept",
                "title": "Two layers of history",
                "body": "Input recall and conversation navigation are separate concerns."
            },
            {
                "type": "code",
                "path": "second.rs",
                "start_line": 7,
                "comment": "This line performs the navigation handoff."
            }
        ]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }
        assert!(matches!(
            editor.ai_code_explanation_view().unwrap().page,
            CodeExplanationPageView::Concept { ref title, .. }
                if title == "Two layers of history"
        ));
        assert!(editor.ai_state.active_selection.is_none());

        assert!(editor.move_code_explanation(true));
        assert!(matches!(
            editor.ai_code_explanation_view().unwrap().page,
            CodeExplanationPageView::Code { start_line: 7, .. }
        ));
        assert!(editor.buffer().file_path().is_none());
        assert_eq!(editor.buffer().line_text(6).as_deref(), Some("// second 7"));
        assert!(editor.ai_state.active_selection.is_some());

        assert!(editor.move_code_explanation(false));
        assert!(matches!(
            editor.ai_code_explanation_view().unwrap().page,
            CodeExplanationPageView::Concept { .. }
        ));
        assert!(editor.ai_state.active_selection.is_none());
        assert_eq!(
            editor
                .ai_state
                .chat
                .as_ref()
                .unwrap()
                .pending_code_explanation
                .as_ref()
                .unwrap()
                .original_active_buffer_id,
            original_target,
            "the original agent target must remain available for completion"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concept_page_overflow_requests_semantic_pagination() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        editor.set_last_layout(
            crate::Rect {
                x: 0,
                y: 0,
                width: 32,
                height: 16,
            },
            0,
            32,
            0,
        );
        let tool_call = call(json!([{
            "type": "concept",
            "title": "An overloaded introduction",
            "body": "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty-one twenty-two twenty-three twenty-four twenty-five twenty-six twenty-seven twenty-eight twenty-nine thirty thirty-one thirty-two thirty-three thirty-four thirty-five thirty-six thirty-seven thirty-eight thirty-nine forty"
        }]));

        let error = editor
            .begin_code_explanation(tool_call, batch_continuation())
            .expect_err("dense concept page should fail")
            .0;
        let ToolResult::Error(error) = error else {
            panic!("expected tool error")
        };
        assert!(error.contains("concept body wraps to"), "{error}");
        assert!(error.contains("current maximum is 8"), "{error}");
        assert!(error.contains("focused concept pages"), "{error}");
        assert!(
            error.contains("rather than truncating or compressing"),
            "{error}"
        );
    }

    #[test]
    fn concept_page_question_quotes_the_page_without_a_fake_code_range() {
        let prompt = walkthrough_question_prompt(
            0,
            &CodeExplanationStep::Concept {
                title: "Two layers of history".into(),
                body: "Input recall and conversation navigation are separate.".into(),
            },
            "Which layer owns Up?",
        );

        assert!(prompt.contains("> Walkthrough page 1 · Concept: Two layers of history"));
        assert!(prompt.contains("> Input recall and conversation navigation are separate."));
        assert!(prompt.contains("Which layer owns Up?"));
        assert!(!prompt.contains(".rs:"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_controls_recover_after_visual_mode_corruption() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));
        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }

        // Before pointer gestures were made inert, dragging over walkthrough
        // code could switch the editor to Visual mode and bypass AiChat input.
        editor.set_mode(crate::mode::Mode::Visual);
        crate::editor::input::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Right, crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
        assert_eq!(editor.ai_code_explanation_view().unwrap().current, 2);

        editor.set_mode(crate::mode::Mode::Visual);
        crate::editor::input::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Esc, crate::Modifiers::NONE),
        )
        .unwrap();
        assert!(!editor.ai_chat_has_pending_code_explanation());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ignored_walkthrough_keys_preserve_the_pinned_viewport() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));
        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }
        crate::editor::input::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Right, crate::Modifiers::NONE),
        )
        .unwrap();
        let pinned_offset = editor.scroll_offset();
        assert_eq!(pinned_offset, 6);

        // Repeating an ignored key used to consume the initial viewport pin and
        // then let the shared scrolloff pass pull the selection toward center.
        for code in [crate::KeyCode::Up, crate::KeyCode::Down, crate::KeyCode::Up] {
            crate::editor::input::InputHandler::handle_key_event(
                &mut editor,
                crate::KeyEvent::new(code, crate::Modifiers::NONE),
            )
            .unwrap();
            assert_eq!(editor.scroll_offset(), pinned_offset);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_pointer_gestures_cannot_enter_visual_mode() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 2,
            "comment": "The entry point validates the request."
        }]));
        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }

        // Simulate a drag already in progress when the walkthrough takes over.
        editor.set_mode(crate::mode::Mode::Visual);
        editor.render_cache.mouse_state.is_dragging = true;
        editor.render_cache.mouse_state.drag_origin = Some((0, 0));
        crate::editor::input::mouse::handle_mouse_event(
            &mut editor,
            crate::MouseEvent {
                kind: crate::MouseEventKind::Drag(crate::MouseButton::Left),
                column: 10,
                row: 5,
            },
        )
        .unwrap();

        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
        assert!(!editor.render_cache.mouse_state.is_dragging);
        assert!(editor.render_cache.mouse_state.drag_origin.is_none());
        assert!(editor.ai_chat_has_pending_code_explanation());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_replay_keeps_source_syntax_and_restores_the_lsp_target() {
        let (_dir, mut editor, first, _second) = setup_editor();
        let source_buffer_id = editor.ai_state.chat.as_ref().unwrap().active_buffer_id;
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 2,
            "comment": "The cached replay must preserve this source version."
        }]));
        editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message_with_tools(
                String::new(),
                "test".into(),
                vec![tool_call.clone()],
            );

        if let Err((error, _)) =
            editor.begin_code_explanation(tool_call.clone(), batch_continuation())
        {
            panic!("walkthrough should start: {error:?}");
        }
        assert!(editor.buffer().has_syntax_highlighting());
        assert!(!editor.lsp.state.needs_lsp_init);
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().active_buffer_id,
            source_buffer_id
        );
        assert!(editor.finish_code_explanation(false));
        assert!(editor.lsp.state.needs_lsp_init);

        editor
            .get_buffer_by_id_mut(source_buffer_id)
            .unwrap()
            .replace_all("changed after the walkthrough\n");
        std::fs::remove_file(first).expect("cached replay must survive source deletion");
        assert!(editor.replay_code_explanation(&tool_call.id));
        assert_eq!(editor.buffer().line_text(1).as_deref(), Some("// first 2"));
        assert!(editor.buffer().has_syntax_highlighting());
        assert!(!editor.lsp.state.needs_lsp_init);
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().active_buffer_id,
            source_buffer_id
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_cache_evicts_oldest_tutorial_before_exceeding_ten_megabytes() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let make_steps = |prefix: &str| {
            ["a.rs", "b.rs"]
                .into_iter()
                .map(|name| CodeExplanationStep::Code {
                    path: name.into(),
                    absolute_path: PathBuf::from(format!("{prefix}-{name}")),
                    snapshot: Some(Arc::from("x".repeat(3 * 1024 * 1024))),
                    start_line: 1,
                    end_line: 1,
                    comment: "Cached source.".into(),
                })
                .collect::<Vec<_>>()
        };

        editor.retain_code_explanation_steps("older", &make_steps("older"));
        editor.retain_code_explanation_steps("newer", &make_steps("newer"));

        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert_eq!(chat.code_explanation_cache.len(), 1);
        assert_eq!(chat.code_explanation_cache[0].tool_call_id, "newer");
        assert_eq!(chat.code_explanation_cache_bytes, 6 * 1024 * 1024);
        assert!(chat.code_explanation_cache_bytes <= MAX_WALKTHROUGH_CACHE_BYTES);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_megabyte_files_are_read_on_demand_instead_of_retained() {
        let (dir, mut editor, _first, _second) = setup_editor();
        let large = dir.path().join("large.rs");
        let mut original = b"// original large source\n".to_vec();
        original.resize(MIN_UNCACHEABLE_WALKTHROUGH_FILE_BYTES, b' ');
        std::fs::write(&large, original).unwrap();
        let tool_call = call(json!([{
            "path": "large.rs",
            "start_line": 1,
            "comment": "Large files are intentionally loaded only for the visible page."
        }]));
        editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message_with_tools(
                String::new(),
                "test".into(),
                vec![tool_call.clone()],
            );

        if let Err((error, _)) =
            editor.begin_code_explanation(tool_call.clone(), batch_continuation())
        {
            panic!("large walkthrough should start: {error:?}");
        }
        let pending = editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_code_explanation
            .as_ref()
            .unwrap();
        assert!(matches!(
            &pending.steps[0],
            CodeExplanationStep::Code { snapshot: None, .. }
        ));
        assert_eq!(
            editor
                .ai_state
                .chat
                .as_ref()
                .unwrap()
                .code_explanation_cache_bytes,
            0
        );
        assert_eq!(
            editor.buffer().line_text(0).as_deref(),
            Some("// original large source")
        );
        assert!(editor.finish_code_explanation(false));

        let mut replacement = b"// replacement read on replay\n".to_vec();
        replacement.resize(MIN_UNCACHEABLE_WALKTHROUGH_FILE_BYTES, b' ');
        std::fs::write(&large, replacement).unwrap();
        assert!(editor.replay_code_explanation(&tool_call.id));
        assert_eq!(
            editor.buffer().line_text(0).as_deref(),
            Some("// replacement read on replay")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn completed_walkthrough_replays_locally_without_changing_history() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "end_line": 4,
                "comment": "The entry point validates the request."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "The handoff occurs here."
            }
        ]));
        let conversation = editor.conversation_mut().unwrap();
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![tool_call.clone()],
        );
        conversation.append_tool_result(
            tool_call.id.clone(),
            "User completed the code walkthrough (2 steps).".into(),
        );
        let history_len = editor.ai_chat_messages().len();

        assert!(editor.replay_code_explanation(&tool_call.id));
        assert_eq!(editor.ai_code_explanation_view().unwrap().total, 2);
        assert!(!editor.ai_chat_waiting());
        assert!(editor.advance_or_finish_code_explanation());
        assert!(editor.advance_or_finish_code_explanation());

        assert!(!editor.ai_chat_has_pending_code_explanation());
        assert_eq!(editor.ai_chat_messages().len(), history_len);
        assert!(!editor.ai_chat_waiting());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_rejects_ranges_larger_than_the_live_safe_range() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 1,
            "end_line": 17,
            "comment": "Too broad."
        }]));

        let error = editor
            .begin_code_explanation(tool_call, batch_continuation())
            .expect_err("oversized range should fail")
            .0;
        let ToolResult::Error(error) = error else {
            panic!("expected tool error")
        };
        assert!(error.contains("maximum is 14"), "{error}");
        assert!(error.contains("first.rs:1-17"), "{error}");
        assert!(!editor.ai_chat_has_pending_code_explanation());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_rejects_logical_ranges_that_overflow_after_wrapping() {
        let (dir, mut editor, _first, _second) = setup_editor();
        let wrapped = dir.path().join("wrapped.rs");
        std::fs::write(
            &wrapped,
            (1..=20)
                .map(|line| format!("// line {line} has enough words to wrap several times\n"))
                .collect::<String>(),
        )
        .unwrap();
        editor.options.wrap = true;
        editor.set_last_layout(
            crate::Rect {
                x: 0,
                y: 0,
                width: 24,
                height: 24,
            },
            4,
            12,
            0,
        );
        let tool_call = call(json!([{
            "path": "wrapped.rs",
            "start_line": 1,
            "end_line": 4,
            "comment": "A logically short but visually tall range."
        }]));

        let error = editor
            .begin_code_explanation(tool_call, batch_continuation())
            .expect_err("wrapped range should fail")
            .0;
        let ToolResult::Error(error) = error else {
            panic!("expected tool error")
        };
        assert!(error.contains("visual rows"), "{error}");
        assert!(error.contains("maximum 14"), "{error}");
        assert!(error.contains("wrapped.rs:1-4"), "{error}");
        assert!(error.contains("Suggested safe endpoint"), "{error}");
        assert!(error.contains("Longest wrapped lines"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_validation_uses_the_full_width_that_replaces_chat() {
        let (dir, mut editor, _first, _second) = setup_editor();
        let wide = dir.path().join("wide.rs");
        std::fs::write(
            &wide,
            (1..=4)
                .map(|line| format!("// {line} {}\n", "word ".repeat(10)))
                .collect::<String>(),
        )
        .unwrap();
        editor.options.wrap = true;
        editor.set_last_layout(
            crate::Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 24,
            },
            4,
            10,
            0,
        );
        editor.render_cache.last_chat_area = Some(crate::Rect {
            x: 30,
            y: 0,
            width: 50,
            height: 24,
        });
        let tool_call = call(json!([{
            "path": "wide.rs",
            "start_line": 1,
            "end_line": 4,
            "comment": "Four related declarations form one cohesive block."
        }]));

        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("full-width walkthrough should fit: {error:?}");
        }
        assert!(editor.ai_chat_has_pending_code_explanation());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn source_metrics_prefer_unsaved_buffer_content_over_disk() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        editor
            .buffer_mut()
            .replace_all("an unsaved line that wraps\nsecond line\n");
        let path = PathBuf::from(editor.buffer().file_path().unwrap());
        let snapshot = editor.capture_code_explanation_source(&path).unwrap();

        let metrics = editor.code_explanation_source_metrics(&snapshot, 1, 1, Some(8));

        assert_eq!(metrics.line_count, 3);
        assert!(metrics.visual_rows.unwrap() > 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enter_advances_before_it_completes_the_walkthrough() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        let tool_call = call(json!([
            {
                "path": "first.rs",
                "start_line": 2,
                "comment": "First concept."
            },
            {
                "path": "second.rs",
                "start_line": 7,
                "comment": "Second concept."
            }
        ]));
        if let Err((error, _)) = editor.begin_code_explanation(tool_call, batch_continuation()) {
            panic!("could not start walkthrough: {error:?}");
        }

        assert!(editor.advance_or_finish_code_explanation());
        assert_eq!(editor.ai_code_explanation_view().unwrap().current, 2);
        assert!(editor.ai_chat_has_pending_code_explanation());

        assert!(editor.advance_or_finish_code_explanation());
        assert!(!editor.ai_chat_has_pending_code_explanation());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_rejects_comments_that_cannot_fit_in_the_card() {
        let (_dir, mut editor, _first, _second) = setup_editor();
        editor.set_last_layout(
            crate::Rect {
                x: 0,
                y: 0,
                width: 32,
                height: 24,
            },
            0,
            32,
            0,
        );
        let tool_call = call(json!([{
            "path": "first.rs",
            "start_line": 1,
            "comment": "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty-one twenty-two twenty-three twenty-four twenty-five twenty-six twenty-seven twenty-eight"
        }]));

        let error = editor
            .begin_code_explanation(tool_call, batch_continuation())
            .expect_err("long comment should fail")
            .0;
        let ToolResult::Error(error) = error else {
            panic!("expected tool error")
        };
        assert!(error.contains("comment wraps to"), "{error}");
        assert!(error.contains("within 5 rows"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_schema_contains_structured_steps_and_live_range_guidance() {
        let (_dir, editor, _first, _second) = setup_editor();
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .unwrap();
        let schemas = editor.build_tool_schemas_for_chat(profile);
        let schema = schemas
            .iter()
            .find(|schema| schema["function"]["name"] == "explain_with_codebase")
            .expect("walkthrough schema");

        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("at most 14 visual code rows"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("hides chat"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("understanding depends on seeing concrete code locations in sequence"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("revisit a line or range"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("Do not use it for a short answer"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("prerequisite before its consequence"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("Do not select an entire function"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("two new ideas"));
        let steps = &schema["function"]["parameters"]["properties"]["steps"];
        assert_eq!(steps["type"], "array");
        let concept = &steps["items"]["oneOf"][0];
        let code = &steps["items"]["oneOf"][1];
        assert_eq!(concept["required"], json!(["type", "title", "body"]));
        assert_eq!(
            code["required"],
            json!(["type", "path", "start_line", "comment"])
        );
        assert_eq!(concept["properties"]["type"]["const"], "concept");
        assert_eq!(code["properties"]["type"]["const"], "code");
        assert!(code["properties"]["end_line"].is_object());
        assert!(steps["description"]
            .as_str()
            .unwrap()
            .contains("single-line anchors"));
        assert!(steps["description"]
            .as_str()
            .unwrap()
            .contains("Repeating a range is encouraged"));
        assert!(steps["description"]
            .as_str()
            .unwrap()
            .contains("smallest condition, assignment, call, or block"));
        assert!(steps["description"]
            .as_str()
            .unwrap()
            .contains("split it into two pages"));
        assert!(code["properties"]["end_line"]["description"]
            .as_str()
            .unwrap()
            .contains("smallest cohesive block"));
        assert!(code["properties"]["end_line"]["description"]
            .as_str()
            .unwrap()
            .contains("surrounding function"));
        assert!(code["properties"]["comment"]["description"]
            .as_str()
            .unwrap()
            .contains("exactly one easy-to-understand idea"));
        assert!(code["properties"]["comment"]["description"]
            .as_str()
            .unwrap()
            .contains("front-load later details"));
        assert!(concept["properties"]["body"]["description"]
            .as_str()
            .unwrap()
            .contains("split it into consecutive concept pages"));
        assert!(steps["description"]
            .as_str()
            .unwrap()
            .contains("at most 12 wrapped body rows"));
        assert!(schema["function"]["description"]
            .as_str()
            .unwrap()
            .contains("pedagogical sequence would reduce cognitive load"));
    }
}
