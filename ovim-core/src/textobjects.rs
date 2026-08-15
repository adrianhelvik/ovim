use crate::buffer::Buffer;
use crate::unicode::CharCol;
use anyhow::Result;

/// Represents a text object selection range
/// NOTE: Uses half-open range semantics: [start_col, end_col)
/// The end_col is EXCLUSIVE (one past the last character to include)
/// This matches Rust's range semantics and buffer delete_range expectations.
///
/// Columns are `CharCol` (Unicode scalar value indices) — the unit rope
/// operations like `delete_range` use. This avoids the char/grapheme
/// confusion that previously caused the bug class fixed in commit 10226d2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextObjectRange {
    pub start_line: usize,
    pub start_col: CharCol,
    pub end_line: usize,
    pub end_col: CharCol, // EXCLUSIVE - one past the last character
}

/// Handles text object operations
pub struct TextObjects;

impl TextObjects {
    /// Gets the range for "inner word" (iw)
    pub fn inner_word(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if col >= chars.len() {
            return None;
        }

        // Find word boundaries
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

        // If cursor is on whitespace, select the whitespace sequence
        if chars[col].is_whitespace() {
            let mut start_col = col;
            while start_col > 0 && chars[start_col - 1].is_whitespace() {
                start_col -= 1;
            }
            let mut end_col = col;
            while end_col < chars.len() && chars[end_col].is_whitespace() {
                end_col += 1;
            }
            return Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col),
            });
        }

        // If cursor is on punctuation (not word char, not whitespace), select punctuation sequence
        if !is_word_char(chars[col]) {
            let mut start_col = col;
            while start_col > 0
                && !is_word_char(chars[start_col - 1])
                && !chars[start_col - 1].is_whitespace()
            {
                start_col -= 1;
            }
            let mut end_col = col;
            while end_col < chars.len()
                && !is_word_char(chars[end_col])
                && !chars[end_col].is_whitespace()
            {
                end_col += 1;
            }
            return Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col),
            });
        }

        // Find start of word
        let mut start_col = col;
        while start_col > 0 && is_word_char(chars[start_col - 1]) {
            start_col -= 1;
        }

        // Find end of word (exclusive - one past the last word character)
        let mut end_col = col;
        while end_col < chars.len() && is_word_char(chars[end_col]) {
            end_col += 1;
        }

        // Note: iw does NOT include trailing whitespace - that's aw (around word)
        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col), // Already exclusive
        })
    }

    /// Gets the range for "around word" (aw)
    pub fn around_word(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if col >= chars.len() {
            return None;
        }

        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

        // If cursor is on whitespace, select the whitespace
        if !is_word_char(chars[col]) {
            let mut start_col = col;
            while start_col > 0 && !is_word_char(chars[start_col - 1]) {
                start_col -= 1;
            }

            let mut end_col = col;
            while end_col < chars.len() && !is_word_char(chars[end_col]) {
                end_col += 1;
            }

            return Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col), // Already exclusive
            });
        }

        // Find start of word
        let mut start_col = col;
        while start_col > 0 && is_word_char(chars[start_col - 1]) {
            start_col -= 1;
        }

        // Find end of word
        let mut end_col = col;
        while end_col < chars.len() && is_word_char(chars[end_col]) {
            end_col += 1;
        }

        // Include trailing whitespace, but only if there's non-whitespace after it
        // (i.e., don't include trailing space if it's the last word on the line)
        let word_end = end_col;
        while end_col < chars.len() && chars[end_col].is_whitespace() && chars[end_col] != '\n' {
            end_col += 1;
        }

        // Check if we found trailing whitespace followed by more content
        let has_trailing_space = end_col > word_end;
        let has_content_after = end_col < chars.len();

        // Vim's aw behavior:
        // - If there's trailing whitespace before more content, include it
        // - If this is the last word (no trailing whitespace or no content after), include leading whitespace
        if has_trailing_space && has_content_after {
            // Keep end_col with trailing whitespace included
        } else {
            // Reset end_col to just after the word
            end_col = word_end;

            // Include leading whitespace instead
            while start_col > 0 && chars[start_col - 1].is_whitespace() {
                start_col -= 1;
            }
        }

        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col), // Already exclusive
        })
    }

    /// Gets the range for "inner WORD" (iW).
    /// WORD uses non-whitespace runs (punctuation is part of the WORD).
    pub fn inner_big_word(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if col >= chars.len() {
            return None;
        }

        // If cursor is on whitespace, select the whitespace sequence.
        if chars[col].is_whitespace() {
            let mut start_col = col;
            while start_col > 0 && chars[start_col - 1].is_whitespace() {
                start_col -= 1;
            }
            let mut end_col = col;
            while end_col < chars.len() && chars[end_col].is_whitespace() {
                end_col += 1;
            }
            return Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col),
            });
        }

        // For WORD, include contiguous non-whitespace.
        let mut start_col = col;
        while start_col > 0 && !chars[start_col - 1].is_whitespace() {
            start_col -= 1;
        }

        let mut end_col = col;
        while end_col < chars.len() && !chars[end_col].is_whitespace() {
            end_col += 1;
        }

        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the range for "around WORD" (aW).
    /// WORD uses non-whitespace runs (punctuation is part of the WORD).
    pub fn around_big_word(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if col >= chars.len() {
            return None;
        }

        // If cursor is on whitespace, select the whitespace sequence.
        if chars[col].is_whitespace() {
            let mut start_col = col;
            while start_col > 0 && chars[start_col - 1].is_whitespace() {
                start_col -= 1;
            }
            let mut end_col = col;
            while end_col < chars.len() && chars[end_col].is_whitespace() {
                end_col += 1;
            }
            return Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col),
            });
        }

        // Find WORD boundaries (contiguous non-whitespace).
        let mut start_col = col;
        while start_col > 0 && !chars[start_col - 1].is_whitespace() {
            start_col -= 1;
        }

        let mut end_col = col;
        while end_col < chars.len() && !chars[end_col].is_whitespace() {
            end_col += 1;
        }

        // Include trailing whitespace if followed by more content,
        // otherwise include leading whitespace.
        let word_end = end_col;
        while end_col < chars.len() && chars[end_col].is_whitespace() && chars[end_col] != '\n' {
            end_col += 1;
        }

        let has_trailing_space = end_col > word_end;
        let has_content_after = end_col < chars.len();
        if !(has_trailing_space && has_content_after) {
            end_col = word_end;
            while start_col > 0 && chars[start_col - 1].is_whitespace() {
                start_col -= 1;
            }
        }

        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the range for a quoted string (inner or around)
    /// quote_char: the quote character (', ", `)
    /// include_quotes: true for "around", false for "inner"
    pub fn quoted_string(
        buffer: &Buffer,
        quote_char: char,
        include_quotes: bool,
    ) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if chars.is_empty() {
            return None;
        }

        // Fix Bug 2: Handle escaped quotes by tracking preceding backslashes
        // Helper to check if a character at position is escaped
        let is_escaped = |pos: usize| -> bool {
            if pos == 0 {
                return false;
            }
            // Count consecutive backslashes before this position
            let mut backslash_count = 0;
            let mut check_pos = pos;
            while check_pos > 0 && chars[check_pos - 1] == '\\' {
                backslash_count += 1;
                check_pos -= 1;
            }
            // Character is escaped if odd number of backslashes precede it
            backslash_count % 2 == 1
        };

        // Find all quote positions on this line (non-escaped)
        let quote_positions: Vec<usize> = chars
            .iter()
            .enumerate()
            .filter(|&(i, &c)| c == quote_char && !is_escaped(i))
            .map(|(i, _)| i)
            .collect();

        if quote_positions.len() < 2 {
            return None; // Need at least 2 quotes to form a pair
        }

        // Find which quote pair contains the cursor
        // Quotes are paired: 0-1, 2-3, 4-5, etc.
        let col = col.min(chars.len().saturating_sub(1));

        // Find the pair that contains the cursor
        let mut start_col = None;
        let mut end_col = None;

        for i in (0..quote_positions.len()).step_by(2) {
            if i + 1 >= quote_positions.len() {
                break; // Odd number of quotes, last one is unpaired
            }
            let open_pos = quote_positions[i];
            let close_pos = quote_positions[i + 1];

            // Select the first pair whose closing quote is at or after the
            // cursor: this is the pair the cursor sits inside, or — when the
            // cursor is before all quotes on the line — the next pair forward,
            // matching Vim's forward search for ci"/di".
            if col <= close_pos {
                start_col = Some(open_pos);
                end_col = Some(close_pos);
                break;
            }
        }

        let start_col = start_col?;
        let end_col = end_col?;

        if include_quotes {
            // "around" - include the quotes
            Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col),
                end_line: line_idx,
                end_col: CharCol(end_col + 1),
            })
        } else {
            // "inner" - exclude the quotes
            if start_col + 1 >= end_col {
                return None; // Empty string
            }
            Some(TextObjectRange {
                start_line: line_idx,
                start_col: CharCol(start_col + 1),
                end_line: line_idx,
                end_col: CharCol(end_col),
            })
        }
    }

    /// Gets the range for parentheses, brackets, or braces
    /// Searches across lines using the rope buffer, tracking depth.
    /// open_char: '(', '[', or '{'
    /// close_char: ')', ']', or '}'
    /// include_delimiters: true for "around", false for "inner"
    pub fn paired_delimiters(
        buffer: &Buffer,
        open_char: char,
        close_char: char,
        include_delimiters: bool,
    ) -> Option<TextObjectRange> {
        let cursor_line = buffer.cursor().line();
        let cursor_col = buffer.cursor_char_col().0;
        let rope = buffer.rope();
        let line_count = buffer.line_count();

        if cursor_line >= line_count {
            return None;
        }

        // Convert cursor position to absolute char offset in the rope
        let cursor_char_offset = rope.line_to_char(cursor_line) + cursor_col;

        // Search backward from cursor for unmatched open delimiter
        let mut open_pos = None;
        let mut depth: i32 = 0;

        // Handle the case where cursor is ON an open delimiter
        if cursor_char_offset < rope.len_chars() {
            let ch = rope.char(cursor_char_offset);
            if ch == open_char {
                open_pos = Some(cursor_char_offset);
            }
        }

        if open_pos.is_none() {
            // Search backward
            for i in (0..cursor_char_offset).rev() {
                let ch = rope.char(i);
                if ch == close_char {
                    depth += 1;
                } else if ch == open_char {
                    if depth == 0 {
                        open_pos = Some(i);
                        break;
                    }
                    depth -= 1;
                }
            }
        }

        let open_offset = open_pos?;

        // Search forward from open delimiter for matching close
        let mut close_pos = None;
        depth = 0;
        for i in (open_offset + 1)..rope.len_chars() {
            let ch = rope.char(i);
            if ch == open_char {
                depth += 1;
            } else if ch == close_char {
                if depth == 0 {
                    close_pos = Some(i);
                    break;
                }
                depth -= 1;
            }
        }

        let close_offset = close_pos?;

        // Convert absolute offsets back to (line, col)
        let open_line = rope.char_to_line(open_offset);
        let open_col = open_offset - rope.line_to_char(open_line);
        let close_line = rope.char_to_line(close_offset);
        let close_col = close_offset - rope.line_to_char(close_line);

        if include_delimiters {
            // "around" - include the delimiters
            Some(TextObjectRange {
                start_line: open_line,
                start_col: CharCol(open_col),
                end_line: close_line,
                end_col: CharCol(close_col + 1),
            })
        } else {
            // "inner" - exclude the delimiters
            // Start is one char after open delimiter
            let inner_start_offset = open_offset + 1;
            let inner_end_offset = close_offset;

            if inner_start_offset >= inner_end_offset {
                return None; // Empty content
            }

            let start_line = rope.char_to_line(inner_start_offset);
            let start_col = inner_start_offset - rope.line_to_char(start_line);

            Some(TextObjectRange {
                start_line,
                start_col: CharCol(start_col),
                end_line: close_line,
                end_col: CharCol(close_col),
            })
        }
    }

    /// Deletes text within a text object range
    /// Note: range.end_col is exclusive (one past the last character)
    pub fn delete_range(buffer: &mut Buffer, range: TextObjectRange) -> Result<String> {
        let start_char = buffer.rope().line_to_char(range.start_line) + range.start_col.0;
        let end_char = buffer.rope().line_to_char(range.end_line) + range.end_col.0;

        let deleted = buffer.rope().slice(start_char..end_char).to_string();
        buffer.rope_mut().remove(start_char..end_char);

        // Position cursor at start of deleted range.
        // range.start_col is a CharCol — convert via the line text to avoid
        // the char/grapheme confusion bug class (commit 10226d2).
        buffer.set_cursor_char_col(range.start_line, range.start_col);

        Ok(deleted)
    }

    /// Yanks text within a text object range
    /// Note: range.end_col is exclusive (one past the last character)
    pub fn yank_range(buffer: &Buffer, range: TextObjectRange) -> Result<String> {
        let start_char = buffer.rope().line_to_char(range.start_line) + range.start_col.0;
        let end_char = buffer.rope().line_to_char(range.end_line) + range.end_col.0;

        Ok(buffer.rope().slice(start_char..end_char).to_string())
    }

    /// Gets the range for an HTML/XML tag (inner or around)
    /// include_tags: true for "around" (includes opening and closing tags), false for "inner" (content only)
    pub fn tag(buffer: &Buffer, include_tags: bool) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        // Get the full text from the buffer to search across multiple lines
        let text = buffer.rope().to_string();
        let cursor_offset = buffer.rope().line_to_char(line_idx) + col;

        // Find the opening tag before or at cursor
        let mut tag_start = None;
        let mut tag_name = None;
        let chars: Vec<char> = text.chars().collect();

        // Search backward for opening tag (index 0 included: `while i > 0`
        // never tested a `<` at buffer start — OV-00287)
        if chars.is_empty() {
            return None;
        }
        let mut i = cursor_offset.min(chars.len() - 1);
        loop {
            if chars[i] == '<' && i + 1 < chars.len() && chars[i + 1] != '/' {
                // Found potential opening tag
                let mut name_end = i + 1;
                while name_end < chars.len()
                    && chars[name_end] != '>'
                    && !chars[name_end].is_whitespace()
                {
                    name_end += 1;
                }
                if name_end < chars.len() {
                    let name: String = chars[(i + 1)..name_end].iter().collect();
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':')
                    {
                        tag_start = Some(i);
                        tag_name = Some(name);
                        break;
                    }
                }
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }

        let tag_start = tag_start?;
        let tag_name = tag_name?;

        // Find the end of the opening tag
        let mut opening_tag_end = tag_start;
        while opening_tag_end < chars.len() && chars[opening_tag_end] != '>' {
            opening_tag_end += 1;
        }
        if opening_tag_end >= chars.len() {
            return None;
        }

        // Check for self-closing tag
        if opening_tag_end > 0 && chars[opening_tag_end - 1] == '/' {
            // Self-closing tag like <br/> - no content
            return None;
        }

        // Find the closing tag
        let mut depth = 1;
        let mut search_pos = opening_tag_end + 1;

        while search_pos < chars.len() && depth > 0 {
            if chars[search_pos] == '<' {
                // Check if this is an opening or closing tag
                if search_pos + 1 < chars.len() && chars[search_pos + 1] == '/' {
                    // Potential closing tag
                    let mut name_end = search_pos + 2;
                    while name_end < chars.len() && chars[name_end] != '>' {
                        name_end += 1;
                    }
                    let found_name: String = chars[(search_pos + 2)..name_end].iter().collect();
                    if found_name == tag_name {
                        depth -= 1;
                        if depth == 0 {
                            // Found matching closing tag. Downstream end_col
                            // is EXCLUSIVE, so both ends are one-past-last
                            // (the old inclusive values kept the last content
                            // char on `dit` and the closing `>` on `dat` —
                            // OV-00286).
                            let content_start = opening_tag_end + 1;
                            let content_end = search_pos;
                            let closing_tag_end = name_end + 1;

                            // Convert char offsets to line/col positions
                            let (start_line, start_col, end_line, end_col) = if include_tags {
                                // Include opening and closing tags
                                Self::char_offset_to_position(buffer, tag_start, closing_tag_end)
                            } else {
                                // Inner - just the content between tags
                                if content_start >= content_end {
                                    return None; // Empty tag
                                }
                                Self::char_offset_to_position(buffer, content_start, content_end)
                            };

                            return Some(TextObjectRange {
                                start_line,
                                start_col: CharCol(start_col),
                                end_line,
                                end_col: CharCol(end_col),
                            });
                        }
                    }
                } else if search_pos + 1 < chars.len()
                    && chars[search_pos + 1] != '!'
                    && chars[search_pos + 1] != '?'
                {
                    // Potential opening tag (not a comment or processing instruction)
                    let mut name_end = search_pos + 1;
                    while name_end < chars.len()
                        && chars[name_end] != '>'
                        && !chars[name_end].is_whitespace()
                    {
                        name_end += 1;
                    }
                    let found_name: String = chars[(search_pos + 1)..name_end].iter().collect();
                    if found_name == tag_name {
                        // Check if it's not self-closing
                        let mut tag_end = name_end;
                        while tag_end < chars.len() && chars[tag_end] != '>' {
                            tag_end += 1;
                        }
                        if tag_end > 0 && chars[tag_end - 1] != '/' {
                            depth += 1;
                        }
                    }
                }
            }
            search_pos += 1;
        }

        None
    }

    /// Helper to convert character offsets to line/col positions
    fn char_offset_to_position(
        buffer: &Buffer,
        start_offset: usize,
        end_offset: usize,
    ) -> (usize, usize, usize, usize) {
        let rope = buffer.rope();

        let start_line = rope.char_to_line(start_offset);
        let start_line_offset = rope.line_to_char(start_line);
        let start_col = start_offset.saturating_sub(start_line_offset);

        let end_line = rope.char_to_line(end_offset);
        let end_line_offset = rope.line_to_char(end_line);
        let end_col = end_offset.saturating_sub(end_line_offset);

        (start_line, start_col, end_line, end_col)
    }

    /// Gets the range for "inner paragraph" (ip)
    /// A paragraph is a sequence of non-blank lines separated by blank lines
    pub fn inner_paragraph(buffer: &Buffer) -> Option<TextObjectRange> {
        let cursor = buffer.cursor();
        let current_line = cursor.line();
        let line_count = buffer.line_count();

        if current_line >= line_count {
            return None;
        }

        // Helper to check if a line is blank
        let is_blank = |line_idx: usize| -> bool {
            if line_idx >= line_count {
                return true;
            }
            crate::display::line_content(buffer.rope(), line_idx)
                .trim()
                .is_empty()
        };

        // If we're on a blank line, return None for inner paragraph
        if is_blank(current_line) {
            return None;
        }

        // Find start of paragraph (first non-blank line in sequence)
        let mut start_line = current_line;
        while start_line > 0 && !is_blank(start_line - 1) {
            start_line -= 1;
        }

        // Find end of paragraph (last non-blank line in sequence)
        let mut end_line = current_line;
        while end_line + 1 < line_count && !is_blank(end_line + 1) {
            end_line += 1;
        }

        // Paragraph operations are linewise - include the trailing newline
        // by pointing to start of next line (or end of file if last line)
        if end_line + 1 < line_count {
            Some(TextObjectRange {
                start_line,
                start_col: CharCol::ZERO,
                end_line: end_line + 1,
                end_col: CharCol::ZERO,
            })
        } else {
            // Last line of file - use the actual end
            let end_col = crate::display::line_content_len(buffer.rope(), end_line);
            Some(TextObjectRange {
                start_line,
                start_col: CharCol::ZERO,
                end_line,
                end_col: CharCol(end_col),
            })
        }
    }

    /// Gets the range for "around paragraph" (ap)
    /// Includes the paragraph and surrounding blank lines
    pub fn around_paragraph(buffer: &Buffer) -> Option<TextObjectRange> {
        let cursor = buffer.cursor();
        let current_line = cursor.line();
        let line_count = buffer.line_count();

        if current_line >= line_count {
            return None;
        }

        // Helper to check if a line is blank
        let is_blank = |line_idx: usize| -> bool {
            if line_idx >= line_count {
                return true;
            }
            crate::display::line_content(buffer.rope(), line_idx)
                .trim()
                .is_empty()
        };

        let mut start_line = current_line;
        let mut end_line = current_line;

        // If we're on a blank line, select blank lines
        if is_blank(current_line) {
            // Find start of blank sequence
            while start_line > 0 && is_blank(start_line - 1) {
                start_line -= 1;
            }
            // Find end of blank sequence
            while end_line + 1 < line_count && is_blank(end_line + 1) {
                end_line += 1;
            }
        } else {
            // Find start of paragraph
            while start_line > 0 && !is_blank(start_line - 1) {
                start_line -= 1;
            }
            // Find end of paragraph
            while end_line + 1 < line_count && !is_blank(end_line + 1) {
                end_line += 1;
            }

            // Include trailing blank lines
            while end_line + 1 < line_count && is_blank(end_line + 1) {
                end_line += 1;
            }

            // If no trailing blank lines, include leading blank lines
            if end_line + 1 >= line_count || !is_blank(end_line) {
                while start_line > 0 && is_blank(start_line - 1) {
                    start_line -= 1;
                }
            }
        }

        // Paragraph operations are linewise - include the trailing newline
        // by pointing to start of next line (or end of file if last line)
        if end_line + 1 < line_count {
            Some(TextObjectRange {
                start_line,
                start_col: CharCol::ZERO,
                end_line: end_line + 1,
                end_col: CharCol::ZERO,
            })
        } else {
            // Last line of file - use the actual end
            let end_col = crate::display::line_content_len(buffer.rope(), end_line);
            Some(TextObjectRange {
                start_line,
                start_col: CharCol::ZERO,
                end_line,
                end_col: CharCol(end_col),
            })
        }
    }

    /// Gets the range for "inner sentence" (is)
    /// A sentence ends with '.', '!', or '?' followed by whitespace or end of line
    pub fn inner_sentence(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if chars.is_empty() {
            return None;
        }

        let is_sentence_end = |c: char| c == '.' || c == '!' || c == '?';

        // Find start of sentence (after previous sentence end + whitespace)
        let mut start_col = 0;
        for i in (0..col).rev() {
            if is_sentence_end(chars[i]) {
                // Skip whitespace after sentence end
                start_col = i + 1;
                while start_col < chars.len() && chars[start_col].is_whitespace() {
                    start_col += 1;
                }
                break;
            }
        }

        // Find end of sentence (next sentence end)
        let mut end_col = col;
        while end_col < chars.len() && !is_sentence_end(chars[end_col]) {
            end_col += 1;
        }

        // For inner sentence, don't include the punctuation (exclusive end handles this)
        // end_col now points to the punctuation or past the end
        // With exclusive semantics, this is correct - we want up to but not including this position

        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the range for "around sentence" (as)
    /// Includes the sentence and trailing whitespace
    pub fn around_sentence(buffer: &Buffer) -> Option<TextObjectRange> {
        let line_idx = buffer.cursor().line();
        let col = buffer.cursor_char_col().0;

        if line_idx >= buffer.line_count() {
            return None;
        }

        let line_text = crate::display::line_content(buffer.rope(), line_idx);
        let chars: Vec<char> = line_text.chars().collect();

        if chars.is_empty() {
            return None;
        }

        let is_sentence_end = |c: char| c == '.' || c == '!' || c == '?';

        // Find start of sentence
        let mut start_col = 0;
        for i in (0..col).rev() {
            if is_sentence_end(chars[i]) {
                start_col = i + 1;
                while start_col < chars.len() && chars[start_col].is_whitespace() {
                    start_col += 1;
                }
                break;
            }
        }

        // Find end of sentence
        let mut end_col = col;
        while end_col < chars.len() && !is_sentence_end(chars[end_col]) {
            end_col += 1;
        }

        // Include punctuation and trailing whitespace
        if end_col < chars.len() {
            end_col += 1; // Move past the punctuation
            while end_col < chars.len() && chars[end_col].is_whitespace() {
                end_col += 1;
            }
            // end_col is now exclusive - points one past the last character to include
        } else {
            // At end of line - use chars.len() as exclusive end
            end_col = chars.len();
        }

        Some(TextObjectRange {
            start_line: line_idx,
            start_col: CharCol(start_col),
            end_line: line_idx,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the indentation level in visual columns.
    fn get_indent_level(line: &str, tab_width: usize) -> usize {
        crate::indentation::leading_width(line, tab_width)
    }

    /// Gets the range for "inner indent" (ii) - lines with same or greater indentation
    pub fn inner_indent(buffer: &Buffer, tab_width: usize) -> Option<TextObjectRange> {
        let cursor = buffer.cursor();
        let line_idx = cursor.line();
        let line_count = buffer.line_count();

        if line_idx >= line_count {
            return None;
        }

        let current_line = buffer.line_text(line_idx)?;

        // Skip blank lines for indent calculation
        if current_line.trim().is_empty() {
            return None;
        }

        let base_indent = Self::get_indent_level(&current_line, tab_width);

        // Find start of indent block (going up)
        let mut start_line = line_idx;
        while start_line > 0 {
            let prev_line = buffer.line_text(start_line - 1)?;

            // Stop at blank lines or lines with less indentation
            if prev_line.trim().is_empty()
                || Self::get_indent_level(&prev_line, tab_width) < base_indent
            {
                break;
            }
            start_line -= 1;
        }

        // Find end of indent block (going down)
        let mut end_line = line_idx;
        while end_line < line_count - 1 {
            let next_line = buffer.line_text(end_line + 1)?;

            // Stop at blank lines or lines with less indentation
            if next_line.trim().is_empty()
                || Self::get_indent_level(&next_line, tab_width) < base_indent
            {
                break;
            }
            end_line += 1;
        }

        // Get the length of the last line for end_col
        let last_line = buffer.line_text(end_line)?;
        let end_col = last_line.chars().count();

        Some(TextObjectRange {
            start_line,
            start_col: CharCol::ZERO,
            end_line,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the range for "around indent" (ai) - includes surrounding blank lines
    pub fn around_indent(buffer: &Buffer, tab_width: usize) -> Option<TextObjectRange> {
        let mut range = Self::inner_indent(buffer, tab_width)?;
        let line_count = buffer.line_count();

        // Extend upward to include blank lines
        while range.start_line > 0 {
            let prev_line = buffer.line_text(range.start_line - 1)?;
            if prev_line.trim().is_empty() {
                range.start_line -= 1;
            } else {
                break;
            }
        }

        // Extend downward to include blank lines
        while range.end_line < line_count - 1 {
            let next_line = buffer.line_text(range.end_line + 1)?;
            if next_line.trim().is_empty() {
                range.end_line += 1;
                // Update end_col for the new last line
                let last_line = buffer.line_text(range.end_line)?;
                range.end_col = CharCol(last_line.chars().count());
            } else {
                break;
            }
        }

        Some(range)
    }

    /// Gets the range for "inner function" (if) text object
    /// Selects the body of the function (between { and }) without the signature
    pub fn inner_function(buffer: &Buffer) -> Option<TextObjectRange> {
        let cursor_line = buffer.cursor().line();

        // Find the opening brace of the function containing the cursor
        let (open_line, open_col) = Self::find_function_open_brace(buffer, cursor_line)?;

        // Find the matching closing brace
        let (close_line, close_col) = Self::find_matching_close_brace(buffer, open_line, open_col)?;

        // For "inner function", we want the content between { and }
        // If { is at end of line, start from next line col 0
        // If } is at start of line, end at previous line end
        let (start_line, start_col) = if open_col + 1 < buffer.line_text(open_line)?.chars().count()
        {
            (open_line, open_col + 1)
        } else {
            (open_line + 1, 0)
        };

        let (end_line, end_col) = if close_col > 0 {
            (close_line, close_col)
        } else if close_line > 0 {
            let prev_line = buffer.line_text(close_line - 1)?;
            (close_line - 1, prev_line.chars().count())
        } else {
            (close_line, close_col)
        };

        // Validate range
        if start_line > end_line || (start_line == end_line && start_col >= end_col) {
            return None;
        }

        Some(TextObjectRange {
            start_line,
            start_col: CharCol(start_col),
            end_line,
            end_col: CharCol(end_col),
        })
    }

    /// Gets the range for "around function" (af) text object
    /// Selects the entire function including signature and braces
    pub fn around_function(buffer: &Buffer) -> Option<TextObjectRange> {
        let cursor_line = buffer.cursor().line();

        // Find the opening brace of the function containing the cursor
        let (open_line, open_col) = Self::find_function_open_brace(buffer, cursor_line)?;

        // Find the function signature start
        let start_line = Self::find_function_signature_start(buffer, open_line)?;

        // Find the matching closing brace
        let (close_line, close_col) = Self::find_matching_close_brace(buffer, open_line, open_col)?;

        // End position includes the closing brace
        let end_line = close_line;
        let end_col = close_col + 1;

        // Include trailing newline if present
        let line = buffer.line_text(end_line)?;
        let end_col = if end_col >= line.chars().count() {
            line.chars().count()
        } else {
            end_col
        };

        Some(TextObjectRange {
            start_line,
            start_col: CharCol::ZERO,
            end_line,
            end_col: CharCol(end_col),
        })
    }

    /// Finds the opening brace { of a function containing the given line
    fn find_function_open_brace(buffer: &Buffer, cursor_line: usize) -> Option<(usize, usize)> {
        // First check if we're inside a function body
        // Search backward for unmatched {
        let mut depth = 0;
        let mut search_line = cursor_line;

        loop {
            let line = buffer.line_text(search_line)?;
            let chars: Vec<char> = line.chars().collect();

            // Search from end of line (or cursor col if on cursor line)
            let end_pos = if search_line == cursor_line {
                buffer.cursor_char_col().0.min(chars.len())
            } else {
                chars.len()
            };

            for (col, &ch) in chars.iter().enumerate().take(end_pos).rev() {
                if ch == '}' {
                    depth += 1;
                } else if ch == '{' {
                    if depth == 0 {
                        // Found unmatched opening brace
                        return Some((search_line, col));
                    }
                    depth -= 1;
                }
            }

            if search_line == 0 {
                break;
            }
            search_line -= 1;
        }

        None
    }

    /// Finds the start of function signature (first non-blank line before opening brace)
    fn find_function_signature_start(buffer: &Buffer, open_brace_line: usize) -> Option<usize> {
        let mut start = open_brace_line;

        // Go backward to find where the function definition starts
        // Look for common function keywords or the start of attributes/decorators
        while start > 0 {
            let prev_line = buffer.line_text(start - 1)?;
            let trimmed = prev_line.trim();

            // Stop if we hit a blank line or a closing brace
            if trimmed.is_empty() || trimmed == "}" || trimmed == "};" {
                break;
            }

            // Continue if line is part of signature (fn, pub, async, #[attr], @decorator, def, etc.)
            if trimmed.starts_with("fn ")
                || trimmed.starts_with("pub ")
                || trimmed.starts_with("async ")
                || trimmed.starts_with('#')
                || trimmed.starts_with('@')
                || trimmed.starts_with("def ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("impl ")
                || trimmed.contains('(')
                || trimmed.ends_with(',')
                || trimmed.ends_with('>')
            {
                start -= 1;
            } else {
                break;
            }
        }

        Some(start)
    }

    /// Finds the matching closing brace for an opening brace
    fn find_matching_close_brace(
        buffer: &Buffer,
        open_line: usize,
        open_col: usize,
    ) -> Option<(usize, usize)> {
        let line_count = buffer.line_count();
        let mut depth = 1;
        let mut search_line = open_line;

        // Start searching after the opening brace
        let first_line = buffer.line_text(open_line)?;
        let chars: Vec<char> = first_line.chars().collect();
        for (col, &ch) in chars.iter().enumerate().skip(open_col + 1) {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some((search_line, col));
                }
            }
        }

        search_line += 1;
        while search_line < line_count {
            let line = buffer.line_text(search_line)?;
            for (col, ch) in line.chars().enumerate() {
                if ch == '{' {
                    depth += 1;
                } else if ch == '}' {
                    depth -= 1;
                    if depth == 0 {
                        return Some((search_line, col));
                    }
                }
            }
            search_line += 1;
        }

        None
    }
}
