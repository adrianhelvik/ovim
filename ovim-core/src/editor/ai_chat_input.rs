use crate::display::{display_width, grapheme_display_width};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Word,
    Punctuation,
    Whitespace,
}

fn word_class(character: char) -> WordClass {
    if character.is_whitespace() {
        WordClass::Whitespace
    } else if character.is_alphanumeric() || character == '_' {
        WordClass::Word
    } else {
        WordClass::Punctuation
    }
}

fn clamped_char_boundary(text: &str, cursor: usize) -> usize {
    let mut boundary = cursor.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

/// Find the readline-style Alt-B boundary before `cursor`: skip whitespace,
/// then traverse one word or punctuation run.
pub fn previous_chat_input_word_boundary(text: &str, cursor: usize) -> usize {
    let mut boundary = clamped_char_boundary(text, cursor);
    while boundary > 0 {
        let (previous, character) = text[..boundary].char_indices().next_back().unwrap();
        if word_class(character) != WordClass::Whitespace {
            break;
        }
        boundary = previous;
    }
    let Some((_, character)) = text[..boundary].char_indices().next_back() else {
        return boundary;
    };
    let class = word_class(character);
    while boundary > 0 {
        let (previous, character) = text[..boundary].char_indices().next_back().unwrap();
        if word_class(character) != class {
            break;
        }
        boundary = previous;
    }
    boundary
}

/// Find the readline-style Alt-F boundary after `cursor`: skip whitespace,
/// then traverse one word or punctuation run.
pub fn next_chat_input_word_boundary(text: &str, cursor: usize) -> usize {
    let mut boundary = clamped_char_boundary(text, cursor);
    while boundary < text.len() {
        let character = text[boundary..].chars().next().unwrap();
        if word_class(character) != WordClass::Whitespace {
            break;
        }
        boundary += character.len_utf8();
    }
    let Some(character) = text[boundary..].chars().next() else {
        return boundary;
    };
    let class = word_class(character);
    while boundary < text.len() {
        let character = text[boundary..].chars().next().unwrap();
        if word_class(character) != class {
            break;
        }
        boundary += character.len_utf8();
    }
    boundary
}

/// Byte range occupied by one visual row of the AI chat composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatInputRow {
    pub start: usize,
    /// Start of displayed content. Separator whitespace that falls exactly at
    /// a soft-wrap boundary remains in the source range but is visually elided.
    pub visible_start: usize,
    pub end: usize,
}

/// Wrap composer text at word boundaries while preserving byte ranges for
/// cursor movement and mouse hit-testing. A word is split only when no
/// whitespace boundary fits on the current row.
pub fn wrap_chat_input_rows(text: &str, max_width: usize, tab_width: usize) -> Vec<ChatInputRow> {
    wrap_chat_input_rows_with_widths(text, max_width, max_width, tab_width)
}

/// Word-aware composer wrapping with a distinct first-row width. Inline AI
/// prompts use this because their prefix leaves less room on the first row.
pub fn wrap_chat_input_rows_with_widths(
    text: &str,
    first_row_width: usize,
    continuation_row_width: usize,
    tab_width: usize,
) -> Vec<ChatInputRow> {
    if text.is_empty() {
        return vec![ChatInputRow {
            start: 0,
            visible_start: 0,
            end: 0,
        }];
    }

    let first_row_width = first_row_width.max(1);
    let continuation_row_width = continuation_row_width.max(1);
    let tab_width = tab_width.max(1);
    let mut rows = Vec::new();
    let mut row_start = 0usize;

    while row_start < text.len() {
        let width_limit = if rows.is_empty() {
            first_row_width
        } else {
            continuation_row_width
        };
        let soft_wrapped = row_start > 0 && text.as_bytes().get(row_start - 1) != Some(&b'\n');
        let visible_start = if soft_wrapped {
            let mut visible = row_start;
            for (relative, character) in text[row_start..].char_indices() {
                if character == '\n' || !character.is_whitespace() {
                    break;
                }
                visible = row_start + relative + character.len_utf8();
            }
            visible
        } else {
            row_start
        };
        let mut row_end = visible_start;
        let mut row_width = 0usize;
        let mut last_word_boundary = None;
        let mut newline_at = None;

        // Grapheme walk: emoji sequences (VS16/ZWJ) are one rendered cell
        // pair and must wrap atomically with their rendered width.
        for (relative, grapheme) in text[visible_start..].grapheme_indices(true) {
            let byte_index = visible_start + relative;
            if grapheme == "\n" {
                newline_at = Some(byte_index);
                row_end = byte_index;
                break;
            }

            let grapheme_width = if grapheme == "\t" {
                tab_width - (row_width % tab_width)
            } else {
                grapheme_display_width(grapheme)
            };
            let is_whitespace = grapheme.chars().all(char::is_whitespace);
            if row_end > visible_start && row_width.saturating_add(grapheme_width) > width_limit {
                if !is_whitespace {
                    if let Some(boundary) = last_word_boundary {
                        row_end = boundary;
                    }
                }
                break;
            }

            row_width = row_width.saturating_add(grapheme_width);
            row_end = byte_index + grapheme.len();
            if is_whitespace && row_end > visible_start {
                last_word_boundary = Some(row_end);
            }
        }

        if row_end == visible_start {
            match text[visible_start..].graphemes(true).next() {
                Some("\n") => {
                    rows.push(ChatInputRow {
                        start: row_start,
                        visible_start,
                        end: visible_start,
                    });
                    row_start = visible_start + 1;
                    continue;
                }
                // Force progress with a whole grapheme so a too-narrow row
                // never splits an emoji sequence mid-cluster.
                Some(grapheme) => row_end = visible_start + grapheme.len(),
                None if visible_start > row_start => row_end = visible_start,
                None => break,
            }
        }

        rows.push(ChatInputRow {
            start: row_start,
            visible_start,
            end: row_end,
        });
        row_start = if newline_at == Some(row_end) {
            row_end + 1
        } else {
            row_end
        };
    }

    if text.ends_with('\n') {
        rows.push(ChatInputRow {
            start: text.len(),
            visible_start: text.len(),
            end: text.len(),
        });
    }
    if rows.is_empty() {
        rows.push(ChatInputRow {
            start: 0,
            visible_start: 0,
            end: 0,
        });
    }
    rows
}

pub fn chat_input_cursor_row_col(
    text: &str,
    cursor_byte: usize,
    rows: &[ChatInputRow],
    tab_width: usize,
) -> (usize, usize) {
    let safe_cursor = cursor_byte.min(text.len());
    for (row_index, row) in rows.iter().enumerate() {
        let shared_with_next_row = rows
            .get(row_index + 1)
            .is_some_and(|next| next.start == safe_cursor && safe_cursor == row.end);
        if safe_cursor < row.end || (safe_cursor == row.end && !shared_with_next_row) {
            return (
                row_index,
                display_width(
                    &text[row.visible_start..safe_cursor.max(row.visible_start)],
                    tab_width,
                ),
            );
        }
    }
    let row_index = rows.len().saturating_sub(1);
    let row = rows.get(row_index).copied().unwrap_or(ChatInputRow {
        start: 0,
        visible_start: 0,
        end: 0,
    });
    (
        row_index,
        display_width(&text[row.visible_start..row.end], tab_width),
    )
}

pub fn chat_input_byte_for_display_column(
    text: &str,
    row: ChatInputRow,
    target_column: usize,
    tab_width: usize,
) -> usize {
    let mut column = 0usize;
    for (relative, grapheme) in text[row.visible_start..row.end].grapheme_indices(true) {
        let grapheme_width = if grapheme == "\t" {
            let tab_width = tab_width.max(1);
            tab_width - (column % tab_width)
        } else {
            grapheme_display_width(grapheme)
        };
        if column.saturating_add(grapheme_width) > target_column {
            return row.visible_start + relative;
        }
        column = column.saturating_add(grapheme_width);
    }
    row.end
}

pub fn chat_input_visible_start(
    total_rows: usize,
    cursor_row: usize,
    visible_rows: usize,
) -> usize {
    if visible_rows == 0 || total_rows <= visible_rows {
        return 0;
    }
    cursor_row
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(total_rows - visible_rows)
}

pub fn move_chat_input_cursor_vertical(
    text: &str,
    cursor_byte: usize,
    rows: &[ChatInputRow],
    direction: i8,
    tab_width: usize,
) -> Option<usize> {
    let (current_row, column) = chat_input_cursor_row_col(text, cursor_byte, rows, tab_width);
    let target_row = if direction < 0 {
        current_row.checked_sub(1)?
    } else {
        let next = current_row.saturating_add(1);
        (next < rows.len()).then_some(next)?
    };
    let mut target = chat_input_byte_for_display_column(text, rows[target_row], column, tab_width);
    // A byte offset shared by two soft-wrapped rows is rendered at the start
    // of the continuation row. When vertical movement targets the visual end
    // of the previous row, keep it on that row's final character instead of
    // appearing not to move at all.
    if target == rows[target_row].end
        && rows
            .get(target_row + 1)
            .is_some_and(|next| next.start == target)
        && target > rows[target_row].visible_start
    {
        target = text[..target]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(target);
    }
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[ChatInputRow], input: &str) -> Vec<String> {
        rows.iter()
            .map(|row| input[row.start..row.end].to_string())
            .collect()
    }

    #[test]
    fn wraps_at_words_before_splitting_them() {
        let input = "alpha beta gamma";
        let rows = wrap_chat_input_rows(input, 11, 4);
        assert_eq!(text(&rows, input), vec!["alpha beta ", "gamma"]);
    }

    #[test]
    fn varying_widths_move_whole_word_to_continuation_row() {
        let input = "example message that fits";
        let rows = wrap_chat_input_rows_with_widths(input, 12, 20, 4);
        assert_eq!(text(&rows, input), vec!["example ", "message that fits"]);
    }

    #[test]
    fn word_boundaries_handle_whitespace_punctuation_and_unicode() {
        let input = "alpha  beta.世界";
        assert_eq!(previous_chat_input_word_boundary(input, input.len()), 12);
        assert_eq!(previous_chat_input_word_boundary(input, 12), 11);
        assert_eq!(previous_chat_input_word_boundary(input, 11), 7);
        assert_eq!(next_chat_input_word_boundary(input, 0), 5);
        assert_eq!(next_chat_input_word_boundary(input, 5), 11);
        assert_eq!(next_chat_input_word_boundary(input, 11), 12);
        assert_eq!(next_chat_input_word_boundary(input, 12), input.len());
    }

    #[test]
    fn splits_a_word_longer_than_the_row() {
        let input = "extraordinary";
        let rows = wrap_chat_input_rows(input, 5, 4);
        assert_eq!(text(&rows, input), vec!["extra", "ordin", "ary"]);
    }

    #[test]
    fn preserves_explicit_newlines_and_trailing_spaces() {
        let input = "abc \ndef";
        let rows = wrap_chat_input_rows(input, 20, 4);
        assert_eq!(text(&rows, input), vec!["abc ", "def"]);
    }

    #[test]
    fn soft_wrapped_separator_before_newline_uses_one_empty_row() {
        let input = "hello \nworld";
        let rows = wrap_chat_input_rows(input, 5, 4);

        assert_eq!(text(&rows, input), vec!["hello", " ", "world"]);
        assert_eq!(&input[rows[1].visible_start..rows[1].end], "");
    }

    #[test]
    fn unicode_separator_before_newline_preserves_utf8_boundaries() {
        let input = "hello\u{2003}\nworld";
        let rows = wrap_chat_input_rows(input, 5, 4);

        assert_eq!(text(&rows, input), vec!["hello", "\u{2003}", "world"]);
        assert!(rows.iter().all(|row| {
            input.is_char_boundary(row.start)
                && input.is_char_boundary(row.visible_start)
                && input.is_char_boundary(row.end)
        }));
    }

    #[test]
    fn cursor_viewport_follows_rows_beyond_the_height_cap() {
        assert_eq!(chat_input_visible_start(9, 8, 5), 4);
        assert_eq!(chat_input_visible_start(9, 2, 5), 0);
    }

    #[test]
    fn display_column_maps_back_to_utf8_byte_offset() {
        let input = "a界b";
        let row = ChatInputRow {
            start: 0,
            visible_start: 0,
            end: input.len(),
        };
        assert_eq!(chat_input_byte_for_display_column(input, row, 1, 4), 1);
        assert_eq!(chat_input_byte_for_display_column(input, row, 3, 4), 4);
    }

    #[test]
    fn elides_only_separator_space_at_an_exact_soft_wrap() {
        let input = "hello world";
        let rows = wrap_chat_input_rows(input, 5, 4);
        assert_eq!(text(&rows, input), vec!["hello", " world"]);
        assert_eq!(&input[rows[1].visible_start..rows[1].end], "world");
    }

    #[test]
    fn vertical_movement_uses_soft_wrapped_rows() {
        let input = "alpha beta gamma";
        let rows = wrap_chat_input_rows(input, 7, 4);
        let moved = move_chat_input_cursor_vertical(input, input.len(), &rows, -1, 4).unwrap();
        let (row, _) = chat_input_cursor_row_col(input, moved, &rows, 4);
        assert_eq!(row, rows.len() - 2);
    }

    #[test]
    fn trailing_separator_keeps_an_empty_cursor_row() {
        let input = "hello ";
        let rows = wrap_chat_input_rows(input, 5, 4);
        let (row, column) = chat_input_cursor_row_col(input, input.len(), &rows, 4);
        assert_eq!(row, 1);
        assert_eq!(column, 0);
    }

    #[test]
    fn cursor_at_soft_wrap_boundary_belongs_to_the_continuation_row() {
        let input = "alpha beta gamma";
        let rows = wrap_chat_input_rows(input, 6, 4);
        assert_eq!(&input[rows[0].start..rows[0].end], "alpha ");

        let (row, column) = chat_input_cursor_row_col(input, rows[0].end, &rows, 4);

        assert_eq!(row, 1);
        assert_eq!(column, 0);
    }

    #[test]
    fn capped_viewport_follows_cursor_across_soft_wrap_boundaries() {
        let input = "one two three four five six seven eight nine ten";
        let rows = wrap_chat_input_rows(input, 6, 4);
        let cursor = rows[6].start;
        let (cursor_row, _) = chat_input_cursor_row_col(input, cursor, &rows, 4);

        assert_eq!(cursor_row, 6);
        assert_eq!(chat_input_visible_start(rows.len(), cursor_row, 5), 2);
    }
}
