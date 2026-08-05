//! Character motion handler for f/t/F/T and other awaiting-char commands.
//!
//! This module handles the AwaitingChar input state, processing the target
//! character for find/till motions. It also handles operator combinations
//! like df, dt, cf, ct, plus r/m/'/` commands.

use crate::{KeyCode, KeyEvent};
use anyhow::Result;

use crate::editor::editing_state::PendingChangeRepeat;
use crate::editor::input_state::CharMotion;
use crate::editor::operators::Operator;
use crate::editor::CursorPos;
use crate::editor::Editor;
use crate::editor::RegisterType;
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::GraphemeCol;

/// Handles the second key in a character motion sequence.
///
/// Called when the editor is in `InputState::AwaitingChar` state.
/// The `motion` indicates what type of motion (f/t/F/T),
/// and `operator` indicates if there's a pending operator (for df/dt/cf/ct).
pub fn handle_char_motion(
    editor: &mut Editor,
    key: KeyEvent,
    motion: CharMotion,
    operator: Option<Operator>,
) -> Result<()> {
    // Handle Escape - cancel the motion
    if key.code == KeyCode::Esc {
        editor.reset_input_state();
        editor.clear_pending_operator();
        editor.clear_count();
        return Ok(());
    }

    // We need a character to proceed
    let KeyCode::Char(target) = key.code else {
        // Non-character key - cancel
        editor.reset_input_state();
        return Ok(());
    };

    let count = editor.effective_count();

    match motion {
        CharMotion::Find | CharMotion::Till | CharMotion::FindBack | CharMotion::TillBack => {
            handle_char_find(editor, motion, target, count, operator);
        }
        CharMotion::Replace => handle_replace_char(editor, target, count)?,
        CharMotion::Mark => handle_set_mark(editor, target),
        CharMotion::JumpMarkLine => {
            if operator.is_some() {
                handle_mark_operator(editor, target, operator, true);
            } else {
                handle_jump_mark_line(editor, target);
            }
        }
        CharMotion::JumpMarkExact => {
            if operator.is_some() {
                handle_mark_operator(editor, target, operator, false);
            } else {
                handle_jump_mark_exact(editor, target);
            }
        }
    }

    // Clear state
    editor.reset_input_state();
    editor.clear_pending_operator();
    editor.clear_count();

    Ok(())
}

fn handle_replace_char(editor: &mut Editor, target: char, count: usize) -> Result<()> {
    if editor.mode().is_visual() {
        crate::editor::input::helpers::replace_visual_selection(editor, target)?;
        crate::editor::input::helpers::exit_visual_mode_to_normal(editor);
        return Ok(());
    }

    editor.record_operation(
        |buf| buf.replace_chars_at_cursor(target, count),
        Some(RepeatAction::ReplaceChar { ch: target, count }),
    );
    Ok(())
}

fn handle_set_mark(editor: &mut Editor, target: char) {
    if target.is_ascii_lowercase() || target.is_ascii_uppercase() {
        editor.set_mark(target);
    }
}

/// Resolves a mark character to a `(line, col)` position, if set.
fn resolve_mark(editor: &Editor, target: char) -> Option<(usize, usize)> {
    if target.is_ascii_lowercase() {
        editor.nav.marks.get_mark(target).map(|m| (m.line, m.col))
    } else {
        // Only local marks supported with operators for now.
        // Global marks (A-Z) involve file switching.
        None
    }
}

/// Handle mark motions with a pending operator (d'a, d`a, y'a, y`a, c'a, c`a).
///
/// `line_wise` = true for `'` (line-wise), false for `` ` `` (character-wise).
fn handle_mark_operator(
    editor: &mut Editor,
    target: char,
    operator: Option<Operator>,
    line_wise: bool,
) {
    let Some(op) = operator else {
        return;
    };
    let Some((mark_line, mark_col)) = resolve_mark(editor, target) else {
        return;
    };

    let cursor_line = editor.buffer().cursor().line();
    let cursor_col = editor.buffer().cursor().col().0;
    let cursor_before = CursorPos::new(cursor_line, GraphemeCol(cursor_col));

    if line_wise {
        let start_line = cursor_line.min(mark_line);
        let end_line = cursor_line.max(mark_line);
        apply_linewise_operator(editor, op, start_line, end_line);
    } else {
        // Order start/end so start <= end
        let (start, end) =
            if cursor_line < mark_line || (cursor_line == mark_line && cursor_col <= mark_col) {
                ((cursor_line, cursor_col), (mark_line, mark_col))
            } else {
                ((mark_line, mark_col), (cursor_line, cursor_col))
            };

        // Vim treats backtick mark motions as exclusive
        let range = OperatorRange::exclusive(start, end);
        apply_charwise_operator(editor, op, cursor_before, range, None);
    }
}

fn handle_jump_mark_line(editor: &mut Editor, target: char) {
    if target == '\'' {
        editor.jump_back();
        return;
    }

    if !(target.is_ascii_lowercase() || target.is_ascii_uppercase() || matches!(target, '.' | '^'))
    {
        return;
    }

    editor.add_jump();
    let _ = editor.jump_to_mark_line(target);
}

fn handle_jump_mark_exact(editor: &mut Editor, target: char) {
    if target == '`' {
        editor.jump_back();
        return;
    }

    if !(target.is_ascii_lowercase()
        || target.is_ascii_uppercase()
        || matches!(target, '.' | '^' | '[' | ']'))
    {
        return;
    }

    editor.add_jump();
    let _ = editor.jump_to_mark(target);
}

// ---------------------------------------------------------------------------
// Unified f/t/F/T handler
// ---------------------------------------------------------------------------

/// Handles all four character find motions (f/t/F/T) and their operator
/// combinations (df/dt/cf/ct/yf/yt etc.).
fn handle_char_find(
    editor: &mut Editor,
    motion: CharMotion,
    target: char,
    count: usize,
    operator: Option<Operator>,
) {
    let start = editor.cursor_position();

    let moved = motion.execute(editor.buffer_mut(), target, count);
    if !moved {
        return;
    }

    // Store for ; and , repeat
    editor.set_last_find(target, motion.find_type(), motion.direction());

    let Some(op) = operator else {
        return;
    };

    let end = editor.cursor_position();

    // For backward motions the cursor moved before start, so swap the range.
    // Backward F/T are EXCLUSIVE in vim: the original cursor character is
    // not part of the operated range (dFx on `axbc` with cursor on `c`
    // leaves `ac`, not `a`) — OV-00288.
    let range = if motion.is_backward() {
        OperatorRange::exclusive((end.line, end.col.0), (start.line, start.col.0))
    } else {
        OperatorRange::inclusive(start, end)
    };

    let repeat = RepeatAction::DeleteCharMotion {
        target,
        forward: motion.is_forward(),
        till: motion.is_till(),
        count,
    };

    apply_charwise_operator(editor, op, start, range, Some(repeat));
}

// ---------------------------------------------------------------------------
// Operator application
// ---------------------------------------------------------------------------

/// A character-wise text range for operator application.
/// Stored as grapheme-space `(line, col)` pairs — the delete_range conversion
/// (treating these as char indices) is pre-existing Class-2 debt.
struct OperatorRange {
    start: (usize, usize),
    /// The end column, always stored as exclusive (one past last char to affect).
    end_col_exclusive: (usize, usize),
}

impl OperatorRange {
    /// Inclusive range — the end position is the last character to include.
    /// Used by f/t motions where the cursor lands *on* the target.
    fn inclusive(start: CursorPos, end: CursorPos) -> Self {
        Self {
            start: (start.line, start.col.0),
            end_col_exclusive: (end.line, end.col.0.saturating_add(1)),
        }
    }

    /// Exclusive range — the end position is already one past the last character.
    /// Used by backtick mark motions. Accepts grapheme-space tuples directly
    /// because the backtick-mark caller still works in raw usize cols.
    fn exclusive(start: (usize, usize), end: (usize, usize)) -> Self {
        Self {
            start,
            end_col_exclusive: end,
        }
    }
}

/// Applies a character-wise operator (delete/change/yank) to a range.
///
/// `repeat` controls the `.` repeat action: `Some(action)` sets it on
/// delete, or wraps it in `PendingChangeRepeat` on change. `None` skips
/// repeat registration (used by mark operators).
fn apply_charwise_operator(
    editor: &mut Editor,
    operator: Operator,
    cursor_before: CursorPos,
    range: OperatorRange,
    repeat: Option<RepeatAction>,
) {
    let (start_line, start_col) = range.start;
    let (end_line, end_col_raw) = range.end_col_exclusive;

    // Clamp end_col to the line length to avoid overflow/past-EOL issues.
    let end_col = if let Some(line) = editor.buffer().line_text(end_line) {
        let line_len = line.chars().count();
        end_col_raw.min(line_len)
    } else {
        end_col_raw
    };

    match operator {
        Operator::Delete => {
            let (deleted, edits) = editor.buffer_mut().record(|buf| {
                // Phase-15 debt: range tuples store grapheme cols; treat as char.
                let d = buf.delete_range(
                    start_line,
                    crate::unicode::CharCol(start_col),
                    end_line,
                    crate::unicode::CharCol(end_col),
                );
                buf.cursor_mut()
                    .set_position(start_line, GraphemeCol(start_col));
                d
            });
            let cursor_after = editor.cursor_position();

            if !edits.is_empty() {
                if !deleted.is_empty() {
                    editor.delete_to_register_with_type(deleted, RegisterType::Character);
                }
                editor.push_recorded_undo(edits, cursor_before, cursor_after);
                if let Some(action) = repeat {
                    editor.set_repeat_action(action);
                }
            }
        }
        Operator::Change => {
            let (deleted, edits) = editor.buffer_mut().record(|buf| {
                // Phase-15 debt: range tuples store grapheme cols; treat as char.
                let d = buf.delete_range(
                    start_line,
                    crate::unicode::CharCol(start_col),
                    end_line,
                    crate::unicode::CharCol(end_col),
                );
                buf.cursor_mut()
                    .set_position(start_line, GraphemeCol(start_col));
                d
            });
            let delete_token = if !edits.is_empty() {
                let cursor_after = editor.cursor_position();
                let token = editor.push_recorded_undo(edits, cursor_before, cursor_after);
                if !deleted.is_empty() {
                    editor.delete_to_register_with_type(deleted, RegisterType::Character);
                }
                Some(token)
            } else {
                None
            };

            let delete_action = repeat.unwrap_or(RepeatAction::DeleteCharMotion {
                target: '\0',
                forward: true,
                till: false,
                count: 1,
            });
            editor.set_pending_change_repeat(PendingChangeRepeat {
                delete_action,
                linewise: false,
                delete_token,
            });
            editor.start_change_building(editor.cursor_position());
            editor.set_mode(Mode::Insert);
        }
        Operator::Yank => {
            let start_char = editor.buffer().rope().line_to_char(start_line) + start_col;
            let end_char = editor.buffer().rope().line_to_char(end_line) + end_col;
            if end_char > start_char {
                let yanked = editor
                    .buffer()
                    .rope()
                    .slice(start_char..end_char)
                    .to_string();
                editor.yank_to_register_with_type(yanked, RegisterType::Character);
                editor.set_yank_flash_range(
                    start_line,
                    GraphemeCol(start_col),
                    end_line,
                    GraphemeCol(end_col.saturating_sub(1)),
                );
            }
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(start_line, GraphemeCol(start_col));
        }
        _ => {}
    }
}

/// Applies a line-wise operator (delete/change/yank) to a range of lines.
fn apply_linewise_operator(
    editor: &mut Editor,
    operator: Operator,
    start_line: usize,
    end_line: usize,
) {
    let line_count = end_line - start_line + 1;

    match operator {
        Operator::Delete => {
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(start_line, GraphemeCol(0));
            let deleted = editor.record_operation(|buf| buf.delete_lines(line_count), None);
            if !deleted.is_empty() {
                editor.delete_to_register_with_type(deleted, RegisterType::Line);
            }
        }
        Operator::Yank => {
            let mut yanked = String::new();
            for line_idx in start_line..=end_line {
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    // line_text strips terminators — re-add them or the
                    // register holds one glued line (OV-00292).
                    yanked.push_str(&line);
                    yanked.push('\n');
                }
            }
            editor.yank_to_register_with_type(yanked, RegisterType::Line);
            editor.set_yank_flash_lines(start_line, end_line);
        }
        Operator::Change => {
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(start_line, GraphemeCol(0));

            let indent = editor
                .buffer()
                .line_text(start_line)
                .map(|l| {
                    l.chars()
                        .take_while(|c| c.is_whitespace() && *c != '\n')
                        .collect::<String>()
                })
                .unwrap_or_default();

            let deleted = editor.record_operation(
                |buf| {
                    let d = buf.delete_lines(line_count);
                    buf.insert_text_at(
                        start_line,
                        crate::unicode::CharCol::ZERO,
                        &format!("{}\n", indent),
                    );
                    buf.cursor_mut()
                        .set_position(start_line, GraphemeCol(indent.len()));
                    d
                },
                None,
            );
            if !deleted.is_empty() {
                editor.delete_to_register_with_type(deleted, RegisterType::Line);
            }
            editor.start_change_building(editor.cursor_position());
            editor.set_mode(Mode::Insert);
        }
        _ => {}
    }
}
