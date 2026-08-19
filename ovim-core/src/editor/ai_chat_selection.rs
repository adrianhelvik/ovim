use crate::ai::chat_types::ChatRole;

use super::Editor;

impl Editor {
    pub fn begin_ai_chat_text_selection(&mut self, row: usize, column: usize) {
        use super::render_cache::{ChatTextPoint, ChatTextSelection};

        let point = ChatTextPoint { row, column };
        self.render_cache.ai_chat_text_autoscroll = None;
        self.render_cache.ai_chat_text_selection = Some(ChatTextSelection {
            anchor: point,
            head: point,
            moved: false,
        });
        self.render_cache.ai_chat_text_selecting = true;
    }

    pub fn update_ai_chat_text_selection(&mut self, row: usize, column: usize) {
        let Some(selection) = self.render_cache.ai_chat_text_selection.as_mut() else {
            return;
        };
        let point = super::render_cache::ChatTextPoint { row, column };
        selection.moved |= point != selection.anchor;
        selection.head = point;
    }

    pub fn set_ai_chat_text_selection_autoscroll(
        &mut self,
        direction: super::render_cache::ChatTextAutoscrollDirection,
        column: usize,
    ) {
        use super::render_cache::{ChatTextAutoscroll, ChatTextAutoscrollDirection};

        let boundary_row = match direction {
            ChatTextAutoscrollDirection::Older => self.render_cache.ai_chat_last_visible_start_row,
            ChatTextAutoscrollDirection::Newer => self
                .render_cache
                .ai_chat_last_visible_end_row
                .saturating_sub(1),
        };
        self.update_ai_chat_text_selection(boundary_row, column);

        let current_tick = chat_text_autoscroll_tick();
        let last_tick = self
            .render_cache
            .ai_chat_text_autoscroll
            .filter(|autoscroll| autoscroll.direction == direction)
            .map_or(current_tick.saturating_sub(1), |autoscroll| {
                autoscroll.last_tick
            });
        self.render_cache.ai_chat_text_autoscroll = Some(ChatTextAutoscroll {
            direction,
            column,
            last_tick,
        });
    }

    pub fn clear_ai_chat_text_selection_autoscroll(&mut self) {
        self.render_cache.ai_chat_text_autoscroll = None;
    }

    /// Advance an edge-drag selection by one rendered conversation row.
    /// Called by the UI clock so selection keeps moving while the pointer is
    /// held outside the history viewport without additional mouse events.
    pub fn tick_ai_chat_text_selection_autoscroll(&mut self) -> bool {
        use super::render_cache::ChatTextAutoscrollDirection;

        if self.mode() != crate::mode::Mode::AiChat || !self.render_cache.ai_chat_text_selecting {
            self.render_cache.ai_chat_text_autoscroll = None;
            return false;
        }
        let tick = chat_text_autoscroll_tick();
        let Some(autoscroll) = self.render_cache.ai_chat_text_autoscroll.as_mut() else {
            return false;
        };
        if autoscroll.last_tick == tick {
            return false;
        }
        autoscroll.last_tick = tick;
        let direction = autoscroll.direction;
        let column = autoscroll.column;
        let start = self.render_cache.ai_chat_last_visible_start_row;
        let end = self.render_cache.ai_chat_last_visible_end_row;
        let total = self.render_cache.ai_chat_rendered_text_rows.len();

        match direction {
            ChatTextAutoscrollDirection::Older if start > 0 => {
                self.ai_chat_scroll_viewport_up(1);
                self.update_ai_chat_text_selection(start - 1, column);
                true
            }
            ChatTextAutoscrollDirection::Newer if end < total => {
                self.ai_chat_scroll_viewport_down(1);
                self.update_ai_chat_text_selection(end, column);
                true
            }
            _ => false,
        }
    }

    /// Finish a mouse selection and copy it immediately, matching terminal
    /// select-to-copy behavior. A click without a drag clears the selection.
    pub fn finish_ai_chat_text_selection(&mut self) -> bool {
        self.render_cache.ai_chat_text_selecting = false;
        self.render_cache.ai_chat_text_autoscroll = None;
        if !self
            .render_cache
            .ai_chat_text_selection
            .is_some_and(|selection| selection.moved)
        {
            self.render_cache.ai_chat_text_selection = None;
            return false;
        }
        let copied = self.copy_ai_chat_text_selection();
        if copied {
            self.set_status_message("Copied selected chat text".to_string());
        }
        copied
    }

    pub fn ai_chat_has_text_selection(&self) -> bool {
        self.render_cache
            .ai_chat_text_selection
            .is_some_and(|selection| selection.moved)
    }

    /// Selected display-column interval for an absolute rendered history row.
    pub fn ai_chat_text_selection_range(&self, row: usize) -> Option<(usize, usize)> {
        let selection = self.render_cache.ai_chat_text_selection?;
        if !selection.moved {
            return None;
        }
        let (start, end) = ordered_chat_selection(selection.anchor, selection.head);
        if row < start.row || row > end.row {
            return None;
        }
        if start.row == end.row {
            return Some((start.column, end.column.saturating_add(1)));
        }
        if row == start.row {
            Some((start.column, usize::MAX))
        } else if row == end.row {
            Some((0, end.column.saturating_add(1)))
        } else {
            Some((0, usize::MAX))
        }
    }

    /// Copy and consume the active mouse selection. Returns false when no
    /// non-empty text is selected, allowing callers to fall back to copying
    /// the conversation without leaving a stale highlight behind.
    pub fn copy_ai_chat_text_selection(&mut self) -> bool {
        self.render_cache.ai_chat_text_selecting = false;
        self.render_cache.ai_chat_text_autoscroll = None;
        let Some(selection) = self.render_cache.ai_chat_text_selection.take() else {
            return false;
        };
        if !selection.moved {
            return false;
        }
        let (start, end) = ordered_chat_selection(selection.anchor, selection.head);
        let rows = &self.render_cache.ai_chat_rendered_text_rows;
        if rows.is_empty() || start.row >= rows.len() {
            return false;
        }

        let mut selected_rows = Vec::new();
        let last_row = end.row.min(rows.len() - 1);
        for (row_index, row) in rows.iter().enumerate().take(last_row + 1).skip(start.row) {
            let (from, to) = if start.row == end.row {
                (start.column, end.column.saturating_add(1))
            } else if row_index == start.row {
                (start.column, usize::MAX)
            } else if row_index == end.row {
                (0, end.column.saturating_add(1))
            } else {
                (0, usize::MAX)
            };
            let mut text = slice_display_columns(row, from, to);
            if from == 0 {
                text = strip_chat_row_prefix(text);
            }
            selected_rows.push(text.trim_end_matches(' ').to_string());
        }
        let output = selected_rows.join("\n");
        if output.is_empty() {
            return false;
        }
        self.registers.set_clipboard(output);
        true
    }

    /// Format the current conversation as plain text and copy to clipboard.
    pub fn copy_ai_chat_conversation(&mut self) {
        let messages = self.ai_chat_messages().to_vec();
        if messages.is_empty() {
            return;
        }

        let mut output = String::new();
        for msg in &messages {
            let role = match msg.role {
                ChatRole::User => "You",
                ChatRole::Assistant => msg.model.as_deref().unwrap_or("Assistant"),
                ChatRole::Thinking => "Thinking",
                ChatRole::Error => "Error",
                ChatRole::Tool => "Tool",
            };
            output.push_str(&format!("### {}\n\n{}\n\n", role, msg.content));
        }

        self.registers.set_clipboard(output);
    }
}

fn chat_text_autoscroll_tick() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 60
}

fn ordered_chat_selection(
    first: super::render_cache::ChatTextPoint,
    second: super::render_cache::ChatTextPoint,
) -> (
    super::render_cache::ChatTextPoint,
    super::render_cache::ChatTextPoint,
) {
    if (first.row, first.column) <= (second.row, second.column) {
        (first, second)
    } else {
        (second, first)
    }
}

fn slice_display_columns(text: &str, start: usize, end: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut column = 0usize;
    text.graphemes(true)
        .filter(|grapheme| {
            let width = crate::display::grapheme_display_width(grapheme).max(1);
            let grapheme_start = column;
            let grapheme_end = column.saturating_add(width);
            column = grapheme_end;
            grapheme_end > start && grapheme_start < end
        })
        .collect()
}

fn strip_chat_row_prefix(mut text: String) -> String {
    if text.starts_with('\u{258d}') || text.starts_with('\u{258c}') {
        text.remove(0);
        if text.starts_with(' ') {
            text.remove(0);
        }
    }
    text
}
