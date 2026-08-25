use super::ai_state::AiSelectionSnapshot;
use super::Editor;
use crate::edit::Edit;
use crate::mode::Mode;
use crate::unicode::GraphemeCol;
use anyhow::Result;

impl Editor {
    /// Returns configured AI profile names sorted for deterministic picker navigation.
    pub fn ai_profile_names_sorted(&self) -> Vec<String> {
        let mut names: Vec<String> = self.ai_state.config.profiles.keys().cloned().collect();
        names.sort();
        names
    }

    /// Select a specific AI profile. Reports and returns false when unknown.
    pub fn ai_set_profile(&mut self, profile_name: &str) -> bool {
        let Some(profile) = self.ai_state.config.resolve_profile(profile_name) else {
            self.set_status_message(format!("Unknown AI profile: {profile_name}"));
            return false;
        };
        self.ai_state.active_profile = profile_name.to_string();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.opts.profile = Some(profile_name.to_string());
        }
        self.set_status_message(format!(
            "AI profile: {} ({}/{})",
            profile_name, profile.provider, profile.model
        ));
        true
    }

    pub fn ai_cycle_profile(&mut self, forward: bool) {
        let names = self.ai_profile_names_sorted();
        if names.is_empty() {
            self.set_status_message("No AI profiles configured".to_string());
            return;
        }
        let current_idx = names
            .iter()
            .position(|name| name == &self.ai_state.active_profile)
            .unwrap_or(0);
        let next_idx = if forward {
            (current_idx + 1) % names.len()
        } else if current_idx == 0 {
            names.len() - 1
        } else {
            current_idx - 1
        };
        let selected = self.ai_set_profile(&names[next_idx]);
        debug_assert!(selected, "profile came from the active configuration");
    }

    /// Attach the current visual selection to the editable AI chat.
    pub fn start_ai_chat_from_visual(&mut self) -> Result<()> {
        self.start_ai_chat_from_visual_with_profile(None)
    }

    /// Compatibility entry point for `vim.ai.edit_selection({ profile = ... })`.
    pub fn start_ai_chat_from_visual_with_profile(
        &mut self,
        profile: Option<String>,
    ) -> Result<()> {
        if let Some(profile_name) = profile.as_deref() {
            if self.ai_state.config.resolve_profile(profile_name).is_none() {
                self.set_status_message(format!("Unknown AI profile: {profile_name}"));
                return Ok(());
            }
        }
        if !self.capture_ai_selection_from_visual()? {
            return Ok(());
        }

        self.open_ai_chat(crate::ai::chat_types::ChatOpts {
            name: "chat".into(),
            profile: profile
                .clone()
                .or_else(|| self.ai_chat_context_profile("chat")),
            allow_edits: true,
            ..Default::default()
        })?;
        if let Some(profile) = profile {
            self.ai_set_profile(&profile);
        }
        let selection = self
            .ai_state
            .active_selection
            .as_ref()
            .expect("selection was captured")
            .clone();
        let buffer = self
            .get_buffer_by_id(selection.buffer_id)
            .expect("selected buffer remains open");
        let source_context = buffer
            .display_name()
            .filter(|name| name.starts_with("Diff · "))
            .map(|_| {
                buffer
                    .rope()
                    .lines()
                    .take(2)
                    .map(|line| line.to_string())
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|context| !context.is_empty());
        let attachment_path = source_context
            .as_deref()
            .and_then(|context| context.lines().find_map(|line| line.strip_prefix("file: ")))
            .map(str::to_string)
            .or_else(|| buffer.file_path().map(ToString::to_string));
        let attachment = super::ai_chat_state::CodeAttachment {
            buffer_id: selection.buffer_id,
            path: attachment_path,
            start_line: selection.start_line,
            start_column: selection.start_col,
            end_line: selection.end_line,
            end_column: selection.end_col.saturating_sub(1),
            linewise: selection.selection_mode == Mode::VisualLine,
            buffer_revision: buffer.version(),
            source_context,
            text: selection.selected_text,
        };
        let label = attachment.label();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = selection.buffer_id;
            chat.pending_code_attachment = Some(attachment);
        }
        self.set_status_message(format!("Attached {label} to AI chat"));
        Ok(())
    }

    fn capture_ai_selection_from_visual(&mut self) -> Result<bool> {
        if self.mode() == Mode::VisualBlock {
            self.set_status_message("AI chat does not support visual block selections".to_string());
            return Ok(false);
        }

        let Some(((start_line, start_col), (end_line, end_col))) = self.visual_selection() else {
            self.set_status_message("No visual selection to attach".to_string());
            return Ok(false);
        };

        let rope = self.buffer().rope();
        let rope_len = rope.len_chars();
        let (start_col, end_col, start_char, end_char) = match self.mode() {
            Mode::VisualLine => {
                let start = rope.line_to_char(start_line).min(rope_len);
                let end = if end_line + 1 < self.buffer().raw_line_count() {
                    rope.line_to_char(end_line + 1)
                } else {
                    rope_len
                };
                let end_col =
                    crate::unicode::grapheme_count(&crate::display::line_content(rope, end_line));
                (0, end_col, start, end.min(rope_len))
            }
            _ => {
                let start_line_text = crate::display::line_content(rope, start_line);
                let end_line_text = crate::display::line_content(rope, end_line);
                let end_col = end_col.saturating_add(1);
                let start = rope.line_to_char(start_line)
                    + crate::unicode::grapheme_to_char_col(
                        &start_line_text,
                        GraphemeCol(start_col),
                    )
                    .0;
                let end = rope.line_to_char(end_line)
                    + crate::unicode::grapheme_to_char_col(&end_line_text, GraphemeCol(end_col)).0;
                (start_col, end_col, start.min(rope_len), end.min(rope_len))
            }
        };

        if end_char <= start_char {
            self.set_status_message("Visual selection is empty".to_string());
            return Ok(false);
        }

        self.ai_state.active_selection = Some(AiSelectionSnapshot {
            buffer_id: self.buffer().id(),
            start_line,
            start_col,
            end_line,
            end_col,
            start_char,
            end_char,
            anchor_line: start_line,
            selected_text: rope.slice(start_char..end_char).to_string(),
            selection_mode: self.mode(),
        });
        Ok(true)
    }

    pub(crate) fn cursor_abs_char(&self) -> usize {
        let cursor = self.buffer().cursor();
        let rope = self.buffer().rope();
        if rope.len_lines() == 0 {
            return 0;
        }
        let line = cursor.line().min(rope.len_lines().saturating_sub(1));
        let line_start = rope.line_to_char(line);
        let line_end = if line + 1 < rope.len_lines() {
            rope.line_to_char(line + 1)
        } else {
            rope.len_chars()
        };
        let content_end = if line_end > line_start && rope.char(line_end - 1) == '\n' {
            line_end - 1
        } else {
            line_end
        };
        line_start + cursor.col().0.min(content_end.saturating_sub(line_start))
    }

    pub(crate) fn set_cursor_from_abs_char(&mut self, abs_char: usize) {
        let rope = self.buffer().rope();
        let clamped = abs_char.min(rope.len_chars());
        let line = rope.char_to_line(clamped);
        let char_col = clamped.saturating_sub(rope.line_to_char(line));
        let line_text = crate::display::line_content(rope, line);
        let col =
            crate::unicode::char_to_grapheme_col(&line_text, crate::unicode::CharCol(char_col));
        self.buffer_mut().cursor_mut().set_position(line, col);
        if !matches!(
            self.mode(),
            Mode::Insert | Mode::Replace | Mode::Command | Mode::Search | Mode::RenameInput
        ) {
            self.buffer_mut().validate_cursor_position();
        }
    }
}

pub(crate) fn remap_abs_char_through_edits(mut abs_char: usize, edits: &[Edit]) -> usize {
    for edit in edits {
        match edit {
            Edit::Insert { offset, text } => {
                if *offset <= abs_char {
                    abs_char = abs_char.saturating_add(text.chars().count());
                }
            }
            Edit::Delete { offset, text } => {
                let delete_end = offset.saturating_add(text.chars().count());
                if abs_char >= delete_end {
                    abs_char = abs_char.saturating_sub(text.chars().count());
                } else if abs_char > *offset {
                    abs_char = *offset;
                }
            }
        }
    }
    abs_char
}
