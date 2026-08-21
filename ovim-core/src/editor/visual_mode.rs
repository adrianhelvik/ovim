use super::Editor;
use crate::mode::Mode;
use crate::unicode::{grapheme_count, GraphemeCol};

impl Editor {
    /// Returns the current visual selection as user-visible text.
    ///
    /// Frontends use this for native clipboard integration while keeping the
    /// mode-specific grapheme/line/block semantics in one core implementation.
    pub fn visual_selection_text(&self) -> Option<String> {
        crate::editor::input::helpers::get_visual_selection_text(self)
    }

    /// Gets the visual selection start position
    pub fn visual_start(&self) -> Option<(usize, usize)> {
        self.visual.visual_start
    }

    /// Sets the visual selection start position
    pub fn set_visual_start(&mut self, line: usize, col: usize) {
        self.visual.visual_start = Some((line, col));
    }

    /// Clears the visual selection
    pub fn clear_visual_start(&mut self) {
        self.visual.visual_start = None;
    }

    /// Saves the current visual selection for gv command
    pub fn save_last_visual_selection(&mut self) {
        if let Some(selection) = self.visual_selection() {
            self.visual.last_visual_selection = Some((selection.0, selection.1, self.mode));
        }
    }

    /// Restores the last visual selection (gv command)
    pub fn restore_last_visual_selection(&mut self) {
        if let Some((start, end, mode)) = self.visual.last_visual_selection {
            // Set the mode first
            self.mode = mode;

            // Clamp start position to buffer bounds. start.1/end.1 are
            // grapheme cols (sourced from `cursor.col()`), so clamp against
            // grapheme count — not `chars().count()` which double-counts
            // multi-codepoint graphemes (emoji, ZWJ, flags). OV-00246.
            let line_count = self.buffer().line_count();
            let clamped_start_line = start.0.min(line_count.saturating_sub(1));
            let start_line_len = self
                .buffer()
                .line_text(clamped_start_line)
                .map(|l| grapheme_count(&l))
                .unwrap_or(0);
            let clamped_start_col = if mode == crate::mode::Mode::VisualLine {
                // For VisualLine, always use column 0
                0
            } else {
                start.1.min(start_line_len.saturating_sub(1))
            };

            // Clamp end position to buffer bounds
            let clamped_end_line = end.0.min(line_count.saturating_sub(1));
            let end_line_len = self
                .buffer()
                .line_text(clamped_end_line)
                .map(|l| grapheme_count(&l))
                .unwrap_or(0);
            let clamped_end_col = if mode == crate::mode::Mode::VisualLine {
                // For VisualLine, always use column 0
                0
            } else {
                end.1.min(end_line_len.saturating_sub(1))
            };

            // Set visual start
            self.visual.visual_start = Some((clamped_start_line, clamped_start_col));
            // Move cursor to end position
            self.buffer_mut()
                .cursor_mut()
                .set_position(clamped_end_line, GraphemeCol(clamped_end_col));
        }
    }

    #[cfg(test)]
    fn set_last_visual_selection_for_test(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        mode: Mode,
    ) {
        self.visual.last_visual_selection = Some((start, end, mode));
    }

    /// Sets visual block insert/append state for replay on insert mode exit
    pub fn set_visual_block_insert_state(
        &mut self,
        state: Option<(usize, usize, usize, bool, bool)>,
    ) {
        self.visual.visual_block_insert_state = state;
    }

    /// Gets visual block insert/append state
    pub fn visual_block_insert_state(&self) -> Option<(usize, usize, usize, bool, bool)> {
        self.visual.visual_block_insert_state
    }

    /// Returns true when `$` was pressed in visual block mode (extend to EOL).
    pub fn visual_block_dollar(&self) -> bool {
        self.visual.visual_block_dollar
    }

    pub fn set_visual_block_dollar(&mut self, val: bool) {
        self.visual.visual_block_dollar = val;
    }

    /// Gets the visual selection range (start and end positions)
    /// Returns ((start_line, start_col), (end_line, end_col))
    /// Note: For VisualBlock, this returns the corners of the rectangle
    pub fn visual_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        self.visual.visual_start.map(|start| {
            let cursor = self.buffer().cursor();
            let mut end = (cursor.line(), cursor.col().0);

            match self.mode {
                Mode::VisualLine => {
                    // Get the length of the end line (excluding newline)
                    if let Some(line_text) = self.buffer().line_text(end.0) {
                        let line_len = line_text.chars().count();
                        end.1 = if line_len > 0 { line_len - 1 } else { 0 };
                    }

                    // Also ensure start is at beginning of its line
                    let mut start = start;
                    start.1 = 0;

                    // Normalize so start is always before end
                    if start.0 <= end.0 {
                        (start, end)
                    } else {
                        // If cursor moved above start line, swap and adjust
                        let mut new_start = end;
                        new_start.1 = 0;
                        let mut new_end = start;
                        if let Some(line_text) = self.buffer().line_text(new_end.0) {
                            let line_len = line_text.chars().count();
                            new_end.1 = if line_len > 0 { line_len - 1 } else { 0 };
                        }
                        (new_start, new_end)
                    }
                }
                Mode::VisualBlock => {
                    // Block mode: return corners of rectangle
                    // Normalize so start_line <= end_line and start_col <= end_col

                    let (min_line, max_line) = if start.0 <= end.0 {
                        (start.0, end.0)
                    } else {
                        (end.0, start.0)
                    };

                    let (min_col, max_col) = if self.visual.visual_block_dollar {
                        // `$` was pressed: extend each line to its own EOL.
                        // Use usize::MAX - 1 as sentinel (avoids +1 overflow in callers).
                        (start.1.min(end.1), usize::MAX - 1)
                    } else if start.1 <= end.1 {
                        (start.1, end.1)
                    } else {
                        (end.1, start.1)
                    };

                    ((min_line, min_col), (max_line, max_col))
                }
                _ => {
                    // Normal visual mode behavior
                    // Normalize so start is always before end
                    if start.0 < end.0 || (start.0 == end.0 && start.1 <= end.1) {
                        (start, end)
                    } else {
                        (end, start)
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OV-00246: `restore_last_visual_selection` clamped grapheme cols
    /// against `chars().count()`. On lines with multi-codepoint graphemes
    /// (emoji, ZWJ, flags) the char count exceeds the grapheme count, so
    /// the clamp let through values that were out of grapheme range —
    /// landing the cursor past the end of the line on `gv`.
    #[test]
    fn restore_clamps_against_grapheme_count_not_chars() {
        // Line: "👨‍👩‍👧‍👦\n" — 1 grapheme, 7 chars.
        // Saved end col = 6 (e.g., from a prior, longer line that got
        // truncated since save).
        // Pre-fix clamp: 6.min(chars - 1) = 6.min(6) = 6 → out of range.
        // Post-fix clamp: 6.min(graphemes - 1) = 6.min(0) = 0 → valid.
        let mut editor = Editor::with_content("👨‍👩‍👧‍👦\n");
        editor.set_last_visual_selection_for_test((0, 0), (0, 6), Mode::Visual);

        editor.restore_last_visual_selection();

        let cursor = editor.buffer().cursor();
        assert_eq!(cursor.line(), 0);
        assert_eq!(
            cursor.col().0,
            0,
            "cursor should clamp to last grapheme col (0), not last char col"
        );
    }

    #[test]
    fn restore_clamps_within_mixed_grapheme_line() {
        // Line: "x👨‍👩‍👧‍👦y\n" — 3 graphemes, 9 chars.
        // Saved end col = 8 (out of grapheme range, in char range).
        // Post-fix clamp: 8.min(2) = 2 → valid grapheme col on the 'y'.
        let mut editor = Editor::with_content("x👨‍👩‍👧‍👦y\n");
        editor.set_last_visual_selection_for_test((0, 0), (0, 8), Mode::Visual);

        editor.restore_last_visual_selection();

        let cursor = editor.buffer().cursor();
        assert_eq!(cursor.line(), 0);
        assert_eq!(
            cursor.col().0,
            2,
            "cursor should clamp to last grapheme col (2), not last char col (8)"
        );
    }

    #[test]
    fn restore_preserves_in_range_col_on_ascii_line() {
        // Sanity check: ASCII-only behavior unchanged. chars().count() ==
        // grapheme_count for ASCII so the fix shouldn't shift the cursor.
        let mut editor = Editor::with_content("hello world\n");
        editor.set_last_visual_selection_for_test((0, 2), (0, 7), Mode::Visual);

        editor.restore_last_visual_selection();

        let cursor = editor.buffer().cursor();
        assert_eq!(cursor.line(), 0);
        assert_eq!(cursor.col().0, 7);
        assert_eq!(editor.visual_start(), Some((0, 2)));
    }
}
