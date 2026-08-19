use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Returns true for control characters that should be displayed as caret notation.
/// Covers 0x00–0x1F (excluding \t and \n) and 0x7F (DEL).
pub fn is_control_char(ch: char) -> bool {
    let c = ch as u32;
    (c <= 0x1F && ch != '\t' && ch != '\n') || c == 0x7F
}

/// Returns the display width of a character, accounting for control chars (2 columns),
/// wide characters (2 columns), and normal characters (1 column).
pub fn char_display_width(ch: char) -> usize {
    if is_control_char(ch) {
        2
    } else {
        ch.width().unwrap_or(1)
    }
}

/// Returns the display width of a single grapheme cluster, accounting for
/// control chars (2 columns, caret notation) and emoji sequences.
///
/// Multi-scalar graphemes (VS16 emoji like ❤️, ZWJ sequences like 👨‍👩‍👧,
/// flags, skin tones) must be measured as a whole string: summing per-char
/// widths overcounts ZWJ sequences and undercounts VS16 sequences. This
/// matches how ratatui (unicode-width's str impl) measures them when
/// rendering, so cursor math and rendered cells agree.
pub fn grapheme_display_width(grapheme: &str) -> usize {
    let mut chars = grapheme.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => char_display_width(ch),
        _ => UnicodeWidthStr::width(grapheme),
    }
}

/// Returns the caret notation for a control character, or None if not a control char.
/// ESC → ['^','['], NUL → ['^','@'], DEL → ['^','?'], etc.
pub fn control_char_caret(ch: char) -> Option<[char; 2]> {
    let c = ch as u32;
    if c <= 0x1F && ch != '\t' && ch != '\n' {
        // 0x00 → '@', 0x01 → 'A', ..., 0x1A → 'Z', 0x1B → '[', etc.
        Some(['^', (c as u8 + b'@') as char])
    } else if c == 0x7F {
        Some(['^', '?'])
    } else {
        None
    }
}

/// Converts a character column index to a display column, accounting for tabs,
/// wide characters, and multi-scalar emoji graphemes.
///
/// Walks graphemes (not chars) so VS16/ZWJ emoji contribute their rendered
/// width. A `char_col` that lands mid-grapheme resolves to the grapheme's
/// starting display column.
pub fn char_col_to_display_col(text: &str, char_col: usize, tab_width: usize) -> usize {
    let mut display_col = 0;
    let mut chars_seen = 0;

    for grapheme in text.graphemes(true) {
        if chars_seen >= char_col {
            break;
        }

        if grapheme == "\t" {
            display_col += tab_width - (display_col % tab_width);
        } else {
            display_col += grapheme_display_width(grapheme);
        }
        chars_seen += grapheme.chars().count();
    }

    display_col
}

/// Converts a display column to a character column index, accounting for tabs,
/// wide characters, and multi-scalar emoji graphemes.
/// If the display column falls in the middle of a wide grapheme, returns the
/// char index of that grapheme's first char.
pub fn display_col_to_char_col(text: &str, display_col: usize, tab_width: usize) -> usize {
    let mut current_display = 0;
    let mut chars_seen = 0;

    for grapheme in text.graphemes(true) {
        if current_display >= display_col {
            return chars_seen;
        }

        if grapheme == "\t" {
            current_display += tab_width - (current_display % tab_width);
        } else {
            current_display += grapheme_display_width(grapheme);
        }

        if current_display > display_col {
            return chars_seen;
        }
        chars_seen += grapheme.chars().count();
    }

    // display_col is beyond end of text — return char count
    chars_seen
}

/// Calculates the display width of a string, accounting for tabs and wide characters.
pub fn display_width(text: &str, tab_width: usize) -> usize {
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            width += tab_width - (width % tab_width);
        } else {
            width += grapheme_display_width(grapheme);
        }
    }
    width
}

// ---------------------------------------------------------------------------
// Rope line-content helpers
//
// Single home for "what's the visible text on this line" when stripping the
// trailing terminator — `Buffer::line_text` is the canonical accessor for
// callers holding a `&Buffer`; these are for callers holding only a `&Rope`
// (closures that can't borrow the owning buffer, free functions, etc.).
// ---------------------------------------------------------------------------

/// Trim a single trailing line terminator (`\r\n`, `\n`, or a bare `\r`) from a
/// rope line slice, returning the content slice. No allocation.
///
/// The rope is LF-only by convention, but a stray `\r` can slip past the input
/// seams (mixed line endings — see `Buffer::line_text`), so a bare `\r` is
/// handled too.
pub fn trim_line_terminator(slice: RopeSlice<'_>) -> RopeSlice<'_> {
    let n = slice.len_chars();
    if n == 0 {
        return slice;
    }
    let end = match slice.char(n - 1) {
        '\n' if n >= 2 && slice.char(n - 2) == '\r' => n - 2,
        '\n' | '\r' => n - 1,
        _ => return slice,
    };
    slice.slice(..end)
}

/// Number of characters on `line` in `rope`, excluding the trailing line
/// terminator. Returns 0 for out-of-range lines.
pub fn line_content_len(rope: &Rope, line: usize) -> usize {
    if line >= rope.len_lines() {
        return 0;
    }
    trim_line_terminator(rope.line(line)).len_chars()
}

/// The visible content of `line` in `rope` as an owned `String`, with the
/// trailing line terminator stripped. Empty for out-of-range lines.
///
/// Mirrors `Buffer::line_text` for `&Rope`-only callers.
pub fn line_content(rope: &Rope, line: usize) -> String {
    if line >= rope.len_lines() {
        return String::new();
    }
    trim_line_terminator(rope.line(line)).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_roundtrip() {
        let text = "hello world";
        for i in 0..text.len() {
            let display = char_col_to_display_col(text, i, 4);
            let back = display_col_to_char_col(text, display, 4);
            assert_eq!(back, i, "roundtrip failed for char_col {i}");
        }
    }

    #[test]
    fn test_tab_handling() {
        let text = "\thello";
        // char_col 0 = display_col 0 (before the tab)
        assert_eq!(char_col_to_display_col(text, 0, 4), 0);
        // char_col 1 = display_col 4 (after the tab)
        assert_eq!(char_col_to_display_col(text, 1, 4), 4);
        // inverse: display_col 4 = char_col 1
        assert_eq!(display_col_to_char_col(text, 4, 4), 1);
        // display_col in the middle of a tab (e.g., 2) = char_col 0 (the tab itself)
        assert_eq!(display_col_to_char_col(text, 2, 4), 0);
    }

    #[test]
    fn test_wide_char() {
        let text = "a世b";
        // 'a' = width 1, '世' = width 2, 'b' = width 1
        assert_eq!(char_col_to_display_col(text, 0, 4), 0);
        assert_eq!(char_col_to_display_col(text, 1, 4), 1); // after 'a'
        assert_eq!(char_col_to_display_col(text, 2, 4), 3); // after '世'
                                                            // inverse
        assert_eq!(display_col_to_char_col(text, 0, 4), 0);
        assert_eq!(display_col_to_char_col(text, 1, 4), 1); // start of '世'
        assert_eq!(display_col_to_char_col(text, 2, 4), 1); // middle of '世' -> returns char 1
        assert_eq!(display_col_to_char_col(text, 3, 4), 2); // 'b'
    }

    #[test]
    fn test_display_width_with_wide_chars() {
        assert_eq!(display_width("a世b", 4), 4); // 1 + 2 + 1
        assert_eq!(display_width("a👨‍👩‍👧‍👦", 4), 3); // joined emoji is one glyph
        assert_eq!(display_width("hello", 4), 5);
        assert_eq!(display_width("\thello", 4), 9); // 4 + 5
        assert_eq!(display_width("", 4), 0);
    }

    #[test]
    fn test_display_col_beyond_text() {
        let text = "abc";
        assert_eq!(display_col_to_char_col(text, 10, 4), 3);
    }

    #[test]
    fn test_is_control_char() {
        assert!(is_control_char('\x00')); // NUL
        assert!(is_control_char('\x01')); // SOH
        assert!(is_control_char('\x1b')); // ESC
        assert!(is_control_char('\x7f')); // DEL
        assert!(!is_control_char('\t')); // tab excluded
        assert!(!is_control_char('\n')); // newline excluded
        assert!(!is_control_char('a'));
        assert!(!is_control_char(' ')); // space is 0x20, not control
    }

    #[test]
    fn test_control_char_caret() {
        assert_eq!(control_char_caret('\x00'), Some(['^', '@'])); // NUL
        assert_eq!(control_char_caret('\x01'), Some(['^', 'A'])); // SOH
        assert_eq!(control_char_caret('\x1b'), Some(['^', '['])); // ESC
        assert_eq!(control_char_caret('\x7f'), Some(['^', '?'])); // DEL
        assert_eq!(control_char_caret('\t'), None);
        assert_eq!(control_char_caret('a'), None);
    }

    #[test]
    fn test_char_display_width() {
        assert_eq!(char_display_width('\x00'), 2); // control char = 2
        assert_eq!(char_display_width('\x1b'), 2); // ESC = 2
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('世'), 2); // wide char = 2
    }

    #[test]
    fn test_display_width_with_control_chars() {
        // "a\x1b[b" = 'a'(1) + ESC(2) + '['(1) + 'b'(1) = 5
        assert_eq!(display_width("a\x1b[b", 4), 5);
    }

    #[test]
    fn test_control_char_roundtrip() {
        let text = "a\x01b";
        // 'a'=1, '\x01'=2, 'b'=1 → total width=4
        assert_eq!(display_width(text, 4), 4);
        // char_col 0 → display 0
        assert_eq!(char_col_to_display_col(text, 0, 4), 0);
        // char_col 1 → display 1 (after 'a')
        assert_eq!(char_col_to_display_col(text, 1, 4), 1);
        // char_col 2 → display 3 (after control char width 2)
        assert_eq!(char_col_to_display_col(text, 2, 4), 3);
        // roundtrip
        assert_eq!(display_col_to_char_col(text, 0, 4), 0);
        assert_eq!(display_col_to_char_col(text, 1, 4), 1);
        assert_eq!(display_col_to_char_col(text, 2, 4), 1); // mid-control → char 1
        assert_eq!(display_col_to_char_col(text, 3, 4), 2);
    }

    #[test]
    fn emoji_sequences_measure_rendered_width() {
        // Single-scalar emoji: 2 columns.
        assert_eq!(grapheme_display_width("😀"), 2);
        // VS16 emoji presentation (❤ + U+FE0F): renders 2 columns even
        // though the base char alone is narrow.
        assert_eq!(grapheme_display_width("❤\u{fe0f}"), 2);
        // ZWJ sequence: one 2-column glyph, not the sum of its scalars.
        assert_eq!(grapheme_display_width("👨\u{200d}👩\u{200d}👧"), 2);
    }

    #[test]
    fn char_col_conversion_handles_emoji_sequences() {
        // "x❤️y": chars are x(0) ❤(1) FE0F(2) y(3). The heart renders as
        // one 2-column glyph, so 'y' starts at display col 3.
        let text = "x❤\u{fe0f}y";
        assert_eq!(char_col_to_display_col(text, 0, 4), 0);
        assert_eq!(char_col_to_display_col(text, 1, 4), 1);
        assert_eq!(char_col_to_display_col(text, 3, 4), 3);
        assert_eq!(display_col_to_char_col(text, 3, 4), 3);
        // mid-heart display col resolves to the heart's first char
        assert_eq!(display_col_to_char_col(text, 2, 4), 1);

        // "x👨‍👩‍👧y": ZWJ family is chars 1..=5, one 2-column glyph;
        // 'y' (char 6) starts at display col 3.
        let text = "x👨\u{200d}👩\u{200d}👧y";
        assert_eq!(char_col_to_display_col(text, 6, 4), 3);
        assert_eq!(display_col_to_char_col(text, 3, 4), 6);

        assert_eq!(display_width("x❤\u{fe0f}y", 4), 4);
        assert_eq!(display_width("x👨\u{200d}👩\u{200d}👧y", 4), 4);
    }

    #[test]
    fn line_content_strips_terminators() {
        let rope = Rope::from_str("lf\ncrlf\r\nbare\rlast");
        // line 0: "lf\n"   → "lf"
        assert_eq!(line_content(&rope, 0), "lf");
        assert_eq!(line_content_len(&rope, 0), 2);
        // line 1: "crlf\r\n" → "crlf"
        assert_eq!(line_content(&rope, 1), "crlf");
        assert_eq!(line_content_len(&rope, 1), 4);
        // line 2: "bare\r" (bare CR is a line break in ropey) → "bare"
        assert_eq!(line_content(&rope, 2), "bare");
        assert_eq!(line_content_len(&rope, 2), 4);
        // line 3: "last" (no terminator) → "last"
        assert_eq!(line_content(&rope, 3), "last");
        assert_eq!(line_content_len(&rope, 3), 4);
    }

    #[test]
    fn line_content_out_of_range_is_empty() {
        let rope = Rope::from_str("only\n");
        // "only\n" → lines: 0 = "only\n", 1 = "" (trailing). Index 2 is OOB.
        assert_eq!(line_content(&rope, 2), "");
        assert_eq!(line_content_len(&rope, 2), 0);
        // The trailing empty line is in range and empty.
        assert_eq!(line_content(&rope, 1), "");
        assert_eq!(line_content_len(&rope, 1), 0);
    }

    #[test]
    fn line_content_empty_rope() {
        let rope = Rope::from_str("");
        assert_eq!(line_content(&rope, 0), "");
        assert_eq!(line_content_len(&rope, 0), 0);
    }
}
