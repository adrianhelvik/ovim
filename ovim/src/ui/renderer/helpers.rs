use crate::display::control_char_caret;
use crate::ui::renderer::markdown_conceal::LineTransform;
use std::ops::Range;

/// Result of expanding tabs and control characters for rendering.
/// Carries the expanded text alongside the mappings needed to remap
/// byte offsets, char indices, and control-char ranges from the
/// original source text into the expanded coordinate space.
pub struct ExpandedLine {
    /// Expanded text (tabs → spaces, control chars → caret notation).
    pub text: String,
    /// Byte mapping: `byte_mapping[i] = (original_byte, expanded_byte)`.
    pub byte_mapping: Vec<(usize, usize)>,
    /// Byte ranges in the expanded string that correspond to control characters.
    pub control_ranges: Vec<Range<usize>>,
    /// Char mapping: `char_mapping[original_char_idx] = expanded_char_idx`.
    pub char_mapping: Vec<usize>,
}

/// Expands tabs and control characters for rendering.
pub fn expand_tabs_with_mapping(text: &str, tab_width: usize) -> ExpandedLine {
    let mut result = String::with_capacity(text.len() * 2);
    let mut display_col = 0;
    let mut byte_mapping = Vec::new(); // original_byte_idx -> expanded_byte_idx
    let mut control_ranges = Vec::new();
    let mut char_mapping = Vec::new(); // original_char_idx -> expanded_char_idx

    let mut expanded_byte_pos = 0;
    let mut expanded_char_idx = 0;

    // Walk graphemes so tab stops after multi-scalar emoji (VS16/ZWJ
    // sequences) use the rendered width, not the sum of scalar widths.
    // Mappings stay per-char: each char inside a copied grapheme maps to
    // its own expanded position (graphemes are copied verbatim).
    use unicode_segmentation::UnicodeSegmentation;
    for (grapheme_start, grapheme) in text.grapheme_indices(true) {
        let mut grapheme_chars = grapheme.chars();
        let first_char = grapheme_chars.next().unwrap_or(' ');
        let is_single_char = grapheme_chars.next().is_none();

        if is_single_char && first_char == '\t' {
            byte_mapping.push((grapheme_start, expanded_byte_pos));
            char_mapping.push(expanded_char_idx);
            // Calculate spaces needed to reach next tab stop
            let spaces_to_add = tab_width - (display_col % tab_width);
            result.push_str(&" ".repeat(spaces_to_add));
            expanded_byte_pos += spaces_to_add;
            expanded_char_idx += spaces_to_add;
            display_col += spaces_to_add;
        } else if is_single_char && control_char_caret(first_char).is_some() {
            let caret = control_char_caret(first_char).unwrap();
            byte_mapping.push((grapheme_start, expanded_byte_pos));
            char_mapping.push(expanded_char_idx);
            let start = expanded_byte_pos;
            result.push(caret[0]); // '^'
            result.push(caret[1]); // notation char
            expanded_byte_pos += 2;
            expanded_char_idx += 2;
            display_col += 2;
            control_ranges.push(start..expanded_byte_pos);
        } else {
            for (offset, ch) in grapheme.char_indices() {
                byte_mapping.push((grapheme_start + offset, expanded_byte_pos));
                char_mapping.push(expanded_char_idx);
                result.push(ch);
                expanded_byte_pos += ch.len_utf8();
                expanded_char_idx += 1;
            }
            // Rendered width of the whole grapheme (emoji sequences = 2)
            display_col += crate::display::grapheme_display_width(grapheme);
        }
    }

    // Add final mapping for end position
    byte_mapping.push((text.len(), expanded_byte_pos));
    char_mapping.push(expanded_char_idx);

    ExpandedLine {
        text: result,
        byte_mapping,
        control_ranges,
        char_mapping,
    }
}

/// Compose a conceal transform (src->view) with tab expansion mapping.
pub fn compose_conceal_and_tabs(
    original: &str,
    transform: &LineTransform,
    tab_width: usize,
) -> ExpandedLine {
    // Expand the transformed text (post-conceal) for tabs/control chars
    let ExpandedLine {
        text: expanded,
        byte_mapping: byte_map_after,
        control_ranges: control,
        char_mapping: char_map_after,
    } = expand_tabs_with_mapping(&transform.text, tab_width);

    // Build byte mapping from original to expanded using src_to_view -> view_to_src/byte map
    let mut orig_byte_to_expanded: Vec<(usize, usize)> = Vec::with_capacity(original.len() + 1);
    let mut orig_char_to_expanded: Vec<usize> = Vec::new();

    // Map original bytes to view chars, then to expanded bytes
    // src_to_view len = src len + 1 (sentinel). view_to_src len = view chars + 1
    for (byte_idx, view_char_idx) in transform.src_to_view.iter().enumerate() {
        let view_char_idx = *view_char_idx;
        let expanded_byte = if view_char_idx < byte_map_after.len() {
            byte_map_after[view_char_idx].1
        } else {
            *byte_map_after
                .last()
                .map(|(_, b)| b)
                .unwrap_or(&expanded.len())
        };
        orig_byte_to_expanded.push((byte_idx.min(original.len()), expanded_byte));
    }

    // Char mapping: walk original chars, map to view char, then to expanded char index
    let mut view_char_indices: Vec<usize> = Vec::new();
    for (ch_idx, (byte_idx, _)) in original.char_indices().enumerate() {
        let view_idx = if byte_idx < transform.src_to_view.len() {
            transform.src_to_view[byte_idx]
        } else {
            *transform.src_to_view.last().unwrap_or(&0)
        };
        view_char_indices.push(view_idx);
        // Map to expanded char index via char_map_after
        let exp_char_idx = if view_idx < char_map_after.len() {
            char_map_after[view_idx]
        } else {
            *char_map_after.last().unwrap_or(&expanded.chars().count())
        };
        orig_char_to_expanded.push(exp_char_idx);
        let _ = ch_idx; // silence unused variable warning
    }
    // sentinel
    orig_char_to_expanded.push(*char_map_after.last().unwrap_or(&expanded.chars().count()));

    ExpandedLine {
        text: expanded,
        byte_mapping: orig_byte_to_expanded,
        control_ranges: control,
        char_mapping: orig_char_to_expanded,
    }
}

/// Expands tabs and control characters (simple version without mapping)
pub fn expand_tabs(text: &str, tab_width: usize) -> String {
    expand_tabs_with_mapping(text, tab_width).text
}

/// Remaps an original char column index through the char mapping from expand_tabs_with_mapping.
/// Returns the corresponding expanded char index.
pub fn remap_char_col(original_col: usize, char_mapping: &[usize]) -> usize {
    if char_mapping.is_empty() {
        return original_col;
    }
    if original_col < char_mapping.len() {
        char_mapping[original_col]
    } else {
        // Beyond the mapped range - use the last mapping value (sentinel)
        *char_mapping.last().unwrap()
    }
}

/// Converts a grapheme column (cursor.col()) to a display column.
///
/// Iterates graphemes directly and sums their display widths, handling tabs.
/// This is correct for multi-codepoint graphemes (ZWJ emoji, combining marks)
/// where `char_col_to_display_col` would over-count by summing per-char widths.
pub fn grapheme_col_to_display_col(text: &str, grapheme_col: usize, tab_width: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;

    let mut display_col = 0;
    for (i, grapheme) in text.graphemes(true).enumerate() {
        if i >= grapheme_col {
            break;
        }
        if grapheme == "\t" {
            let spaces = tab_width - (display_col % tab_width);
            display_col += spaces;
        } else {
            display_col += crate::display::grapheme_display_width(grapheme);
        }
    }
    display_col
}

/// Truncates text to fit within a display width, accounting for wide characters
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut result = String::new();
    let mut display_width = 0;

    for grapheme in text.graphemes(true) {
        let g_width = crate::display::grapheme_display_width(grapheme);
        if display_width + g_width > max_width {
            break;
        }
        result.push_str(grapheme);
        display_width += g_width;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tabs_basic() {
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("\thello", 4);
        assert_eq!(text, "    hello");
        assert!(control_ranges.is_empty());
    }

    #[test]
    fn test_expand_control_chars() {
        // ESC (0x1B) → ^[
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("\x1b[31m", 4);
        assert_eq!(text, "^[[31m");
        assert_eq!(control_ranges.len(), 1);
        assert_eq!(control_ranges[0], 0..2); // ^[ occupies bytes 0..2
    }

    #[test]
    fn test_expand_nul() {
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("a\x00b", 4);
        assert_eq!(text, "a^@b");
        assert_eq!(control_ranges.len(), 1);
        assert_eq!(control_ranges[0], 1..3); // ^@ at byte offset 1..3
    }

    #[test]
    fn test_expand_del() {
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("x\x7fy", 4);
        assert_eq!(text, "x^?y");
        assert_eq!(control_ranges.len(), 1);
        assert_eq!(control_ranges[0], 1..3);
    }

    #[test]
    fn test_expand_multiple_control_chars() {
        // "\x1b[31mred\x1b[0m" → "^[[31mred^[[0m"
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("\x1b[31mred\x1b[0m", 4);
        assert_eq!(text, "^[[31mred^[[0m");
        assert_eq!(control_ranges.len(), 2);
        assert_eq!(control_ranges[0], 0..2); // first ^[
        assert_eq!(control_ranges[1], 9..11); // second ^[
    }

    #[test]
    fn test_expand_no_control_chars() {
        let ExpandedLine {
            text,
            control_ranges,
            ..
        } = expand_tabs_with_mapping("hello world", 4);
        assert_eq!(text, "hello world");
        assert!(control_ranges.is_empty());
    }

    #[test]
    fn test_byte_mapping_with_control_chars() {
        // "a\x01b" → "a^Ab"
        let ExpandedLine {
            text,
            byte_mapping: mapping,
            ..
        } = expand_tabs_with_mapping("a\x01b", 4);
        assert_eq!(text, "a^Ab");
        // mapping: orig 0 → exp 0 ('a'), orig 1 → exp 1 ('\x01' → ^A), orig 2 → exp 3 ('b'), end
        assert_eq!(mapping[0], (0, 0));
        assert_eq!(mapping[1], (1, 1));
        assert_eq!(mapping[2], (2, 3));
        assert_eq!(mapping[3], (3, 4)); // end sentinel
    }

    #[test]
    fn test_char_mapping_with_tabs() {
        // "\thello" with tab_width=4 → "    hello"
        let ExpandedLine { char_mapping, .. } = expand_tabs_with_mapping("\thello", 4);
        // original char 0 (tab) → expanded char 0 (first of 4 spaces)
        assert_eq!(char_mapping[0], 0);
        // original char 1 ('h') → expanded char 4
        assert_eq!(char_mapping[1], 4);
        // original char 2 ('e') → expanded char 5
        assert_eq!(char_mapping[2], 5);
        // sentinel
        assert_eq!(char_mapping[6], 9);
    }

    #[test]
    fn test_char_mapping_with_control_chars() {
        // "a\x01b" → "a^Ab"
        let ExpandedLine { char_mapping, .. } = expand_tabs_with_mapping("a\x01b", 4);
        assert_eq!(char_mapping[0], 0); // 'a' → expanded 0
        assert_eq!(char_mapping[1], 1); // '\x01' → expanded 1 (^A takes 2 chars)
        assert_eq!(char_mapping[2], 3); // 'b' → expanded 3
        assert_eq!(char_mapping[3], 4); // sentinel
    }

    #[test]
    fn test_char_mapping_plain_text() {
        let ExpandedLine { char_mapping, .. } = expand_tabs_with_mapping("hello", 4);
        for (i, &mapped) in char_mapping.iter().enumerate().take(6) {
            assert_eq!(mapped, i); // 1:1 mapping for plain text
        }
    }

    #[test]
    fn test_remap_char_col() {
        let ExpandedLine { char_mapping, .. } = expand_tabs_with_mapping("\thello", 4);
        assert_eq!(remap_char_col(0, &char_mapping), 0);
        assert_eq!(remap_char_col(1, &char_mapping), 4);
        assert_eq!(remap_char_col(2, &char_mapping), 5);
        // Beyond range returns sentinel
        assert_eq!(remap_char_col(100, &char_mapping), 9);
    }

    #[test]
    fn test_remap_char_col_empty() {
        assert_eq!(remap_char_col(5, &[]), 5); // passthrough for empty mapping
    }

    // ================================================================
    // grapheme_col_to_display_col tests
    // ================================================================

    #[test]
    fn test_grapheme_display_col_ascii() {
        // ASCII: grapheme index == char index == display col
        assert_eq!(grapheme_col_to_display_col("hello", 0, 4), 0);
        assert_eq!(grapheme_col_to_display_col("hello", 3, 4), 3);
        assert_eq!(grapheme_col_to_display_col("hello", 5, 4), 5);
    }

    #[test]
    fn test_grapheme_display_col_zwj_emoji() {
        // "a👨‍👩‍👧‍👦b" — 3 graphemes, 8 chars (a=1, ZWJ emoji=6 chars, b=1)
        // Grapheme 0 ('a') → char 0 → display 0
        // Grapheme 1 (emoji) → char 1 → display 1 (emoji is width 2)
        // Grapheme 2 ('b') → char 7 → display 3 (after emoji width 2)
        let text = "a👨\u{200D}👩\u{200D}👧\u{200D}👦b";
        assert_eq!(grapheme_col_to_display_col(text, 0, 4), 0);
        assert_eq!(grapheme_col_to_display_col(text, 1, 4), 1);
        assert_eq!(grapheme_col_to_display_col(text, 2, 4), 3);
    }

    #[test]
    fn test_grapheme_display_col_combining_mark() {
        // "e\u{0301}x" = 'é' + 'x' — 2 graphemes, 3 chars
        // Grapheme 0 (é) → char 0 → display 0
        // Grapheme 1 (x) → char 2 → display 1 (combining mark is zero-width)
        let text = "e\u{0301}x";
        assert_eq!(grapheme_col_to_display_col(text, 0, 4), 0);
        assert_eq!(grapheme_col_to_display_col(text, 1, 4), 1);
    }

    #[test]
    fn test_grapheme_display_col_flag_emoji() {
        // "a🇺🇸b" — 3 graphemes, 4 chars (regional indicators are 2 chars)
        // Grapheme 0 ('a') → char 0 → display 0
        // Grapheme 1 (flag) → char 1 → display 1
        // Grapheme 2 ('b') → char 3 → display 3 (flag is width 2)
        let text = "a🇺🇸b";
        assert_eq!(grapheme_col_to_display_col(text, 0, 4), 0);
        assert_eq!(grapheme_col_to_display_col(text, 1, 4), 1);
        assert_eq!(grapheme_col_to_display_col(text, 2, 4), 3);
    }

    #[test]
    fn test_grapheme_display_col_with_tab() {
        // "👨‍👩‍👧‍👦\tx" — 3 graphemes (emoji, tab, x)
        // Emoji is width 2, tab at col 2 expands by 2 to reach col 4
        let text = "👨\u{200D}👩\u{200D}👧\u{200D}👦\tx";
        assert_eq!(grapheme_col_to_display_col(text, 0, 4), 0); // emoji at display 0
        assert_eq!(grapheme_col_to_display_col(text, 1, 4), 2); // tab starts at display 2
        assert_eq!(grapheme_col_to_display_col(text, 2, 4), 4); // x after tab expands to col 4
    }

    #[test]
    fn test_grapheme_display_col_cjk() {
        // "你好x" — 3 graphemes, 3 chars, but CJK chars are width 2
        assert_eq!(grapheme_col_to_display_col("你好x", 0, 4), 0);
        assert_eq!(grapheme_col_to_display_col("你好x", 1, 4), 2);
        assert_eq!(grapheme_col_to_display_col("你好x", 2, 4), 4);
    }
}
