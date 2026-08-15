use super::Buffer;
use crate::change::TextObjectType;
use crate::edit::Edit;
use crate::indentation::{leading_char_count, leading_width, IndentOptions};
use crate::number_ops::{find_number_at_or_after, format_number, parse_number};
use crate::unicode::{
    char_to_grapheme_col, grapheme_at_index, grapheme_count, grapheme_to_char_col, CharCol,
    GraphemeCol,
};

impl Buffer {
    /// Inserts text at a specific position (line, col)
    pub fn insert_text_at(&mut self, line: usize, col: CharCol, text: &str) {
        // Track buffer edit metrics
        crate::metrics::BUFFER_EDITS_TOTAL.inc();

        // Use raw_line_count() to allow inserting at the phantom empty line
        // (which is valid for appending at end of buffer)
        if line >= self.raw_line_count() {
            return;
        }

        let line_start = self.rope.line_to_char(line);
        let insert_pos = line_start + col.0;

        // Clamp to valid position
        let insert_pos = insert_pos.min(self.rope.len_chars());
        // Convert char col to byte col for highlighting (cache stores byte offsets)
        let line_start_char = self.rope.line_to_char(line);
        let col_clamped = col.0.min(self.rope.len_chars() - line_start_char);
        let byte_col = self.rope.char_to_byte(line_start_char + col_clamped)
            - self.rope.char_to_byte(line_start_char);

        // Create tree-sitter edit BEFORE modifying rope (needs old state)
        let ts_edit = self.create_ts_insert_edit(line, byte_col, text);

        // Shift highlights BEFORE modifying rope
        self.shift_highlights_for_insertion(line, byte_col, text);

        self.rope.insert(insert_pos, text);
        self.modified = true;

        let recorded_edit = Edit::Insert {
            offset: insert_pos,
            text: text.to_string(),
        };

        // Record the edit if we're in a recording session
        if let Some(ref mut session) = self.recording {
            session.edits.push(recorded_edit.clone());
        }

        // Update buffer size metrics
        crate::metrics::BUFFER_SIZE_BYTES.set(self.rope.len_bytes() as i64);
        crate::metrics::BUFFER_LINES.set(self.rope.len_lines() as i64);

        // Apply incremental tree-sitter edit (much faster than full re-parse)
        if let Some(edit) = ts_edit {
            self.apply_incremental_syntax_edit(edit);
        }

        // Increment versions for cache invalidation
        self.version += 1;
        self.highlight_version = self.highlight_version.wrapping_add(1);
        self.pending_rehighlight = true;
        // Invalidate code block cache so stale byte offsets don't override
        // the fresh tree-sitter highlights that viewport rehighlight provides
        self.code_block_cache = None;
        self.injection_cache = None;

        // Publish to edit_log so decoration projection sees this edit
        // immediately, regardless of whether a `record()` / stateful session
        // is in progress.
        self.edit_log.push(self.version as u64, vec![recorded_edit]);
    }

    /// Inserts text at `(line, col)` and, if the buffer actually mutated,
    /// positions the cursor at the end of the inserted text (char-space
    /// position converted to grapheme via `set_cursor_char_col`).
    ///
    /// Returns `true` when the buffer version changed (i.e. the insertion
    /// was not blocked or clamped to a no-op). This mirrors the historical
    /// `Change::InsertText::apply` behavior that callers in insert/replace
    /// mode relied on for cursor landing.
    pub fn insert_text_at_positioning_cursor(
        &mut self,
        line: usize,
        col: CharCol,
        text: &str,
    ) -> bool {
        let version_before = self.version();
        self.insert_text_at(line, col, text);
        if self.version() == version_before {
            return false;
        }
        // Position cursor at end of inserted text. The counting iterates
        // chars (not graphemes), matching the legacy `calculate_end_position`
        // on `Change`; `set_cursor_char_col` converts to grapheme space.
        let mut end_line = line;
        let mut end_col = col.0;
        for ch in text.chars() {
            if ch == '\n' {
                end_line += 1;
                end_col = 0;
            } else {
                end_col += 1;
            }
        }
        self.set_cursor_char_col(end_line, CharCol(end_col));
        true
    }

    /// Deletes the char-space range `[start..end)` and, if the buffer
    /// actually mutated, positions the cursor at the start of the deleted
    /// range.
    ///
    /// Returns `(mutated, deleted_text)` — `mutated` is `true` when the
    /// buffer version changed. Mirrors the historical `Change::DeleteText::apply`
    /// behavior.
    pub fn delete_range_positioning_cursor(
        &mut self,
        start_line: usize,
        start_col: CharCol,
        end_line: usize,
        end_col: CharCol,
    ) -> (bool, String) {
        let version_before = self.version();
        let deleted = self.delete_range(start_line, start_col, end_line, end_col);
        if self.version() == version_before {
            return (false, deleted);
        }
        // Position cursor at deletion start (char-space → grapheme via
        // set_cursor_char_col).
        self.set_cursor_char_col(start_line, start_col);
        (true, deleted)
    }

    /// Deletes text in a range and returns the deleted text.
    ///
    /// Columns are char indices (`CharCol`) — what rope operations use. The
    /// end column is exclusive.
    pub fn delete_range(
        &mut self,
        start_line: usize,
        start_col: CharCol,
        end_line: usize,
        end_col: CharCol,
    ) -> String {
        // Track buffer edit metrics
        crate::metrics::BUFFER_EDITS_TOTAL.inc();

        if start_line >= self.line_count() {
            return String::new();
        }

        // Validate start column is within line bounds to prevent addition overflow
        let start_line_len = self.line_len(start_line);
        let actual_start_col = start_col.0.min(start_line_len);

        let start_line_char = self.rope.line_to_char(start_line);
        let start_pos = start_line_char + actual_start_col;

        // Calculate actual end position and column
        let (end_pos, actual_end_line, actual_end_col) = if end_line >= self.line_count() {
            (
                self.rope.len_chars(),
                self.line_count().saturating_sub(1),
                self.line_len(self.line_count().saturating_sub(1)),
            )
        } else {
            // Validate end column is within line bounds to prevent addition overflow
            let end_line_len = self.line_len(end_line);
            let actual_end_col = end_col.0.min(end_line_len);

            let end_line_char = self.rope.line_to_char(end_line);
            (end_line_char + actual_end_col, end_line, actual_end_col)
        };

        // Final safety clamp to buffer length (should be redundant after column validation)
        let start_pos = start_pos.min(self.rope.len_chars());
        let end_pos = end_pos.min(self.rope.len_chars());

        if start_pos >= end_pos {
            return String::new();
        }
        let deleted = self.rope.slice(start_pos..end_pos).to_string();

        // Convert char cols to byte cols for highlighting (cache stores byte offsets)
        let start_byte_col = self.rope.char_to_byte(start_line_char + actual_start_col)
            - self.rope.char_to_byte(start_line_char);
        let end_line_char_offset = self.rope.line_to_char(actual_end_line);
        let end_byte_col = self
            .rope
            .char_to_byte(end_line_char_offset + actual_end_col)
            - self.rope.char_to_byte(end_line_char_offset);

        // Create tree-sitter edit BEFORE modifying rope (needs old state)
        let ts_edit = self.create_ts_delete_edit(
            start_line,
            start_byte_col,
            actual_end_line,
            end_byte_col,
            &deleted,
        );

        // Shift highlights BEFORE modifying rope
        self.shift_highlights_for_deletion(
            start_line,
            start_byte_col,
            actual_end_line,
            end_byte_col,
        );

        self.rope.remove(start_pos..end_pos);
        self.modified = true;

        let recorded_edit = Edit::Delete {
            offset: start_pos,
            text: deleted.clone(),
        };

        // Record the edit if we're in a recording session
        if let Some(ref mut session) = self.recording {
            session.edits.push(recorded_edit.clone());
        }

        // Update buffer size metrics
        crate::metrics::BUFFER_SIZE_BYTES.set(self.rope.len_bytes() as i64);
        crate::metrics::BUFFER_LINES.set(self.rope.len_lines() as i64);

        // Apply incremental tree-sitter edit (much faster than full re-parse)
        if let Some(edit) = ts_edit {
            self.apply_incremental_syntax_edit(edit);
        }

        // Increment versions for cache invalidation
        self.version += 1;
        self.highlight_version = self.highlight_version.wrapping_add(1);
        self.pending_rehighlight = true;
        self.code_block_cache = None;
        self.injection_cache = None;

        // Publish to edit_log so decoration projection sees this edit
        // immediately, regardless of whether a recording session is active.
        self.edit_log.push(self.version as u64, vec![recorded_edit]);

        deleted
    }

    /// Deletes text by absolute char positions in the rope.
    ///
    /// Unlike `delete_range` which clamps columns to `line_len()` (excluding
    /// newlines), this method uses the char indices directly. This is needed
    /// for undo of insertions at positions past the content of a line (e.g.,
    /// after the newline character).
    pub fn delete_char_range(&mut self, start_char: usize, end_char: usize) {
        crate::metrics::BUFFER_EDITS_TOTAL.inc();

        let start_pos = start_char.min(self.rope.len_chars());
        let end_pos = end_char.min(self.rope.len_chars());

        if start_pos >= end_pos {
            return;
        }
        let start_line = self.rope.char_to_line(start_pos);
        let start_char_col = start_pos - self.rope.line_to_char(start_line);
        let end_line = self.rope.char_to_line(end_pos);
        let end_char_col = end_pos - self.rope.line_to_char(end_line);

        let deleted = self.rope.slice(start_pos..end_pos).to_string();

        // Convert char cols to byte cols for highlighting (cache stores byte offsets)
        let start_line_char = self.rope.line_to_char(start_line);
        let start_byte_col = self.rope.char_to_byte(start_line_char + start_char_col)
            - self.rope.char_to_byte(start_line_char);
        let end_line_char_offset = self.rope.line_to_char(end_line);
        let end_byte_col = self.rope.char_to_byte(end_line_char_offset + end_char_col)
            - self.rope.char_to_byte(end_line_char_offset);

        let ts_edit = self.create_ts_delete_edit(
            start_line,
            start_byte_col,
            end_line,
            end_byte_col,
            &deleted,
        );
        self.shift_highlights_for_deletion(start_line, start_byte_col, end_line, end_byte_col);

        self.rope.remove(start_pos..end_pos);
        self.modified = true;

        let recorded_edit = Edit::Delete {
            offset: start_pos,
            text: deleted,
        };

        // Record the edit if we're in a recording session
        if let Some(ref mut session) = self.recording {
            session.edits.push(recorded_edit.clone());
        }

        crate::metrics::BUFFER_SIZE_BYTES.set(self.rope.len_bytes() as i64);
        crate::metrics::BUFFER_LINES.set(self.rope.len_lines() as i64);

        if let Some(edit) = ts_edit {
            self.apply_incremental_syntax_edit(edit);
        }

        self.version += 1;
        self.highlight_version = self.highlight_version.wrapping_add(1);
        self.pending_rehighlight = true;
        self.code_block_cache = None;
        self.injection_cache = None;

        // Publish to edit_log so decoration projection sees this edit
        // immediately, regardless of whether a recording session is active.
        self.edit_log.push(self.version as u64, vec![recorded_edit]);
    }

    /// Replaces the entire buffer content.
    ///
    /// Clears the edit log via `reset_derived_state`: prior entries reference
    /// offsets into the old rope and must not be replayed.
    pub fn replace_all(&mut self, content: &str) {
        self.rope = ropey::Rope::from_str(content);
        self.modified = true;
        self.cursor = super::Cursor::new(0, GraphemeCol(0));
        self.reset_derived_state(content);
    }

    /// Joins the current line with the next line (J command)
    /// Adds a space between the lines unless the current line already ends with whitespace
    pub fn join_lines(&mut self, count: usize) -> anyhow::Result<()> {
        self.join_lines_impl(count, true)
    }

    /// Joins lines without adding a space (gJ command)
    pub fn join_lines_no_space(&mut self, count: usize) -> anyhow::Result<()> {
        self.join_lines_impl(count, false)
    }

    /// Internal implementation for joining lines
    fn join_lines_impl(&mut self, count: usize, add_space: bool) -> anyhow::Result<()> {
        let start_line = self.cursor.line();

        // Vim's [count]J joins `count` LINES, which is `count - 1` joins —
        // with a minimum of one join so plain J still works (3J on
        // `a\nb\nc\nd` gives `a b c`, leaving `d` untouched) — OV-00290.
        let lines_to_join = count.saturating_sub(1).max(1);

        for _ in 0..lines_to_join {
            if start_line >= self.line_count().saturating_sub(1) {
                // Already at the last line, nothing to join
                break;
            }

            // Get the current line and next line
            let current_line_text = match self.line_text(start_line) {
                Some(text) => text.to_string(),
                None => break,
            };

            let next_line_text = match self.line_text(start_line + 1) {
                Some(text) => text.to_string(),
                None => break,
            };

            // Determine if we need to add a space
            let separator = if add_space {
                // Add space unless current line ends with whitespace
                if current_line_text.ends_with(|c: char| c.is_whitespace()) {
                    ""
                } else {
                    " "
                }
            } else {
                ""
            };

            // Trim leading whitespace from next line
            let next_trimmed = next_line_text.trim_start();

            // Build the joined line
            let joined = if next_trimmed.is_empty() {
                // Next line is all whitespace, just use current line
                current_line_text.clone()
            } else {
                format!("{}{}{}", current_line_text, separator, next_trimmed)
            };

            // Delete both lines (from start_line to start_line+2)
            self.delete_range(start_line, CharCol::ZERO, start_line + 2, CharCol::ZERO);

            // Insert the joined line with newline
            self.insert_text_at(start_line, CharCol::ZERO, &format!("{}\n", joined));

            // Position cursor at the junction point (end of original first line).
            // cursor.col() is a grapheme index, so use grapheme_count (not chars().count()).
            let junction_col = GraphemeCol(grapheme_count(&current_line_text));
            self.cursor.set_position(start_line, junction_col);
        }

        Ok(())
    }

    /// Replace a line's leading indentation with the canonical representation
    /// of `target_width` visual columns. Returns the new prefix's char count.
    pub fn set_indent_width_at(
        &mut self,
        line_idx: usize,
        target_width: usize,
        options: IndentOptions,
    ) -> usize {
        let options = options.normalized();
        let Some(line_text) = self.line_text(line_idx).map(|line| line.into_owned()) else {
            return 0;
        };
        let old_chars = leading_char_count(&line_text);
        let replacement = options.encode_indent(target_width);
        if line_text.chars().take(old_chars).eq(replacement.chars()) {
            return replacement.chars().count();
        }

        if old_chars > 0 {
            self.delete_range(line_idx, CharCol::ZERO, line_idx, CharCol(old_chars));
        }
        if !replacement.is_empty() {
            self.insert_text_at(line_idx, CharCol::ZERO, &replacement);
        }
        replacement.chars().count()
    }

    /// Shift lines [start, end) left by one indentation level, measured in
    /// visual columns and canonically re-encoded according to `options`.
    pub fn dedent_lines_at(&mut self, start: usize, end: usize, options: IndentOptions) {
        let options = options.normalized();
        let actual_end = end.min(self.line_count());
        for line_idx in start..actual_end {
            let Some(line_text) = self.line_text(line_idx) else {
                continue;
            };
            let current = leading_width(&line_text, options.tab_width);
            self.set_indent_width_at(
                line_idx,
                current.saturating_sub(options.shift_width),
                options,
            );
        }
    }

    /// Clamp cursor column to valid range for current line (normal mode: last char).
    pub fn clamp_cursor_col(&mut self) {
        let line = self.cursor().line();
        let col = self.cursor().col().0;
        if let Some(line_text) = self.line_text(line) {
            let line_len = grapheme_count(&line_text);
            if col > 0 && col >= line_len {
                self.cursor_mut()
                    .set_col(GraphemeCol(if line_len > 0 { line_len - 1 } else { 0 }));
            }
        }
    }

    /// Shift lines [start, end) right by one indentation level in visual columns.
    /// Skips empty/whitespace-only lines.
    pub fn indent_lines_at(&mut self, start: usize, end: usize, options: IndentOptions) {
        let options = options.normalized();
        let actual_end = end.min(self.line_count());
        for line_idx in start..actual_end {
            // Skip empty/whitespace-only lines
            if let Some(line) = self.line_text(line_idx) {
                if line.trim().is_empty() {
                    continue;
                }
                let current = leading_width(&line, options.tab_width);
                self.set_indent_width_at(line_idx, current + options.shift_width, options);
            }
        }
    }

    /// Toggle case of character at cursor, advance cursor.
    /// Returns true if cursor advanced (more chars available on line).
    ///
    /// cursor.col() is a grapheme index; rope operations use char indices.
    /// We convert at the boundary to handle multi-codepoint graphemes correctly.
    pub fn toggle_char_at_cursor(&mut self) -> bool {
        let line_idx = self.cursor().line();
        let grapheme_col = self.cursor().col(); // GraphemeCol
        let Some(line_text) = self.line_text(line_idx) else {
            return false;
        };
        let line_grapheme_len = grapheme_count(&line_text);
        if grapheme_col.0 >= line_grapheme_len {
            return false;
        }

        // Get the grapheme cluster at cursor (not a single char)
        let grapheme = grapheme_at_index(&line_text, grapheme_col.0).unwrap();

        // Toggle case of all chars in the grapheme (handles combining marks, etc.)
        let toggled: String = grapheme
            .chars()
            .map(|ch| {
                if ch.is_lowercase() {
                    ch.to_uppercase().to_string()
                } else if ch.is_uppercase() {
                    ch.to_lowercase().to_string()
                } else {
                    ch.to_string()
                }
            })
            .collect();

        // Convert grapheme col → char col for rope operations
        let char_col = grapheme_to_char_col(&line_text, grapheme_col);
        let grapheme_char_len = grapheme.chars().count();
        self.delete_range(
            line_idx,
            char_col,
            line_idx,
            char_col.saturating_add(grapheme_char_len),
        );
        self.insert_text_at(line_idx, char_col, &toggled);

        // Re-read line: toggling may change grapheme count (e.g. ß → SS)
        let new_line_grapheme_len = self
            .line_text(line_idx)
            .map(|l| grapheme_count(&l))
            .unwrap_or(0);
        let toggled_grapheme_count = grapheme_count(&toggled);
        let new_grapheme_col = grapheme_col.0 + toggled_grapheme_count;
        if new_grapheme_col < new_line_grapheme_len {
            self.cursor_mut().set_col(GraphemeCol(new_grapheme_col));
            true
        } else {
            false
        }
    }

    /// Gets the word under the cursor
    /// Returns the word and its (start_col, end_col) as char indices on the current line.
    ///
    /// cursor.col() is a grapheme index; we convert to char index for the chars vec lookup.
    pub fn word_under_cursor(&self) -> Option<(String, usize, usize)> {
        let line_idx = self.cursor.line();
        let grapheme_col = self.cursor.col(); // grapheme index

        if line_idx >= self.line_count() {
            return None;
        }

        // Use rope slice to avoid allocation until we need the final word
        let line_slice = self.line_slice(line_idx)?;

        // Build a chars vector from the slice (excluding newline)
        let chars: Vec<char> = line_slice.chars().take_while(|&c| c != '\n').collect();

        // Convert grapheme col → char col for indexing into chars vec
        let line_text: String = chars.iter().collect();
        let char_col = grapheme_to_char_col(&line_text, grapheme_col).0;

        if chars.is_empty() || char_col >= chars.len() {
            return None;
        }

        // Check if cursor is on a word character
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

        if !is_word_char(chars[char_col]) {
            return None;
        }

        // Find start of word (char indices)
        let mut start = char_col;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }

        // Find end of word (char indices)
        let mut end = char_col;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }

        // Only allocate String for the final word
        let word: String = chars[start..end].iter().collect();
        Some((word, start, end))
    }

    /// Finds the number at/after cursor, applies delta, replaces it in-place.
    /// Positions cursor on the last digit of the new number.
    ///
    /// cursor.col() is a grapheme index; find_number_at_or_after works in char indices.
    /// We convert at the boundaries.
    pub fn modify_number_at_cursor(&mut self, delta: i64) {
        let line_idx = self.cursor().line();
        let grapheme_col = self.cursor().col(); // grapheme index

        let Some(line_text) = self.line_text(line_idx) else {
            return;
        };

        // Convert grapheme col → char col for find_number_at_or_after (char-based)
        let char_col = grapheme_to_char_col(&line_text, grapheme_col);

        let Some((start_col, end_col, number_str)) = find_number_at_or_after(&line_text, char_col)
        else {
            return;
        };
        let (mut value, base, prefix_len) = parse_number(&number_str);

        let has_plus_sign = number_str.starts_with('+');
        value += delta;
        let mut new_number_str = format_number(value, base, prefix_len);

        // Preserve explicit '+' sign for positive numbers
        if has_plus_sign && value >= 0 && !new_number_str.starts_with('+') {
            new_number_str = format!("+{}", new_number_str);
        }

        // start_col/end_col are char indices — correct for delete_range/insert_text_at
        self.delete_range(line_idx, start_col, line_idx, end_col);
        self.insert_text_at(line_idx, start_col, &new_number_str);

        // Convert the char-based end position → grapheme for cursor
        let new_char_end_col =
            start_col.saturating_add(new_number_str.chars().count().saturating_sub(1));
        let new_line = self.line_text(line_idx).unwrap_or_default();
        let new_line_text = new_line;
        let new_grapheme_col = char_to_grapheme_col(&new_line_text, new_char_end_col);
        self.cursor_mut().set_position(line_idx, new_grapheme_col);
    }

    /// Finds and deletes a text object at the current cursor position.
    pub fn delete_text_object(&mut self, object_type: &TextObjectType) {
        if let Some(range) = object_type.resolve(self) {
            self.delete_range(
                range.start_line,
                range.start_col,
                range.end_line,
                range.end_col,
            );
            // TextObjectRange cols are char indices; convert to grapheme for cursor.
            self.set_cursor_char_col(range.start_line, range.start_col);
        }
    }

    /// Deletes text found by a character find/till motion on the current line.
    /// Returns the deleted text (for register storage).
    pub fn delete_char_motion(
        &mut self,
        target: char,
        forward: bool,
        till: bool,
        count: usize,
    ) -> String {
        let line_idx = self.cursor().line();
        let col = self.cursor_char_col();

        let Some(line_text) = self.line_text(line_idx) else {
            return String::new();
        };
        let chars: Vec<char> = line_text.chars().collect();

        let found = if forward {
            let mut seen = 0usize;
            let mut found_idx = None;
            for (i, &c) in chars.iter().enumerate().skip(col.0 + 1) {
                if c == target {
                    seen += 1;
                    if seen == count {
                        found_idx = Some(i);
                        break;
                    }
                }
            }
            found_idx
        } else if col == 0 {
            None
        } else {
            let mut seen = 0usize;
            let mut found_idx = None;
            for i in (0..col.0).rev() {
                if chars.get(i).copied() == Some(target) {
                    seen += 1;
                    if seen == count {
                        found_idx = Some(i);
                        break;
                    }
                }
            }
            found_idx
        };

        let Some(found_idx) = found else {
            return String::new();
        };
        let found_col = CharCol(found_idx);

        // Backward F/T are EXCLUSIVE in vim: the cursor character itself is
        // not deleted (dFx on `axbc` with cursor on `c` leaves `ac`) —
        // OV-00288, mirrored in the interactive handler in char_motion.rs.
        let (start_col, end_col) = if forward {
            let end_excl = if till { found_col } else { found_col + 1 };
            (col, end_excl)
        } else if till {
            // Backward till-motion (T): delete from just after target up to cursor.
            (found_col + 1, col)
        } else {
            // Backward find-motion (F): delete from target up to cursor.
            (found_col, col)
        };

        let deleted = self.delete_range(line_idx, start_col, line_idx, end_col);
        self.set_cursor_char_col(line_idx, start_col);
        deleted
    }

    /// Deletes count characters forward from cursor (x command).
    /// Returns the deleted text. Clamps to end of line.
    pub fn delete_chars_forward(&mut self, count: usize) -> String {
        let line_idx = self.cursor().line();
        let grapheme_col = self.cursor().col();
        let Some(line) = self.line_text(line_idx) else {
            return String::new();
        };
        // Operate in grapheme space so multi-codepoint clusters (combining marks,
        // ZWJ emoji, flags) are deleted whole rather than split scalar-by-scalar.
        let grapheme_len = grapheme_count(&line);
        if grapheme_col.0 >= grapheme_len {
            return String::new();
        }
        let end_grapheme = GraphemeCol((grapheme_col.0 + count).min(grapheme_len));
        let start_char = grapheme_to_char_col(&line, grapheme_col);
        let end_char = grapheme_to_char_col(&line, end_grapheme);
        let deleted = self.delete_range(line_idx, start_char, line_idx, end_char);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes count characters backward from cursor (X command).
    /// Returns the deleted text. Clamps to start of line.
    pub fn delete_chars_backward(&mut self, count: usize) -> String {
        let line_idx = self.cursor().line();
        let grapheme_col = self.cursor().col();
        if grapheme_col.0 == 0 {
            return String::new();
        }
        let Some(line) = self.line_text(line_idx) else {
            return String::new();
        };
        // Walk back `count` graphemes (not scalars) so composed clusters delete whole.
        let grapheme_start = GraphemeCol(grapheme_col.0.saturating_sub(count));
        let start_char = grapheme_to_char_col(&line, grapheme_start);
        let end_char = grapheme_to_char_col(&line, grapheme_col);
        let deleted = self.delete_range(line_idx, start_char, line_idx, end_char);
        // Restore cursor using grapheme col offset (cursor stays in grapheme space)
        self.cursor_mut().set_position(line_idx, grapheme_start);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes count lines from cursor line (dd command).
    /// Returns the deleted text.
    pub fn delete_lines(&mut self, count: usize) -> String {
        let start_line = self.cursor().line();
        let end_line = (start_line + count).min(self.line_count());
        let deleted = self.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
        // Clamp cursor to buffer bounds
        let new_line = start_line.min(self.line_count().saturating_sub(1));
        self.cursor_mut().set_position(new_line, GraphemeCol(0));
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to end of line (D / d$ command).
    /// Returns the deleted text.
    pub fn delete_to_end_of_line(&mut self) -> String {
        let line_idx = self.cursor().line();
        let col = self.cursor_char_col();
        let Some(line) = self.line_text(line_idx) else {
            return String::new();
        };
        let line_len = line.chars().count();
        if col >= line_len {
            return String::new();
        }
        let deleted = self.delete_range(line_idx, col, line_idx, CharCol(line_len));
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to next word boundary (dw command).
    /// Returns the deleted text.
    pub fn delete_word_forward(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_grapheme = self.cursor().col();
        let start_col = self.cursor_char_col();

        // Vim's eol rule applies only to the LAST word moved over: when the
        // final `w` crosses a line boundary, the operated range ends at the
        // eol of the line it crossed FROM — but earlier words (and their
        // newlines) are deleted in full. Truncating whenever the whole
        // motion crossed a line made `d2w` discard everything past the
        // first newline (OV-00289).
        let (prev_line, prev_col) = if count > 1 {
            Motions::word_forward(self, count - 1);
            (self.cursor().line(), self.cursor_char_col())
        } else {
            (start_line, start_col)
        };
        Motions::word_forward(self, 1);

        let end_line = self.cursor().line();
        let mut del_end_line = end_line;
        let mut end_col = self.cursor_char_col();

        if end_line > prev_line {
            // Final word motion crossed a newline: stop at eol of the line
            // holding the last word actually moved over.
            if let Some(line) = self.line_text(prev_line) {
                del_end_line = prev_line;
                end_col = CharCol(line.chars().count());
            }
        } else if end_line == prev_line && end_col == prev_col && end_line + 1 >= self.line_count()
        {
            // Motion didn't move — last word on last line. Delete to end of line.
            if let Some(line) = self.line_text(end_line) {
                end_col = CharCol(line.chars().count());
            }
        }

        let deleted = self.delete_range(start_line, start_col, del_end_line, end_col);
        self.cursor_mut().set_position(start_line, start_grapheme);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes current line and count lines below (dj command).
    /// Returns the deleted text.
    pub fn delete_line_down(&mut self, count: usize) -> String {
        let start_line = self.cursor().line();
        let end_line = (start_line + count + 1).min(self.line_count());
        let deleted = self.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
        let new_line = start_line.min(self.line_count().saturating_sub(1));
        self.cursor_mut().set_position(new_line, GraphemeCol(0));
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes current line and count lines above (dk command).
    /// Returns the deleted text.
    pub fn delete_line_up(&mut self, count: usize) -> String {
        let end_line = self.cursor().line() + 1;
        let start_line = self.cursor().line().saturating_sub(count);
        let deleted = self.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
        let new_line = start_line.min(self.line_count().saturating_sub(1));
        self.cursor_mut().set_position(new_line, GraphemeCol(0));
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to paragraph forward (d} command).
    /// Returns the deleted text.
    pub fn delete_paragraph_forward(
        &mut self,
        count: usize,
    ) -> (String, crate::motion_range::Wise) {
        use crate::editor::Motions;
        use crate::motion_range::MotionRange;

        let start_line = self.cursor().line();
        let start_grapheme = self.cursor().col();
        let start_col = self.cursor_char_col();

        Motions::paragraph_forward(self, count);
        let end = (self.cursor().line(), self.cursor_char_col());

        // } is an exclusive motion: normalize with vim's exclusive
        // adjustments so a d} from mid-line keeps the blank separator line
        // and yields a charwise register (OV-00293).
        let range = MotionRange::from_exclusive(self, (start_line, start_col), end);
        let deleted = self.delete_motion_range(range);
        self.cursor_mut().set_position(start_line, start_grapheme);
        // validate_cursor_position clamps both line (may be past EOF after delete)
        // and column, which is a superset of clamp_cursor_col
        self.validate_cursor_position();
        (deleted, range.wise)
    }

    /// Deletes from paragraph backward to cursor (d{ command).
    /// Returns the deleted text.
    pub fn delete_paragraph_backward(
        &mut self,
        count: usize,
    ) -> (String, crate::motion_range::Wise) {
        use crate::editor::Motions;
        use crate::motion_range::MotionRange;

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        Motions::paragraph_backward(self, count);
        let start = (self.cursor().line(), self.cursor_char_col());

        // { is exclusive: the original cursor position is excluded from the
        // operated range, with vim's col-0 adjustments applied (OV-00293).
        let range = MotionRange::from_exclusive(self, start, (end_line, end_col));
        let deleted = self.delete_motion_range(range);
        self.cursor_mut()
            .set_position(range.start.0, GraphemeCol(0));
        // validate_cursor_position clamps both line (may be past EOF after delete)
        // and column, which is a superset of clamp_cursor_col
        self.validate_cursor_position();
        (deleted, range.wise)
    }

    /// Deletes from cursor line to target_line (inclusive, line-wise). (dG command)
    /// Returns the deleted text.
    pub fn delete_to_last_line(&mut self, target_line: usize) -> String {
        let cursor_line = self.cursor().line();
        let start_line = cursor_line.min(target_line);
        let end_line = (cursor_line.max(target_line) + 1).min(self.line_count());
        let deleted = self.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
        let new_line = start_line.min(self.line_count().saturating_sub(1));
        self.cursor_mut().set_position(new_line, GraphemeCol(0));
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to matching bracket (d% command).
    /// Returns the deleted text, or empty string if no bracket at cursor.
    pub fn delete_to_matching_bracket(&mut self) -> String {
        self.delete_to_matching_bracket_inner(true)
    }

    /// `c%` delete phase — like [`delete_to_matching_bracket`] but leaves the
    /// insert point un-clamped (the bracket span may end at EOL; the change
    /// then appends there). See [`delete_to_word_end`].
    pub fn change_to_matching_bracket(&mut self) -> String {
        self.delete_to_matching_bracket_inner(false)
    }

    fn delete_to_matching_bracket_inner(&mut self, clamp: bool) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        let rope = &self.rope;
        let text = rope.to_string();
        let chars: Vec<char> = text.chars().collect();

        // Compute absolute position from line/col
        let mut abs_start = 0;
        for i in 0..start_line {
            if i < rope.len_lines() {
                abs_start += rope.line(i).len_chars();
            }
        }
        abs_start += start_col.0;

        if abs_start >= chars.len() {
            return String::new();
        }

        let current_char = chars[abs_start];

        let (is_opening, matching_char) = match current_char {
            '(' => (true, ')'),
            ')' => (false, '('),
            '[' => (true, ']'),
            ']' => (false, '['),
            '{' => (true, '}'),
            '}' => (false, '{'),
            '<' => (true, '>'),
            '>' => (false, '<'),
            _ => return String::new(),
        };

        let match_abs_pos = if is_opening {
            Motions::find_matching_bracket_forward(&chars, abs_start, current_char, matching_char)
        } else {
            Motions::find_matching_bracket_backward(&chars, abs_start, matching_char, current_char)
        };

        let Some(abs_end) = match_abs_pos else {
            return String::new();
        };

        let (delete_start, delete_end) = if abs_start < abs_end {
            (abs_start, abs_end + 1)
        } else {
            (abs_end, abs_start + 1)
        };

        let (del_start_line, del_start_col) =
            Motions::abs_pos_to_line_col(&self.rope, delete_start);
        let (del_end_line, del_end_col) = Motions::abs_pos_to_line_col(&self.rope, delete_end);

        let deleted = self.delete_range(del_start_line, del_start_col, del_end_line, del_end_col);
        self.set_cursor_char_col(del_start_line, del_start_col);
        if clamp {
            self.clamp_cursor_col();
        }
        deleted
    }

    /// Replaces count characters at cursor with ch (r command).
    /// Returns the replaced (old) text, or an empty string if the replacement
    /// cannot cover the requested count before end-of-line.
    pub fn replace_chars_at_cursor(&mut self, ch: char, count: usize) -> String {
        let line_idx = self.cursor().line();
        let grapheme_col = self.cursor().col();

        let Some(line_text) = self.line_text(line_idx) else {
            return String::new();
        };
        // Replace whole graphemes, not scalars, so composed clusters aren't corrupted.
        let grapheme_len = grapheme_count(&line_text);

        let available = grapheme_len.saturating_sub(grapheme_col.0);
        if count == 0 || count > available {
            return String::new();
        }

        let start_char = grapheme_to_char_col(&line_text, grapheme_col);
        let end_char = grapheme_to_char_col(&line_text, GraphemeCol(grapheme_col.0 + count));

        let deleted = self.delete_range(line_idx, start_char, line_idx, end_char);
        let replacement = ch.to_string().repeat(count);
        self.insert_text_at(line_idx, start_char, &replacement);

        // Vim leaves the cursor on the final replaced character.
        self.cursor_mut()
            .set_position(line_idx, GraphemeCol(grapheme_col.0 + count - 1));

        deleted
    }

    /// Deletes from cursor backward by word motion (db command).
    /// Returns the deleted text.
    pub fn delete_word_backward(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        Motions::word_backward(self, count);

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        // Backward motion: new position is before start
        let deleted = self.delete_range(end_line, end_col, start_line, start_col);
        self.set_cursor_char_col(end_line, end_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to end of word (de command).
    /// Returns the deleted text. Inclusive (includes the last char of word).
    pub fn delete_word_end(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        Motions::word_end_forward(self, count);

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        // Inclusive: delete through the character the motion lands on
        let delete_end_col = end_col + 1;
        let deleted = self.delete_range(start_line, start_col, end_line, delete_end_col);
        self.set_cursor_char_col(start_line, start_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor backward by WORD motion (dB command).
    /// Returns the deleted text.
    pub fn delete_word_backward_big(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        Motions::word_backward_big(self, count);

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        let deleted = self.delete_range(end_line, end_col, start_line, start_col);
        self.set_cursor_char_col(end_line, end_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to end of WORD (dE command).
    /// Returns the deleted text. Inclusive.
    pub fn delete_word_end_big(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        Motions::word_end_forward_big(self, count);

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        let delete_end_col = end_col + 1;
        let deleted = self.delete_range(start_line, start_col, end_line, delete_end_col);
        self.set_cursor_char_col(start_line, start_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Whether the character under the cursor is whitespace (or there is no
    /// character there — past end of line / empty line). Used to pick the
    /// `cw`/`cW` change semantics.
    fn cursor_on_blank(&self) -> bool {
        let col = self.cursor_char_col().0;
        self.line_text(self.cursor().line())
            .and_then(|line| line.chars().nth(col))
            .is_none_or(|c| c.is_whitespace())
    }

    /// Inclusive delete to the end of the current/next word, for the change
    /// operators (`cw`/`cW`/`ce`/`cE`). Unlike the `delete_word_end*` methods
    /// used by `de`/`dE`, this does **not** clamp the cursor afterwards: a
    /// change re-enters insert mode, where `col == line_len` (append at EOL) is
    /// a valid position — clamping to the normal-mode bound would pull the
    /// insert point back one char when the delete reached end of line.
    ///
    /// `prefer_current` selects the `cw`/`cW` motion (change the current word
    /// even when the cursor already sits at its end) versus the `ce`/`cE`
    /// motion (advance to the next word's end in that case).
    fn delete_to_word_end(&mut self, count: usize, big_word: bool, prefer_current: bool) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        match (big_word, prefer_current) {
            (false, false) => Motions::word_end_forward(self, count),
            (false, true) => Motions::word_end_forward_prefer_current(self, count),
            (true, false) => Motions::word_end_forward_big(self, count),
            (true, true) => Motions::word_end_forward_big_prefer_current(self, count),
        }

        let end_line = self.cursor().line();
        let end_col = self.cursor_char_col();

        // Inclusive: delete through the character the motion lands on.
        let deleted = self.delete_range(start_line, start_col, end_line, end_col + 1);
        self.set_cursor_char_col(start_line, start_col);
        deleted
    }

    /// `cw` delete phase. Vim special-cases `cw`/`cW` (`:help cw`): on a
    /// **non-blank** they behave like `ce`/`cE` (change to the word end,
    /// leaving trailing whitespace), but on a **blank** they behave like
    /// `dw`/`dW` (change only the whitespace run up to the next word, leaving
    /// the following word intact). Returns the deleted text.
    pub fn change_word_forward(&mut self, count: usize) -> String {
        if self.cursor_on_blank() {
            self.change_via_word_motion(count, false)
        } else {
            self.delete_to_word_end(count, false, true)
        }
    }

    /// `cW` delete phase. WORD-wise counterpart of [`change_word_forward`].
    pub fn change_word_forward_big(&mut self, count: usize) -> String {
        if self.cursor_on_blank() {
            self.change_via_word_motion(count, true)
        } else {
            self.delete_to_word_end(count, true, true)
        }
    }

    /// `ce` delete phase — like [`delete_word_end`] but leaves the insert point
    /// un-clamped for the change (see [`delete_to_word_end`]).
    pub fn change_word_end(&mut self, count: usize) -> String {
        self.delete_to_word_end(count, false, false)
    }

    /// `cE` delete phase — WORD-wise counterpart of [`change_word_end`].
    pub fn change_word_end_big(&mut self, count: usize) -> String {
        self.delete_to_word_end(count, true, false)
    }

    /// `cw`/`cW` blank case: reuse the `dw`/`dW` delete, then restore the
    /// un-clamped insert point. `delete_word_forward*` clamps the cursor to the
    /// normal-mode bound (correct for `dw`), but the change re-enters insert
    /// mode where `col == line_len` is valid — without this the insert point is
    /// pulled back one char when the blank run runs to end of line.
    fn change_via_word_motion(&mut self, count: usize, big_word: bool) -> String {
        let line = self.cursor().line();
        let col = self.cursor_char_col();
        let deleted = if big_word {
            self.delete_word_forward_big(count)
        } else {
            self.delete_word_forward(count)
        };
        self.set_cursor_char_col(line, col);
        deleted
    }

    /// Deletes one character to the left of cursor (dh command).
    /// Returns the deleted text. Stops at start of line.
    pub fn delete_char_left(&mut self, count: usize) -> String {
        let line_idx = self.cursor().line();
        let col = self.cursor_char_col();
        if col == CharCol::ZERO {
            return String::new();
        }
        let start_col = col.saturating_sub(count);
        let deleted = self.delete_range(line_idx, start_col, line_idx, col);
        self.set_cursor_char_col(line_idx, start_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to start of line (d0 command).
    /// Returns the deleted text.
    pub fn delete_to_start_of_line(&mut self) -> String {
        let line_idx = self.cursor().line();
        let col = self.cursor_char_col();
        if col == CharCol::ZERO {
            return String::new();
        }
        let deleted = self.delete_range(line_idx, CharCol::ZERO, line_idx, col);
        self.cursor_mut().set_position(line_idx, GraphemeCol(0));
        deleted
    }

    /// Deletes from cursor to first non-blank character (d^ command).
    /// Returns the deleted text.
    pub fn delete_to_first_non_blank(&mut self) -> String {
        let line_idx = self.cursor().line();
        let col = self.cursor_char_col();
        let fnb = self.first_non_blank_col(line_idx);
        if fnb == col {
            return String::new();
        }
        let (start, end) = if fnb < col { (fnb, col) } else { (col, fnb) };
        let deleted = self.delete_range(line_idx, start, line_idx, end);
        self.set_cursor_char_col(line_idx, start);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor to next WORD boundary (dW command).
    /// Returns the deleted text.
    pub fn delete_word_forward_big(&mut self, count: usize) -> String {
        use crate::editor::Motions;

        let start_line = self.cursor().line();
        let start_col = self.cursor_char_col();

        // Same last-word-only eol rule as delete_word_forward (OV-00289).
        let (prev_line, prev_col) = if count > 1 {
            Motions::word_forward_big(self, count - 1);
            (self.cursor().line(), self.cursor_char_col())
        } else {
            (start_line, start_col)
        };
        Motions::word_forward_big(self, 1);

        let end_line = self.cursor().line();
        let mut del_end_line = end_line;
        let mut end_col = self.cursor_char_col();

        if end_line > prev_line {
            if let Some(line) = self.line_text(prev_line) {
                del_end_line = prev_line;
                end_col = CharCol(line.chars().count());
            }
        } else if end_line == prev_line && end_col == prev_col && end_line + 1 >= self.line_count()
        {
            // Motion didn't move — last WORD on last line. Delete to end of line.
            if let Some(line) = self.line_text(end_line) {
                end_col = CharCol(line.chars().count());
            }
        }

        let deleted = self.delete_range(start_line, start_col, del_end_line, end_col);
        self.set_cursor_char_col(start_line, start_col);
        self.clamp_cursor_col();
        deleted
    }

    /// Deletes from cursor line to target_line (inclusive, line-wise). (dgg command)
    /// Positions cursor at first non-blank of remaining line.
    /// Returns the deleted text.
    pub fn delete_to_first_line(&mut self, target_line: usize) -> String {
        let cursor_line = self.cursor().line();
        let start_line = cursor_line.min(target_line);
        let end_line = (cursor_line.max(target_line) + 1).min(self.line_count());
        let deleted = self.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
        let new_line = start_line.min(self.line_count().saturating_sub(1));
        self.cursor_mut().set_position(new_line, GraphemeCol(0));
        self.clamp_cursor_col();
        // Move to first non-blank character
        // first_non_blank_col returns char index; convert to grapheme for cursor.
        let fnb = self.first_non_blank_col(new_line);
        self.set_cursor_char_col(new_line, fnb);
        deleted
    }
}
