//! Word motions: w, W, b, B, e, E, ge, gE

use unicode_segmentation::UnicodeSegmentation;

use super::{char_class, CharClass, Motions};
use crate::buffer::Buffer;
use crate::unicode::{grapheme_count, GraphemeCol};

impl Motions {
    /// Moves cursor forward to the start of the next word
    /// w - moves to start of next word (word = alphanumeric + underscore)
    pub fn word_forward(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_forward_once(buffer, false);
        }
    }

    /// Moves cursor forward to the start of the next WORD
    /// W - moves to start of next WORD (WORD = any non-whitespace)
    pub fn word_forward_big(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_forward_once(buffer, true);
        }
    }

    fn word_forward_once(buffer: &mut Buffer, big_word: bool) {
        let rope = buffer.rope();
        let cursor = buffer.cursor();
        let line_idx = cursor.line();
        let grapheme_col = cursor.col();

        if line_idx >= rope.len_lines() {
            return;
        }

        let line = crate::display::line_content(rope, line_idx);
        let chars = word_units(&line);
        let col = grapheme_col.0;

        if col >= chars.len() {
            // At end of line (or empty line), advance to next word start.
            if let Some((next_line, next_col)) =
                Self::find_next_word_start(rope, line_idx + 1, big_word)
            {
                buffer.cursor_mut().set_position(next_line, next_col);
            }
            // else: at end of buffer, don't move
            return;
        }

        let current_char = chars[col];
        let mut new_col = col;

        if big_word {
            // Skip current WORD (non-whitespace)
            if !Self::is_whitespace(current_char) {
                while new_col < chars.len() && !Self::is_whitespace(chars[new_col]) {
                    new_col += 1;
                }
            }
            // Skip whitespace
            while new_col < chars.len() && Self::is_whitespace(chars[new_col]) {
                new_col += 1;
            }
        } else {
            // Skip current word/CJK/punctuation
            let class = char_class(current_char);
            match class {
                CharClass::Cjk => {
                    // Each CJK char is its own word — skip exactly one
                    new_col += 1;
                }
                CharClass::Word => {
                    while new_col < chars.len() && char_class(chars[new_col]) == CharClass::Word {
                        new_col += 1;
                    }
                }
                CharClass::Punctuation => {
                    while new_col < chars.len()
                        && char_class(chars[new_col]) == CharClass::Punctuation
                    {
                        new_col += 1;
                    }
                }
                CharClass::Whitespace => {}
            }
            // Skip whitespace
            while new_col < chars.len() && Self::is_whitespace(chars[new_col]) {
                new_col += 1;
            }
        }

        if new_col >= chars.len() {
            // Ran past end of line — advance to next word start across lines.
            if let Some((next_line, next_col)) =
                Self::find_next_word_start(rope, line_idx + 1, big_word)
            {
                buffer.cursor_mut().set_position(next_line, next_col);
            }
            // else: end of buffer, don't move
        } else {
            let clamped = new_col.min(chars.len().saturating_sub(1));
            buffer.cursor_mut().set_col(GraphemeCol(clamped));
        }
    }

    /// Scan forward from `start_line` looking for the next word start.
    ///
    /// Vim rules for cross-line `w`/`W`:
    /// - Empty line (zero visible chars) → word boundary, return `(line, 0)`
    /// - Whitespace-only line → skip it entirely
    /// - Line with non-whitespace → return `(line, first_non_ws_col)`
    pub(super) fn find_next_word_start(
        rope: &ropey::Rope,
        start_line: usize,
        _big_word: bool,
    ) -> Option<(usize, GraphemeCol)> {
        // Use Vim-compatible line count: exclude the phantom empty line that
        // ropey appends after a trailing '\n'. Motions should not land there.
        let total_lines = {
            let raw = rope.len_lines();
            if raw > 1 && rope.len_chars() > 0 && rope.char(rope.len_chars() - 1) == '\n' {
                raw - 1
            } else {
                raw
            }
        };
        for line_idx in start_line..total_lines {
            let content = crate::display::line_content(rope, line_idx);

            if content.is_empty() {
                // Truly empty line — word boundary, stop here.
                return Some((line_idx, GraphemeCol::ZERO));
            }

            // Check if line has any non-whitespace (char index → grapheme)
            if let Some(char_pos) = content.chars().position(|c| !c.is_whitespace()) {
                return Some((
                    line_idx,
                    crate::unicode::char_to_grapheme_col(
                        &content,
                        crate::unicode::CharCol(char_pos),
                    ),
                ));
            }

            // Whitespace-only line — skip.
        }
        None
    }

    /// Moves cursor backward to the start of the previous word
    /// b - moves to start of previous word
    pub fn word_backward(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_backward_once(buffer, false);
        }
    }

    /// Moves cursor backward to the start of the previous WORD
    /// B - moves to start of previous WORD
    pub fn word_backward_big(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_backward_once(buffer, true);
        }
    }

    fn word_backward_once(buffer: &mut Buffer, big_word: bool) {
        let rope = buffer.rope();
        let cursor = buffer.cursor();
        let mut line_idx = cursor.line();
        let mut grapheme_col = cursor.col();

        if line_idx >= rope.len_lines() {
            return;
        }

        if grapheme_col == GraphemeCol::ZERO {
            // At start of line, move to end of previous line and continue
            if line_idx > 0 {
                line_idx -= 1;
                grapheme_col = GraphemeCol(grapheme_count(&crate::display::line_content(
                    rope, line_idx,
                )));
                // If previous line is empty, just land at col 0
                if grapheme_col == GraphemeCol::ZERO {
                    buffer
                        .cursor_mut()
                        .set_position(line_idx, GraphemeCol::ZERO);
                    return;
                }
                // grapheme_col is now one past the last grapheme; fall through to word-backward logic
            } else {
                return;
            }
        }

        loop {
            let line = crate::display::line_content(rope, line_idx);
            let chars = word_units(&line);
            // Iterate whole graphemes, classified by their leading character.
            let mut new_col = grapheme_col.0;

            // Skip backward over whitespace first. When new_col == chars.len(),
            // we're past the end and begin with chars[new_col - 1].
            while new_col > 0 && Self::is_whitespace(chars[new_col - 1]) {
                new_col -= 1;
            }

            if new_col == 0 {
                // Leading whitespace is not a word boundary. Continue onto the
                // preceding line, skipping whitespace-only lines. A genuinely
                // empty line is a Vim word boundary, so stop on it.
                if line_idx == 0 {
                    buffer
                        .cursor_mut()
                        .set_position(line_idx, GraphemeCol::ZERO);
                    return;
                }

                line_idx -= 1;
                let previous_line = crate::display::line_content(rope, line_idx);
                if previous_line.is_empty() {
                    buffer
                        .cursor_mut()
                        .set_position(line_idx, GraphemeCol::ZERO);
                    return;
                }
                grapheme_col = GraphemeCol(grapheme_count(&previous_line));
                continue;
            }

            if big_word {
                // Move back to start of WORD
                while new_col > 0 && !Self::is_whitespace(chars[new_col - 1]) {
                    new_col -= 1;
                }
            } else {
                let target_char = chars[new_col - 1];
                let class = char_class(target_char);
                match class {
                    CharClass::Cjk => {
                        // Each CJK char is its own word — back exactly one
                        new_col -= 1;
                    }
                    CharClass::Word => {
                        while new_col > 0 && char_class(chars[new_col - 1]) == CharClass::Word {
                            new_col -= 1;
                        }
                    }
                    CharClass::Punctuation => {
                        while new_col > 0
                            && char_class(chars[new_col - 1]) == CharClass::Punctuation
                        {
                            new_col -= 1;
                        }
                    }
                    CharClass::Whitespace => {}
                }
            }

            buffer
                .cursor_mut()
                .set_position(line_idx, GraphemeCol(new_col));
            return;
        }
    }

    /// Moves cursor forward to the end of the current/next word
    /// e - moves to end of word
    pub fn word_end_forward(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_end_forward_once(buffer, false, false);
        }
    }

    /// Variant of `word_end_forward` that prefers the current word on the
    /// first step, even if the cursor is already at that word's end.
    ///
    /// This matches Vim's `cw`/`ce` behavior on single-character words.
    pub fn word_end_forward_prefer_current(buffer: &mut Buffer, count: usize) {
        for i in 0..count {
            Self::word_end_forward_once(buffer, false, i == 0);
        }
    }

    /// Moves cursor forward to the end of the current/next WORD
    /// E - moves to end of WORD
    pub fn word_end_forward_big(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_end_forward_once(buffer, true, false);
        }
    }

    pub fn word_end_forward_big_prefer_current(buffer: &mut Buffer, count: usize) {
        for i in 0..count {
            Self::word_end_forward_once(buffer, true, i == 0);
        }
    }

    fn word_end_forward_once(buffer: &mut Buffer, big_word: bool, prefer_current: bool) {
        let (line_idx, grapheme_col, total_lines, line) = {
            let cursor = buffer.cursor();
            let line_idx = cursor.line();
            (
                line_idx,
                cursor.col(),
                buffer.line_count(),
                crate::display::line_content(buffer.rope(), line_idx),
            )
        };

        if line_idx >= total_lines {
            return;
        }

        let chars = word_units(&line);
        let col = grapheme_col.0;

        if chars.is_empty() {
            // Skip consecutive blank lines to find next non-empty line
            let mut next_line = line_idx + 1;
            while next_line < total_lines {
                let next_trimmed = crate::display::line_content(buffer.rope(), next_line);
                if !next_trimmed.is_empty() {
                    // Start at first non-ws, then move to end of that word
                    let Some(char_col) =
                        next_trimmed.chars().position(|c: char| !c.is_whitespace())
                    else {
                        next_line += 1;
                        continue;
                    };
                    let start_grapheme = crate::unicode::char_to_grapheme_col(
                        &next_trimmed,
                        crate::unicode::CharCol(char_col),
                    );
                    buffer.cursor_mut().set_position(next_line, start_grapheme);
                    Self::word_end_forward_once(buffer, big_word, prefer_current);
                    return;
                }
                next_line += 1;
            }
            buffer
                .cursor_mut()
                .set_position(total_lines.saturating_sub(1), GraphemeCol::ZERO);
            return;
        }

        if col >= chars.len() {
            if line_idx + 1 < total_lines {
                buffer
                    .cursor_mut()
                    .set_position(line_idx + 1, GraphemeCol::ZERO);
                Self::word_end_forward_once(buffer, big_word, prefer_current);
            }
            return;
        }

        let mut idx = col;

        let is_ws = chars.get(idx).is_some_and(|c| c.is_whitespace());
        if is_ws {
            while idx < chars.len() && chars[idx].is_whitespace() {
                idx += 1;
            }
            if idx >= chars.len() {
                if line_idx + 1 < total_lines {
                    buffer
                        .cursor_mut()
                        .set_position(line_idx + 1, GraphemeCol::ZERO);
                    Self::word_end_forward_once(buffer, big_word, prefer_current);
                }
                return;
            }
        } else {
            let end_of_current = if big_word {
                let mut end = idx;
                while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
                    end += 1;
                }
                end
            } else {
                let class = char_class(chars[idx]);
                match class {
                    CharClass::Cjk => idx,
                    CharClass::Word | CharClass::Punctuation => {
                        let mut end = idx;
                        while end + 1 < chars.len() && char_class(chars[end + 1]) == class {
                            end += 1;
                        }
                        end
                    }
                    CharClass::Whitespace => idx,
                }
            };

            if prefer_current || idx < end_of_current {
                buffer.cursor_mut().set_col(GraphemeCol(end_of_current));
                return;
            }

            // Subsequent repeats: already at end of current word — advance into next word.
            idx = idx.saturating_add(1);
            while idx < chars.len() && chars[idx].is_whitespace() {
                idx += 1;
            }
            if idx >= chars.len() {
                if line_idx + 1 < total_lines {
                    buffer
                        .cursor_mut()
                        .set_position(line_idx + 1, GraphemeCol::ZERO);
                    Self::word_end_forward_once(buffer, big_word, prefer_current);
                }
                return;
            }
        }

        let end_of_next = if big_word {
            let mut end = idx;
            while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
                end += 1;
            }
            end
        } else {
            let class = char_class(chars[idx]);
            match class {
                CharClass::Cjk => idx,
                CharClass::Word | CharClass::Punctuation => {
                    let mut end = idx;
                    while end + 1 < chars.len() && char_class(chars[end + 1]) == class {
                        end += 1;
                    }
                    end
                }
                CharClass::Whitespace => idx,
            }
        };

        buffer.cursor_mut().set_col(GraphemeCol(end_of_next));
    }

    /// Moves cursor backward to the end of the previous word
    /// ge - moves to end of previous word
    pub fn word_end_backward(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_end_backward_once(buffer, false);
        }
    }

    /// Moves cursor backward to the end of the previous WORD
    /// gE - moves to end of previous WORD
    pub fn word_end_backward_big(buffer: &mut Buffer, count: usize) {
        for _ in 0..count {
            Self::word_end_backward_once(buffer, true);
        }
    }

    fn word_end_backward_once(buffer: &mut Buffer, big_word: bool) {
        // ge/gE: move backward to the end of the previous word/WORD.
        // Algorithm:
        //   1. Move back one position (crossing lines)
        //   2. Skip whitespace backward (crossing lines)
        //   3. Now we're on a non-ws char. Check: did we cross a word class boundary
        //      relative to the original cursor position? If not, we're still inside
        //      the same word, so skip to start of this word class, then skip whitespace
        //      again. The char we land on is the end of the previous word.
        //   4. If we DID cross a boundary (or whitespace), we're already at the end of
        //      the previous word. Done.

        let rope = buffer.rope();
        let orig_line = buffer.cursor().line();
        let orig_grapheme_col = buffer.cursor().col();

        if orig_line >= rope.len_lines() {
            return;
        }

        // Helper to get visible line content (trailing terminator stripped).
        let get_line_str = |l: usize| -> String { crate::display::line_content(rope, l) };

        // Helper to get the line's characters (terminator excluded).
        let get_chars = |l: usize| -> Vec<char> { word_units(&get_line_str(l)) };

        let orig_chars = get_chars(orig_line);
        // Word units use the same grapheme coordinates as the cursor.
        let orig_col = orig_grapheme_col.0;
        let orig_class = if orig_col < orig_chars.len() {
            Some(if big_word {
                // For WORD: any non-ws is the same "class"
                if Self::is_whitespace(orig_chars[orig_col]) {
                    CharClass::Whitespace
                } else {
                    CharClass::Word // treat all non-ws as Word for big_word
                }
            } else {
                char_class(orig_chars[orig_col])
            })
        } else {
            None
        };

        let mut line_idx = orig_line;
        let mut col = orig_col;

        // Step 1: Move back one position
        if col == 0 {
            if line_idx == 0 {
                return;
            }
            line_idx -= 1;
            let chars = get_chars(line_idx);
            col = if chars.is_empty() { 0 } else { chars.len() - 1 };
        } else {
            col -= 1;
        }

        // Step 2: Skip whitespace backward (crossing lines), landing on a non-ws char
        let (ws_line, ws_col) = Self::skip_whitespace_backward(line_idx, col, &get_chars);
        line_idx = ws_line;
        col = ws_col;

        let chars = get_chars(line_idx);
        if chars.is_empty() {
            buffer
                .cursor_mut()
                .set_position(line_idx, GraphemeCol::ZERO);
            return;
        }

        // Step 3: Check if we crossed a word boundary
        let current_class = if big_word {
            CharClass::Word // all non-ws treated as same for WORD
        } else {
            char_class(chars[col])
        };

        // If we're on a different line, or crossed whitespace, or different word class,
        // then we already crossed a word boundary — this IS the end of the previous word.
        let crossed_boundary = line_idx != orig_line
            || orig_class != Some(current_class)
            || col < orig_col.saturating_sub(1); // whitespace was skipped

        if crossed_boundary {
            buffer.cursor_mut().set_position(line_idx, GraphemeCol(col));
            return;
        }

        // Still in same word — skip to start of this word class, then find end of previous word
        if big_word {
            while col > 0 && !Self::is_whitespace(chars[col - 1]) {
                col -= 1;
            }
        } else {
            while col > 0 && char_class(chars[col - 1]) == current_class {
                col -= 1;
            }
        }

        // Move back one more — if we can't, there's no previous word; don't move
        if col == 0 {
            if line_idx == 0 {
                return; // No previous word — leave cursor unchanged
            }
            line_idx -= 1;
            let prev_chars = get_chars(line_idx);
            col = if prev_chars.is_empty() {
                0
            } else {
                prev_chars.len() - 1
            };
        } else {
            col -= 1;
        }

        // Skip whitespace backward again
        let (final_line, final_char_col) =
            Self::skip_whitespace_backward(line_idx, col, &get_chars);
        buffer
            .cursor_mut()
            .set_position(final_line, GraphemeCol(final_char_col));
    }

    /// Skip whitespace backward (crossing lines), returning the position of
    /// the first non-whitespace character found.
    pub(super) fn skip_whitespace_backward(
        mut line_idx: usize,
        mut col: usize,
        get_chars: &dyn Fn(usize) -> Vec<char>,
    ) -> (usize, usize) {
        loop {
            let chars = get_chars(line_idx);
            if chars.is_empty() {
                if line_idx == 0 {
                    return (0, 0);
                }
                line_idx -= 1;
                let prev = get_chars(line_idx);
                col = if prev.is_empty() { 0 } else { prev.len() - 1 };
                continue;
            }

            // Clamp col
            if col >= chars.len() {
                col = chars.len() - 1;
            }

            // Skip whitespace on this line
            while col > 0 && Self::is_whitespace(chars[col]) {
                col -= 1;
            }

            if !Self::is_whitespace(chars[col]) {
                return (line_idx, col);
            }

            // col == 0 and it's whitespace
            if line_idx == 0 {
                return (0, 0);
            }
            line_idx -= 1;
            let prev = get_chars(line_idx);
            col = if prev.is_empty() { 0 } else { prev.len() - 1 };
        }
    }
}

/// Classify each grapheme by its leading scalar so combining marks and ZWJ
/// sequences cannot create word boundaries inside a visible character.
fn word_units(line: &str) -> Vec<char> {
    line.graphemes(true)
        .filter_map(|g| g.chars().next())
        .collect()
}
