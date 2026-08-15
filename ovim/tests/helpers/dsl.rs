use crate::helpers::EditorTest;
use ovim::mode::Mode;

#[derive(Debug, Clone)]
pub struct Fixture {
    pub mode: Mode,
    pub content: String,
    pub cursor: (usize, usize),
    pub visual_start: Option<(usize, usize)>,
    pub expected_visual_selection: Option<((usize, usize), (usize, usize))>,
}

pub fn run_editor_test_case(given: Fixture, keys: &str, expect: Fixture) {
    let mut test = build_editor_from_fixture(&given);
    test.keys(keys);
    assert_editor_matches_fixture(&test, &expect);
}

#[allow(
    dead_code,
    reason = "Used by editor_test! multi-step fixtures in specific test targets."
)]
pub fn run_editor_test_steps(given: Fixture, steps: Vec<(&'static str, Fixture)>) {
    let mut test = build_editor_from_fixture(&given);
    for (keys, expect) in steps {
        test.keys(keys);
        assert_editor_matches_fixture(&test, &expect);
    }
}

pub fn fixture_from_pairs(mode: Mode, pairs: &[&str]) -> Fixture {
    parse_pairs_fixture(mode, pairs)
}

fn build_editor_from_fixture(fixture: &Fixture) -> EditorTest {
    let mut test = EditorTest::new(&fixture.content);
    test.set_cursor(fixture.cursor.0, fixture.cursor.1);
    test.editor.set_mode(fixture.mode);
    if let Some((line, col)) = fixture.visual_start {
        test.editor.set_visual_start(line, col);
    }
    test
}

fn assert_editor_matches_fixture(test: &EditorTest, expect: &Fixture) {
    test.assert_mode(expect.mode);
    test.assert_cursor(expect.cursor.0, expect.cursor.1);

    let actual_buffer = test.buffer_content();
    let expected_buffer = normalize_expected_buffer(&expect.content);
    assert_eq!(actual_buffer, expected_buffer, "Buffer content mismatch");

    if matches!(
        expect.mode,
        Mode::Visual | Mode::VisualLine | Mode::VisualBlock
    ) {
        assert_eq!(
            test.get_visual_selection(),
            expect.expected_visual_selection,
            "Visual selection mismatch"
        );
    }
}

fn normalize_expected_buffer(content: &str) -> String {
    let mut s = content.to_string();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Expand annotation string to match content length.
///
/// Rules:
/// - If annotation is shorter than content, pad with spaces
/// - If annotation ends with '-' AND has length > 1, extend selection to end of line
/// - Empty annotation "" becomes all spaces
/// - Single character annotations are padded with spaces (no extension)
///
/// Examples:
///   expand_annotation("^", 6)      -> "^     "   (cursor at 0, pad spaces)
///   expand_annotation("-", 6)      -> "-     "   (selected at 0 only, pad spaces)
///   expand_annotation("^-", 6)     -> "^-----"   (cursor at 0, selected 1-5)
///   expand_annotation("  ^", 6)    -> "  ^   "   (cursor at 2, pad spaces)
///   expand_annotation("", 6)       -> "      "   (all spaces)
///   expand_annotation(" -", 6)     -> " -----"   (selected 1-5)
///   expand_annotation("--", 6)     -> "------"   (selected 0-5)
fn expand_annotation(ann: &str, content_len: usize) -> String {
    if content_len == 0 {
        return String::new();
    }

    let ann_chars: Vec<char> = ann.chars().collect();
    let ann_len = ann_chars.len();

    if ann_len == 0 {
        // Empty annotation -> all spaces
        return " ".repeat(content_len);
    }

    if ann_len >= content_len {
        // Already long enough, return as-is
        return ann.to_string();
    }

    // Only extend with '-' if:
    // 1. Annotation ends with '-'
    // 2. Annotation has more than one character (single '-' just means col 0 selected)
    let last_char = ann_chars[ann_len - 1];
    let should_extend = last_char == '-' && ann_len > 1;
    let fill_char = if should_extend { '-' } else { ' ' };

    let mut result = ann.to_string();
    let padding_needed = content_len - ann_len;
    for _ in 0..padding_needed {
        result.push(fill_char);
    }
    result
}

fn parse_pairs_fixture(mode: Mode, pairs: &[&str]) -> Fixture {
    assert!(
        pairs.len().is_multiple_of(2),
        "Fixture must have even number of strings (text + annotation per line). \
         Each content line needs a corresponding annotation line (can be empty string \"\")."
    );

    let mut content_lines: Vec<String> = Vec::new();
    let mut cursor: Option<(usize, usize)> = None;
    let mut anchor: Option<(usize, usize)> = None;

    // Track selection markers for Visual (charwise) and VisualLine (linewise).
    let mut selected_cells: Vec<(usize, usize)> = Vec::new();
    let mut selected_lines: Vec<usize> = Vec::new();

    for line_idx in (0..pairs.len()).step_by(2).enumerate() {
        let (logical_line_idx, pair_idx) = line_idx;
        let text = pairs[pair_idx];
        let ann_raw = pairs[pair_idx + 1];
        content_lines.push(text.to_string());

        // Pad annotation with spaces if shorter than content.
        // If annotation ends with '-', extend selection to end of line.
        let text_len = text.chars().count();
        let ann = expand_annotation(ann_raw, text_len);

        // Validate annotation doesn't exceed content length
        let ann_len = ann.chars().count();
        if ann_len > text_len && text_len > 0 {
            panic!(
                "Annotation is longer than content on line {}:\n  content ({}): {:?}\n  annotation ({}): {:?}\n\
                 Hint: annotation markers should not exceed content length.",
                logical_line_idx, text_len, text, ann_len, ann
            );
        }

        let mut caret_cols: Vec<usize> = Vec::new();
        let mut anchor_cols: Vec<usize> = Vec::new();
        for (col, ch) in ann.chars().enumerate() {
            match ch {
                '^' => caret_cols.push(col),
                '@' => anchor_cols.push(col),
                '-' => selected_cells.push((logical_line_idx, col)),
                ' ' => {}
                other => panic!(
                    "Invalid annotation character '{}' on line {}. \
                     Valid markers: '^' (cursor), '@' (anchor), '-' (selected), ' ' (nothing).",
                    other, logical_line_idx
                ),
            }
        }

        if caret_cols.len() > 1 {
            panic!(
                "Multiple '^' carets on line {}. Each line can have at most one caret.",
                logical_line_idx
            );
        }
        if let Some(caret_col) = caret_cols.into_iter().next() {
            if cursor.is_some() {
                panic!(
                    "Multiple '^' carets in fixture (second one on line {}). \
                     Fixture must contain exactly one '^' caret total.",
                    logical_line_idx
                );
            }
            cursor = Some((logical_line_idx, caret_col));
            if mode.is_visual() {
                selected_cells.push((logical_line_idx, caret_col));
                selected_lines.push(logical_line_idx);
            }
        }

        if anchor_cols.len() > 1 {
            panic!(
                "Multiple '@' anchors on line {}. Each line can have at most one anchor.",
                logical_line_idx
            );
        }
        if let Some(anchor_col) = anchor_cols.into_iter().next() {
            if anchor.is_some() {
                panic!(
                    "Multiple '@' anchors in fixture (second one on line {}). \
                     Fixture must contain at most one '@' anchor total.",
                    logical_line_idx
                );
            }
            anchor = Some((logical_line_idx, anchor_col));
            if mode.is_visual() {
                selected_cells.push((logical_line_idx, anchor_col));
                selected_lines.push(logical_line_idx);
            }
        }

        if !mode.is_visual() && ann.chars().any(|c| c == '-' || c == '@') {
            panic!("Non-visual fixture cannot contain selection markers ('-' or '@').");
        }

        if matches!(mode, Mode::VisualLine) {
            if ann.chars().any(|c| c != ' ') {
                selected_lines.push(logical_line_idx);
            }
        }
    }

    let content = content_lines.join("\n");
    let cursor = cursor.unwrap_or_else(|| {
        panic!(
            "Fixture must contain exactly one '^' caret. None found.\n\
             Hint: add '^' to an annotation line to mark cursor position."
        )
    });

    match mode {
        Mode::Normal
        | Mode::Insert
        | Mode::Replace
        | Mode::Command
        | Mode::Search
        | Mode::Picker
        | Mode::HoverPreview
        | Mode::HoverNavigate
        | Mode::FileTree
        | Mode::SubstituteConfirm
        | Mode::Dashboard
        | Mode::LspManager
        | Mode::RenameInput
        | Mode::AiChat => Fixture {
            mode,
            content,
            cursor,
            visual_start: None,
            expected_visual_selection: None,
        },
        Mode::Visual => {
            let (visual_start, expected_visual_selection) =
                derive_visual_charwise(cursor, anchor, &selected_cells);
            Fixture {
                mode,
                content,
                cursor,
                visual_start: Some(visual_start),
                expected_visual_selection: Some(expected_visual_selection),
            }
        }
        Mode::VisualLine => {
            let (visual_start, expected_visual_selection) =
                derive_visual_linewise(&content_lines, cursor, anchor, &selected_lines);
            Fixture {
                mode,
                content,
                cursor,
                visual_start: Some(visual_start),
                expected_visual_selection: Some(expected_visual_selection),
            }
        }
        Mode::VisualBlock => {
            let (visual_start, expected_visual_selection) =
                derive_visual_block(cursor, anchor, &selected_cells);
            Fixture {
                mode,
                content,
                cursor,
                visual_start: Some(visual_start),
                expected_visual_selection: Some(expected_visual_selection),
            }
        }
    }
}

/// (visual_start, (selection_start, selection_end))
type DerivedVisual = ((usize, usize), ((usize, usize), (usize, usize)));

fn derive_visual_charwise(
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    selected_cells: &[(usize, usize)],
) -> DerivedVisual {
    let (start, end) = if !selected_cells.is_empty() {
        let mut sorted = selected_cells.to_vec();
        sorted.sort_unstable();
        (*sorted.first().unwrap(), *sorted.last().unwrap())
    } else if let Some(anchor) = anchor {
        if anchor.0 < cursor.0 || (anchor.0 == cursor.0 && anchor.1 <= cursor.1) {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        }
    } else {
        (cursor, cursor)
    };

    let visual_start = if let Some(anchor) = anchor {
        anchor
    } else if cursor == start {
        end
    } else if cursor == end {
        start
    } else {
        panic!("cursor must be at one end of the selection span");
    };

    (visual_start, (start, end))
}

fn derive_visual_linewise(
    content_lines: &[String],
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    selected_lines: &[usize],
) -> DerivedVisual {
    let mut lines = selected_lines.to_vec();
    lines.sort_unstable();
    lines.dedup();

    let start_line = *lines.first().unwrap_or(&cursor.0);
    let end_line = *lines.last().unwrap_or(&cursor.0);

    let visual_start_line = if let Some(anchor) = anchor {
        anchor.0
    } else if cursor.0 == start_line {
        end_line
    } else if cursor.0 == end_line {
        start_line
    } else if start_line == end_line {
        cursor.0
    } else {
        panic!("cursor must be at one end of the linewise selection span");
    };

    let end_col = content_lines
        .get(end_line)
        .map(|l| l.chars().count().saturating_sub(1))
        .unwrap_or(0);

    let expected_start = (start_line, 0);
    let expected_end = (end_line, end_col);

    ((visual_start_line, 0), (expected_start, expected_end))
}

fn derive_visual_block(
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    selected_cells: &[(usize, usize)],
) -> DerivedVisual {
    let (min_line, max_line, min_col, max_col) = if !selected_cells.is_empty() {
        let mut min_line = usize::MAX;
        let mut max_line = 0;
        let mut min_col = usize::MAX;
        let mut max_col = 0;
        for (line, col) in selected_cells {
            min_line = min_line.min(*line);
            max_line = max_line.max(*line);
            min_col = min_col.min(*col);
            max_col = max_col.max(*col);
        }
        (min_line, max_line, min_col, max_col)
    } else if let Some(anchor) = anchor {
        (
            cursor.0.min(anchor.0),
            cursor.0.max(anchor.0),
            cursor.1.min(anchor.1),
            cursor.1.max(anchor.1),
        )
    } else {
        panic!("VisualBlock fixture must provide '@' anchor or '-' selection markers");
    };

    let expected_selection = ((min_line, min_col), (max_line, max_col));

    let visual_start = if let Some(anchor) = anchor {
        anchor
    } else {
        let tl = (min_line, min_col);
        let tr = (min_line, max_col);
        let bl = (max_line, min_col);
        let br = (max_line, max_col);

        if cursor == tl {
            br
        } else if cursor == br {
            tl
        } else if cursor == tr {
            bl
        } else if cursor == bl {
            tr
        } else {
            panic!("cursor must be at a rectangle corner for VisualBlock fixtures");
        }
    };

    (visual_start, expected_selection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_visual_charwise_single_line_with_selection_markers() {
        let fixture = fixture_from_pairs(Mode::Visual, &["hello", "^--- "]);
        assert_eq!(fixture.cursor, (0, 0));
        assert_eq!(fixture.visual_start, Some((0, 3)));
        assert_eq!(fixture.expected_visual_selection, Some(((0, 0), (0, 3))));
    }

    #[test]
    fn parse_visual_charwise_with_explicit_anchor() {
        let fixture = fixture_from_pairs(Mode::Visual, &["hello", "@-^  "]);
        assert_eq!(fixture.cursor, (0, 2));
        assert_eq!(fixture.visual_start, Some((0, 0)));
        assert_eq!(fixture.expected_visual_selection, Some(((0, 0), (0, 2))));
    }

    #[test]
    fn parse_visual_linewise_selection() {
        let fixture = fixture_from_pairs(
            Mode::VisualLine,
            &["line1", "^", "line2", "-", "line3", "-", "line4", ""],
        );
        assert_eq!(fixture.cursor, (0, 0));
        assert_eq!(fixture.visual_start, Some((2, 0)));
        assert_eq!(fixture.expected_visual_selection, Some(((0, 0), (2, 4))));
    }

    #[test]
    #[should_panic(expected = "cursor must be at one end")]
    fn parse_visual_linewise_requires_cursor_at_edge_of_selection() {
        let _fixture = fixture_from_pairs(
            Mode::VisualLine,
            &["line1", "-", "line2", "^", "line3", "-"],
        );
    }

    #[test]
    fn parse_visual_block_from_marked_rectangle() {
        let fixture = fixture_from_pairs(Mode::VisualBlock, &["abcd", " ^- ", "efgh", " -- "]);
        assert_eq!(fixture.cursor, (0, 1));
        assert_eq!(fixture.expected_visual_selection, Some(((0, 1), (1, 2))));
        assert_eq!(fixture.visual_start, Some((1, 2)));
    }

    #[test]
    fn parse_visual_block_from_explicit_anchor() {
        let fixture = fixture_from_pairs(
            Mode::VisualBlock,
            &["abcd", " ^  ", "efgh", "    ", "ijkl", "   @"],
        );
        assert_eq!(fixture.cursor, (0, 1));
        assert_eq!(fixture.visual_start, Some((2, 3)));
        assert_eq!(fixture.expected_visual_selection, Some(((0, 1), (2, 3))));
    }

    #[test]
    #[should_panic(expected = "Non-visual fixture cannot contain selection markers")]
    fn parse_normal_disallows_selection_markers() {
        let _fixture = fixture_from_pairs(Mode::Normal, &["hello", "^-"]);
    }

    #[test]
    fn test_expand_annotation_empty() {
        assert_eq!(expand_annotation("", 6), "      ");
        assert_eq!(expand_annotation("", 0), "");
    }

    #[test]
    fn test_expand_annotation_single_caret() {
        // Single ^ pads with spaces (cursor at col 0)
        assert_eq!(expand_annotation("^", 6), "^     ");
    }

    #[test]
    fn test_expand_annotation_single_dash() {
        // Single - does NOT extend (selected at col 0 only)
        assert_eq!(expand_annotation("-", 6), "-     ");
    }

    #[test]
    fn test_expand_annotation_caret_dash_extends() {
        // ^- extends selection to end of line
        assert_eq!(expand_annotation("^-", 6), "^-----");
    }

    #[test]
    fn test_expand_annotation_space_dash_extends() {
        // " -" extends selection from col 1 to end
        assert_eq!(expand_annotation(" -", 6), " -----");
    }

    #[test]
    fn test_expand_annotation_double_dash_extends() {
        // "--" extends to end
        assert_eq!(expand_annotation("--", 6), "------");
    }

    #[test]
    fn test_expand_annotation_caret_in_middle() {
        // Caret in middle, pad with spaces
        assert_eq!(expand_annotation("  ^", 6), "  ^   ");
    }

    #[test]
    fn test_expand_annotation_already_full_length() {
        // Already full length, no change
        assert_eq!(expand_annotation("^-----", 6), "^-----");
        assert_eq!(expand_annotation("------", 6), "------");
    }

    #[test]
    fn test_expand_annotation_longer_than_content() {
        // Longer annotations are returned as-is (validation happens elsewhere)
        assert_eq!(expand_annotation("^-------", 6), "^-------");
    }
}
