//! Visual mode handler
//!
//! Handles all input events in Visual, VisualLine, and VisualBlock modes including:
//! - Visual mode motions (h/j/k/l, w/b/e, etc.)
//! - Visual mode operators (d, c, y, >, <, ~, u, U)
//! - Visual mode text objects (iw, aw, i", a{, etc.)
//! - Visual block operations (I, A, c, r)
//! - Visual mode commands (o to swap cursor, gv to reselect)
//! - Visual mode search (/ and ?)

use crate::editor::{
    CursorPos, Editor, Motions, PendingChangeRepeat, RegisterType, TextObjectRange, TextObjects,
};
use crate::indentation::leading_char_count;
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::{CharCol, GraphemeCol};
use crate::{KeyCode, KeyEvent, Modifiers};
use anyhow::Result;

use super::char_motion;
use super::helpers;
use super::numbers;
use crate::editor::input_state::{CharMotion, InputState};

/// Apply a text object range to the visual selection.
/// If `inclusive` is true, end_col is used directly; otherwise it's decremented by 1.
fn apply_text_object(editor: &mut Editor, range: Option<TextObjectRange>, inclusive: bool) {
    if let Some(range) = range {
        // Phase-15 debt: visual_start stores grapheme cols; range cols are char.
        editor.set_visual_start(range.start_line, range.start_col.0);
        let end_col = if inclusive {
            range.end_col
        } else {
            range.end_col.saturating_sub(1)
        };
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(range.end_line, GraphemeCol(end_col.0));
    }
}

/// Mirror of normal-mode `cc` for a VisualLine-c selection: delete all
/// selected lines as a single recorded edit, open a blank line at the
/// deletion site preserving the deleted line's indent, and set up
/// PendingChangeRepeat for dot-repeat via `RepeatAction::Change`.
///
/// Vim reference (`vim -N -u NONE`): `VcNEW<Esc>` on
/// `"line one\nline two\nline three\n"` → `"NEW\nline two\nline three\n"`.
/// `j.` after that → `"NEW\nNEW\nline three\n"`.
fn handle_visual_line_change(editor: &mut Editor) -> Result<()> {
    let Some(((start_line, _), (end_line, _))) = editor.visual_selection() else {
        // No selection → nothing to delete; fall through to plain insert.
        return Ok(());
    };
    let line_count = end_line.saturating_sub(start_line) + 1;
    let cursor_before = editor.cursor_position();

    // Capture indent from the first selected line before deleting (matches
    // normal-mode `cc`: the opened blank line inherits the first line's
    // indent). Note: ovim preserves indent unconditionally; Vim only does so
    // with `autoindent`, but this matches the ovim convention established by
    // `handle_cc` / `substitute_line`.
    let indent = editor
        .buffer()
        .line_text(start_line)
        .map(|l| {
            l.chars()
                .take_while(|c| c.is_whitespace() && *c != '\n')
                .collect::<String>()
        })
        .unwrap_or_default();

    // Phase 1: delete selected lines + open blank with indent, atomically.
    let (deleted, edits) = editor.buffer_mut().record(|buf| {
        let line_count_total = buf.line_count();
        let delete_end = (end_line + 1).min(line_count_total);
        let deleted = buf.delete_range(start_line, CharCol::ZERO, delete_end, CharCol::ZERO);
        let insert_at = start_line.min(buf.line_count());
        buf.insert_text_at(insert_at, CharCol::ZERO, &format!("{}\n", indent));
        buf.cursor_mut()
            .set_position(insert_at, GraphemeCol(indent.len()));
        deleted
    });

    let delete_token = if !edits.is_empty() {
        let cursor_after = editor.cursor_position();
        Some(editor.push_recorded_undo(edits, cursor_before, cursor_after))
    } else {
        None
    };

    if !deleted.is_empty() {
        editor.delete_to_register_with_type(deleted, RegisterType::Line);
    }

    // Phase 2: set up dot-repeat + insert-mode change building.
    let delete_action = RepeatAction::DeleteVisualLine { line_count };
    // Mirror the install that `delete_visual_selection_with_token` would
    // have done, so other consumers that read last_repeat_action observe
    // the same semantic delete.
    editor.set_repeat_action(delete_action.clone());

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action,
        linewise: true,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    Ok(())
}

fn handle_visual_leader_input(
    editor: &mut Editor,
    key_event: KeyEvent,
    keys: &[char],
) -> Result<()> {
    if key_event.code == KeyCode::Esc {
        editor.reset_input_state();
        return Ok(());
    }

    let KeyCode::Char(c) = key_event.code else {
        editor.reset_input_state();
        return Ok(());
    };

    if keys.is_empty() {
        match c {
            // <Space><Space> in visual mode: attach the selection to AI chat.
            ' ' => {
                editor.start_ai_chat_from_visual()?;
                editor.reset_input_state();
            }
            _ => {
                editor.reset_input_state();
            }
        }
        return Ok(());
    }

    editor.reset_input_state();

    Ok(())
}

/// Handles input in Visual mode (Visual, VisualLine, VisualBlock)
pub fn handle_visual_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    // =====================================================================
    // INPUT STATE CHECK (must happen before mode-specific handling)
    // =====================================================================
    // If we're awaiting a character (for f/t/F/T motions), handle that first
    // before processing any visual mode specific keys. This prevents conflicts
    // where the target character (like 'e' in 'fe') would be interpreted as
    // a motion command instead of the search target.
    if let InputState::AwaitingChar { motion, operator } = editor.input_state().clone() {
        return char_motion::handle_char_motion(editor, key_event, motion, operator);
    }
    if let InputState::Leader { ref keys } = editor.input_state().clone() {
        let keys_clone = keys.clone();
        return handle_visual_leader_input(editor, key_event, &keys_clone);
    }

    // Handle pending command prefixes (g, i/a text-objects, etc.)
    if let Some(pending) = editor.pending_command() {
        editor.clear_pending_command();
        match (pending, key_event.code) {
            ('g', KeyCode::Char('g')) => {
                // gg - go to first line (or line specified by count)
                let target_line = if let Some(count) = editor.count() {
                    count.saturating_sub(1)
                } else {
                    0
                };

                let is_visual_block = editor.mode() == Mode::VisualBlock;
                let current_col = editor.buffer().cursor().col();
                let cursor = editor.buffer_mut().cursor_mut();
                cursor.set_line(target_line);

                if !is_visual_block {
                    cursor.set_col(GraphemeCol(0));
                    cursor.update_desired_col(GraphemeCol(0));
                } else {
                    cursor.set_col(current_col);
                    cursor.update_desired_col(current_col);
                }

                helpers::clamp_cursor_to_line(editor);
                editor.clear_count();
                return Ok(());
            }
            ('g', KeyCode::Char('a')) if key_event.modifiers.contains(Modifiers::CONTROL) => {
                // g Ctrl-A: Sequential increment in visual selection
                numbers::sequential_modify_numbers(editor, 1)?;
                helpers::exit_visual_mode_to_normal(editor);
                return Ok(());
            }
            ('g', KeyCode::Char('x')) if key_event.modifiers.contains(Modifiers::CONTROL) => {
                // g Ctrl-X: Sequential decrement in visual selection
                numbers::sequential_modify_numbers(editor, -1)?;
                helpers::exit_visual_mode_to_normal(editor);
                return Ok(());
            }
            ('g', KeyCode::Char('n')) => {
                // gn - extend selection to next search match
                if !editor.search_select_next() {
                    editor.set_status_message("Pattern not found".to_string());
                }
                editor.clear_count();
                return Ok(());
            }
            ('g', KeyCode::Char('N')) => {
                // gN - extend selection to previous search match
                if !editor.search_select_prev() {
                    editor.set_status_message("Pattern not found".to_string());
                }
                editor.clear_count();
                return Ok(());
            }
            ('i', KeyCode::Char('w')) => {
                apply_text_object(editor, TextObjects::inner_word(editor.buffer()), false);
                return Ok(());
            }
            ('i', KeyCode::Char('W')) => {
                apply_text_object(editor, TextObjects::inner_big_word(editor.buffer()), false);
                return Ok(());
            }
            ('a', KeyCode::Char('w')) => {
                apply_text_object(editor, TextObjects::around_word(editor.buffer()), false);
                return Ok(());
            }
            ('a', KeyCode::Char('W')) => {
                apply_text_object(editor, TextObjects::around_big_word(editor.buffer()), false);
                return Ok(());
            }
            ('i', KeyCode::Char('p')) => {
                apply_text_object(editor, TextObjects::inner_paragraph(editor.buffer()), true);
                return Ok(());
            }
            ('a', KeyCode::Char('p')) => {
                apply_text_object(editor, TextObjects::around_paragraph(editor.buffer()), true);
                return Ok(());
            }
            ('i', KeyCode::Char('"')) | ('i', KeyCode::Char('\'')) | ('i', KeyCode::Char('`')) => {
                let quote = match key_event.code {
                    KeyCode::Char(c) => c,
                    _ => return Ok(()),
                };
                apply_text_object(
                    editor,
                    TextObjects::quoted_string(editor.buffer(), quote, false),
                    false,
                );
                return Ok(());
            }
            ('a', KeyCode::Char('"')) | ('a', KeyCode::Char('\'')) | ('a', KeyCode::Char('`')) => {
                let quote = match key_event.code {
                    KeyCode::Char(c) => c,
                    _ => return Ok(()),
                };
                apply_text_object(
                    editor,
                    TextObjects::quoted_string(editor.buffer(), quote, true),
                    false,
                );
                return Ok(());
            }
            ('i', KeyCode::Char('(')) | ('i', KeyCode::Char(')')) | ('i', KeyCode::Char('b')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '(', ')', false),
                    false,
                );
                return Ok(());
            }
            ('a', KeyCode::Char('(')) | ('a', KeyCode::Char(')')) | ('a', KeyCode::Char('b')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '(', ')', true),
                    false,
                );
                return Ok(());
            }
            ('i', KeyCode::Char('[')) | ('i', KeyCode::Char(']')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '[', ']', false),
                    false,
                );
                return Ok(());
            }
            ('a', KeyCode::Char('[')) | ('a', KeyCode::Char(']')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '[', ']', true),
                    false,
                );
                return Ok(());
            }
            ('i', KeyCode::Char('{')) | ('i', KeyCode::Char('}')) | ('i', KeyCode::Char('B')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '{', '}', false),
                    false,
                );
                return Ok(());
            }
            ('a', KeyCode::Char('{')) | ('a', KeyCode::Char('}')) | ('a', KeyCode::Char('B')) => {
                apply_text_object(
                    editor,
                    TextObjects::paired_delimiters(editor.buffer(), '{', '}', true),
                    false,
                );
                return Ok(());
            }
            _ => {
                // Unknown pending command, ignore
            }
        }
    }

    match key_event.code {
        KeyCode::Esc => {
            helpers::exit_visual_mode_to_normal(editor);
        }
        KeyCode::Char(' ') => {
            editor.set_input_state(InputState::Leader { keys: Vec::new() });
        }
        // Half-page scroll down (Ctrl-D) — must come before 'd' delete handler
        KeyCode::Char('d') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            let half_page = editor.half_page_scroll();
            let count = editor.count().unwrap_or(half_page);
            let max_line = editor.buffer().line_count().saturating_sub(1);

            let cursor = editor.buffer_mut().cursor_mut();
            let new_line = (cursor.line() + count).min(max_line);
            cursor.set_line(new_line);
            helpers::clamp_cursor_to_line(editor);
            editor.clear_count();
        }
        // Half-page scroll up (Ctrl-U) — must come before 'u' lowercase handler
        KeyCode::Char('u') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            let half_page = editor.half_page_scroll();
            let count = editor.count().unwrap_or(half_page);
            let cursor = editor.buffer_mut().cursor_mut();
            let new_line = cursor.line().saturating_sub(count);
            cursor.set_line(new_line);
            helpers::clamp_cursor_to_line(editor);
            editor.clear_count();
        }
        // Text object prefixes in visual mode
        KeyCode::Char('i') | KeyCode::Char('a') => {
            // Set pending command to handle text objects (iw, aw, ip, ap, i{, a{, etc.)
            editor.set_pending_command(match key_event.code {
                KeyCode::Char(c) => c,
                _ => unreachable!(),
            });
        }
        // Motion keys work in visual mode too
        KeyCode::Char('h') | KeyCode::Left => {
            editor.set_visual_block_dollar(false);
            helpers::move_left(editor);
        }
        KeyCode::Char('j') | KeyCode::Down => {
            helpers::move_down(editor);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            helpers::move_up(editor);
        }
        KeyCode::Char('l') | KeyCode::Right => {
            editor.set_visual_block_dollar(false);
            helpers::move_right(editor);
        }
        KeyCode::Char('w') => {
            editor.set_visual_block_dollar(false);
            let count = editor.effective_count();
            Motions::word_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        KeyCode::Char('b') => {
            editor.set_visual_block_dollar(false);
            let count = editor.effective_count();
            Motions::word_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        KeyCode::Char('e') => {
            editor.set_visual_block_dollar(false);
            let count = editor.effective_count();
            Motions::word_end_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        KeyCode::Char('0') => {
            editor.set_visual_block_dollar(false);
            // If there's already a count, treat this as a digit (e.g., "50j")
            // Otherwise, treat it as a motion to column 0
            if editor.count().is_some() {
                editor.append_count(0);
            } else {
                editor.buffer_mut().cursor_mut().set_col(GraphemeCol(0));
            }
        }
        KeyCode::Char('$') => {
            if editor.mode() == Mode::VisualBlock {
                // Set "extend to end-of-line" flag so each line in the block
                // is deleted/yanked to its own end, not to a fixed column.
                editor.set_visual_block_dollar(true);
                let line_idx = editor.buffer().cursor().line();
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    let line_len = line.chars().count();
                    let col = if line_len > 0 { line_len - 1 } else { 0 };
                    let cursor = editor.buffer_mut().cursor_mut();
                    cursor.set_col(GraphemeCol(col));
                    cursor.update_desired_col(GraphemeCol(usize::MAX));
                }
            } else {
                // Normal visual mode: move to end of current line
                let line_idx = editor.buffer().cursor().line();
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    let line_len = line.chars().count();
                    let col = if line_len > 0 { line_len - 1 } else { 0 };
                    let cursor = editor.buffer_mut().cursor_mut();
                    cursor.set_col(GraphemeCol(col));
                    // Set desired_col to usize::MAX to indicate "always end of line"
                    cursor.update_desired_col(GraphemeCol(usize::MAX));
                }
            }
        }
        KeyCode::Char('G') => {
            // G - go to last line (or line specified by count)
            let target_line = if let Some(count) = editor.count() {
                count.saturating_sub(1)
            } else {
                editor.buffer().line_count().saturating_sub(1)
            };
            let is_visual_block = editor.mode() == Mode::VisualBlock;
            let current_col = editor.buffer().cursor().col();
            let cursor = editor.buffer_mut().cursor_mut();
            cursor.set_line(target_line);

            if !is_visual_block {
                cursor.set_col(GraphemeCol(0));
                cursor.update_desired_col(GraphemeCol(0));
            } else {
                cursor.set_col(current_col);
                cursor.update_desired_col(current_col);
            }

            helpers::clamp_cursor_to_line(editor);
            editor.clear_count();
        }
        KeyCode::Char('g') => {
            // g - first key of gg (go to first line with optional count)
            editor.set_pending_command('g');
        }
        // Find character forward (f)
        KeyCode::Char('f') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Find,
                operator: None,
            });
        }
        // Find character backward (F)
        KeyCode::Char('F') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::FindBack,
                operator: None,
            });
        }
        // Till character forward (t)
        KeyCode::Char('t') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Till,
                operator: None,
            });
        }
        // Till character backward (T)
        KeyCode::Char('T') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::TillBack,
                operator: None,
            });
        }
        // Jump to mark exact position (`)
        KeyCode::Char('`') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::JumpMarkExact,
                operator: None,
            });
        }
        // Jump to mark line (')
        KeyCode::Char('\'') => {
            use crate::editor::input_state::{CharMotion, InputState};
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::JumpMarkLine,
                operator: None,
            });
        }
        // Repeat the last find motion in visual modes (`;`/`,`).
        KeyCode::Char(';') => {
            editor.repeat_last_find(false);
        }
        KeyCode::Char(',') => {
            editor.repeat_last_find(true);
        }
        // Jump to matching bracket (%)
        KeyCode::Char('%') => {
            Motions::jump_to_matching_bracket(editor.buffer_mut());
            editor.clear_count();
        }
        // Search forward in visual mode
        KeyCode::Char('/') => {
            // Save visual search state for extending selection after search
            if let Some((anchor_line, anchor_col)) = editor.visual_start() {
                let mode = editor.mode();
                editor.set_visual_search_state((anchor_line, anchor_col), mode);
            }
            editor.clear_search_buffer();
            editor.set_search_forward(true);
            editor.save_search_start_position();
            editor.set_mode(Mode::Search);
        }
        // Search backward in visual mode
        KeyCode::Char('?') => {
            // Save visual search state for extending selection after search
            if let Some((anchor_line, anchor_col)) = editor.visual_start() {
                let mode = editor.mode();
                editor.set_visual_search_state((anchor_line, anchor_col), mode);
            }
            editor.clear_search_buffer();
            editor.set_search_forward(false);
            editor.save_search_start_position();
            editor.set_mode(Mode::Search);
        }
        // Search next in visual mode
        KeyCode::Char('n') => {
            editor.search_next();
        }
        // Search previous in visual mode
        KeyCode::Char('N') => {
            editor.search_prev();
        }
        // Search forward for selected text (* in visual mode)
        KeyCode::Char('*') => {
            if !helpers::search_visual_selection_forward(editor) {
                editor.set_status_message("Pattern not found".to_string());
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Search backward for selected text (# in visual mode)
        KeyCode::Char('#') => {
            if !helpers::search_visual_selection_backward(editor) {
                editor.set_status_message("Pattern not found".to_string());
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Delete selection
        KeyCode::Char('d') | KeyCode::Char('x') => {
            helpers::delete_visual_selection(editor)?;
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Yank selection
        KeyCode::Char('y') => {
            // Move cursor to start of selection before yanking (Vim behavior)
            if let Some(((start_line, start_col), (end_line, end_col))) = editor.visual_selection()
            {
                let mode = editor.mode();
                helpers::yank_visual_selection(editor)?;
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(start_line, GraphemeCol(start_col));
                // Flash the yanked region
                if mode == Mode::VisualLine {
                    editor.set_yank_flash_lines(start_line, end_line);
                } else {
                    editor.set_yank_flash_range(
                        start_line,
                        GraphemeCol(start_col),
                        end_line,
                        GraphemeCol(end_col),
                    );
                }
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Change selection
        KeyCode::Char('c') => {
            let mode_before = editor.mode();
            editor.set_pending_visual_block_change_repeat(None);
            editor.editing.pending_visual_block_change_delete_token = None;

            // VisualLine-c must mirror normal-mode `cc`: delete the whole
            // line(s), open a blank line at the deletion site (preserving the
            // deleted line's indent), and enter insert mode there. The naive
            // path of `delete_visual_selection_with_token` deletes the
            // trailing newline(s) too, which would fuse the inserted text
            // with the following line. See dot_repeat_test.rs
            // `test_dot_after_visual_line_change_multichar` and Vim's
            // behavior (`vim -N -u NONE` on `VcNEW<Esc>`).
            if mode_before == Mode::VisualLine {
                handle_visual_line_change(editor)?;
                helpers::save_and_clear_visual(editor);
                editor.set_mode(Mode::Insert);
                return Ok(());
            }

            // For visual block mode, need to track the block for multi-line insert
            let visual_block_state = if mode_before == Mode::VisualBlock {
                editor
                    .visual_selection()
                    .map(|((start_line, start_col), (end_line, end_col))| {
                        let line_count = end_line.saturating_sub(start_line) + 1;
                        let width = end_col.saturating_sub(start_col) + 1;
                        (start_line, end_line, start_col, line_count, width)
                    })
            } else {
                None
            };

            let delete_token = helpers::delete_visual_selection_with_token(editor)?;

            if let Some((start_line, end_line, start_col, line_count, width)) = visual_block_state {
                editor.set_pending_visual_block_change_repeat(Some((line_count, width)));
                editor.editing.pending_visual_block_change_delete_token = delete_token;

                // Set visual block insert state for multi-line replication
                // For 'c', move cursor to start_line (move_to_end = false)
                let cursor_before = CursorPos::new(start_line, GraphemeCol(start_col));
                editor.set_visual_block_insert_state(Some((
                    start_line, end_line, start_col, false, false,
                )));
                editor.start_change_building(cursor_before);
            } else if delete_token.is_some() {
                // Regular visual (v) with a non-empty selection: route the
                // change through PendingChangeRepeat + RepeatAction::Change
                // so that the full inserted text is captured for dot-repeat
                // (matching cw/cc/C semantics).
                //
                // delete_visual_selection_with_token has already installed
                // DeleteVisualChar as last_repeat_action; clone it as the
                // delete template for the Change action.
                let delete_action = editor
                    .buffer()
                    .change_manager()
                    .last_repeat_action
                    .clone()
                    .expect(
                        "delete_visual_selection_with_token installs a RepeatAction for \
                         non-empty selections",
                    );
                editor.set_pending_change_repeat(PendingChangeRepeat {
                    delete_action,
                    linewise: false,
                    delete_token,
                });
                editor.start_change_building(editor.cursor_position());
            }
            // else: empty selection — fall through to plain insert mode (no
            // dot-repeat template set; matches the pre-fix behavior for an
            // empty visual-c).

            helpers::save_and_clear_visual(editor);
            editor.set_mode(Mode::Insert);
        }
        // Join lines
        KeyCode::Char('J') => {
            if let Some(((start_line, _), (end_line, _))) = editor.visual_selection() {
                // Calculate expected cursor position after join
                // The cursor should be at the last space inserted (before the last line)
                let mut cursor_col = 0;
                for line_idx in start_line..end_line {
                    // Note: end_line not included
                    if let Some(line_text) = editor.buffer().line_text(line_idx) {
                        cursor_col += line_text.chars().count();
                        if line_idx < end_line - 1 {
                            cursor_col += 1; // Space after this line
                        }
                    }
                }

                // Join all lines in the selection
                let count = (end_line - start_line) + 1;
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(start_line, GraphemeCol(0));
                helpers::join_lines(editor, count)?;

                // Position cursor at the last inserted space
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(start_line, GraphemeCol(cursor_col));
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Move to other end of selection
        KeyCode::Char('o') => {
            if let Some(visual_start) = editor.visual_start() {
                let cursor = editor.buffer().cursor();
                let cursor_pos = (cursor.line(), cursor.col().0);

                if editor.mode() == Mode::VisualBlock {
                    // For visual block mode, flip to diagonally opposite corner
                    // Swap line from one with column from the other
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(visual_start.0, GraphemeCol(cursor_pos.1));
                    editor.set_visual_start(cursor_pos.0, visual_start.1);
                } else {
                    // For other visual modes, swap positions normally
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(visual_start.0, GraphemeCol(visual_start.1));
                    editor.set_visual_start(cursor_pos.0, cursor_pos.1);
                }
            }
        }
        // Flip horizontally (uppercase O) - swap columns only
        KeyCode::Char('O') => {
            if let Some(visual_start) = editor.visual_start() {
                let cursor = editor.buffer().cursor();
                let cursor_pos = (cursor.line(), cursor.col().0);

                if editor.mode() == Mode::VisualBlock {
                    // For visual block mode, flip horizontally (swap columns only, keep line)
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(cursor_pos.0, GraphemeCol(visual_start.1));
                    editor.set_visual_start(visual_start.0, cursor_pos.1);
                } else {
                    // For other visual modes, same as 'o'
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(visual_start.0, GraphemeCol(visual_start.1));
                    editor.set_visual_start(cursor_pos.0, cursor_pos.1);
                }
            }
        }
        // Switch to other visual modes
        KeyCode::Char('v') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.mode() == Mode::VisualBlock {
                helpers::exit_visual_mode_to_normal(editor);
            } else {
                editor.set_mode(Mode::VisualBlock);
            }
        }
        KeyCode::Char('v') => {
            if editor.mode() == Mode::Visual {
                helpers::exit_visual_mode_to_normal(editor);
            } else {
                // Switching to Visual mode from VisualLine or VisualBlock
                editor.set_mode(Mode::Visual);
            }
        }
        KeyCode::Char('V') => {
            if editor.mode() == Mode::VisualLine {
                helpers::exit_visual_mode_to_normal(editor);
            } else {
                // Switching to VisualLine mode
                if let Some((anchor_line, _)) = editor.visual_start() {
                    editor.set_visual_start(anchor_line, 0);
                } else {
                    let cursor = editor.buffer().cursor();
                    editor.set_visual_start(cursor.line(), 0);
                }
                editor.set_mode(Mode::VisualLine);
            }
        }
        // Visual block insert/append
        KeyCode::Char('I') => {
            if editor.mode() == Mode::VisualBlock {
                // Insert at beginning of block on each line
                if let Some(((start_line, start_col), (end_line, _))) = editor.visual_selection() {
                    let cursor_before = CursorPos::new(start_line, GraphemeCol(start_col));
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(start_line, GraphemeCol(start_col));
                    // Track visual block insert state: (start_line, end_line, col, is_append, move_to_end)
                    // For 'I', move cursor to end_line (move_to_end = true)
                    editor.set_visual_block_insert_state(Some((
                        start_line, end_line, start_col, false, true,
                    )));
                    editor.clear_visual_start();
                    editor.start_change_building(cursor_before);
                    editor.set_mode(Mode::Insert);
                }
            } else {
                // Regular visual mode - just enter insert at start of selection
                if let Some(((start_line, start_col), _)) = editor.visual_selection() {
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(start_line, GraphemeCol(start_col));
                    editor.clear_visual_start();
                    editor.set_mode(Mode::Insert);
                }
            }
        }
        KeyCode::Char('A') => {
            if editor.mode() == Mode::VisualBlock {
                // Append at end of block on each line
                if let Some(((start_line, _), (end_line, end_col))) = editor.visual_selection() {
                    // Get actual end column - clamp to line length to avoid overflow
                    let line_len = editor
                        .buffer()
                        .line_text(start_line)
                        .map(|l| l.chars().count())
                        .unwrap_or(0);
                    let actual_end_col = end_col.min(line_len.saturating_sub(1));
                    let append_col = actual_end_col.saturating_add(1);

                    // A block created "to end of line" — either via `$` inside
                    // block mode (visual_block_dollar) or a `$` before entering
                    // block mode (which leaves the sticky column at usize::MAX) —
                    // appends at each line's own EOL. A fixed-column block appends
                    // at the block column (padding short lines). Fold the sticky
                    // MAXCOL case into visual_block_dollar so the insert finalize
                    // has a single flag to consult.
                    if editor.buffer().cursor().desired_col() == usize::MAX {
                        editor.set_visual_block_dollar(true);
                    }

                    let cursor_before = CursorPos::new(start_line, GraphemeCol(append_col));
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(start_line, GraphemeCol(append_col));
                    // Track visual block append state: (start_line, end_line, col, is_append, move_to_end)
                    // For 'A', move cursor to end_line (move_to_end = true)
                    editor.set_visual_block_insert_state(Some((
                        start_line, end_line, append_col, true, true,
                    )));
                    editor.clear_visual_start();
                    editor.start_change_building(cursor_before);
                    editor.set_mode(Mode::Insert);
                }
            } else {
                // Regular visual mode - just enter insert at end of selection
                if let Some((_, (end_line, end_col))) = editor.visual_selection() {
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(end_line, GraphemeCol(end_col + 1));
                    editor.clear_visual_start();
                    editor.set_mode(Mode::Insert);
                }
            }
        }
        // Replace in visual mode (all visual variants)
        KeyCode::Char('r') => {
            // r{char} in any visual mode - wait for replacement character via input state.
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Replace,
                operator: None,
            });
        }
        // Case operations in visual mode
        KeyCode::Char('~') => {
            helpers::toggle_case_visual_selection(editor)?;
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Paste in visual mode (replace selection)
        KeyCode::Char('p') | KeyCode::Char('P') => {
            let is_visual_line = editor.mode() == Mode::VisualLine;

            // 1. Save paste text + type from register
            let (paste_text, paste_type) = editor.get_from_register_with_type();

            // 2. Delete the visual selection (saves to numbered registers + unnamed)
            helpers::delete_visual_selection(editor)?;

            // 3. Save deleted text from unnamed register
            let (deleted_text, deleted_type) = editor.get_from_register_with_type();

            // 4. Write paste text to unnamed register
            editor.registers.set_with_type(None, paste_text, paste_type);

            // 5. Branch on paste type
            if is_visual_line || paste_type == RegisterType::Line {
                // Linewise: use paste_before to insert at current line
                helpers::paste_before(editor, 1)?;
            } else {
                // Character: paste at the position the selection started. After
                // deleting the selection the cursor sits on the surviving char at
                // that column. paste_after inserts *after* the cursor, so step
                // back one column first — but at column 0 there's nothing to step
                // back over, so paste_before (which inserts AT the cursor column)
                // is required to avoid a one-char misplacement.
                let cursor_col = editor.buffer().cursor().col().0;
                if cursor_col > 0 {
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_col(GraphemeCol(cursor_col - 1));
                    helpers::paste_after(editor, 1)?;
                } else {
                    helpers::paste_before(editor, 1)?;
                }
            }

            // 6. Set unnamed register to deleted text (so next p pastes the replaced text)
            editor
                .registers
                .set_with_type(None, deleted_text, deleted_type);

            helpers::exit_visual_mode_to_normal(editor);
        }
        // Uppercase in visual mode
        KeyCode::Char('U') => {
            helpers::uppercase_visual_selection(editor)?;
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Lowercase in visual mode
        KeyCode::Char('u') => {
            helpers::lowercase_visual_selection(editor)?;
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Indent/dedent in visual mode
        KeyCode::Char('>') => {
            if let Some(((start_line, _), (end_line, _))) = editor.visual_selection() {
                let cursor = editor.buffer().cursor();
                let cursor_before = CursorPos::new(cursor.line(), cursor.col());
                let is_visual_block = editor.mode() == Mode::VisualBlock;
                let original_col = cursor_before.col.0;
                let old_indent_chars = editor
                    .buffer()
                    .line_text(end_line)
                    .map(|line| leading_char_count(&line))
                    .unwrap_or(0);

                helpers::indent_lines_with_tracking(
                    editor,
                    start_line,
                    end_line + 1,
                    cursor_before,
                )?;

                // For visual block mode, move cursor to end line at adjusted column
                if is_visual_block {
                    let new_indent_chars = editor
                        .buffer()
                        .line_text(end_line)
                        .map(|line| leading_char_count(&line))
                        .unwrap_or(0);
                    let adjusted_col = original_col
                        .saturating_sub(old_indent_chars)
                        .saturating_add(new_indent_chars);
                    let cursor = editor.buffer_mut().cursor_mut();
                    cursor.set_position(end_line, GraphemeCol(adjusted_col));
                }
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        KeyCode::Char('<') => {
            if let Some(((start_line, _), (end_line, _))) = editor.visual_selection() {
                let cursor = editor.buffer().cursor();
                let cursor_before = CursorPos::new(cursor.line(), cursor.col());
                let is_visual_block = editor.mode() == Mode::VisualBlock;

                helpers::dedent_lines_with_tracking(
                    editor,
                    start_line,
                    end_line + 1,
                    cursor_before,
                )?;

                // For visual block mode, move cursor to start position (start_line, 0)
                if is_visual_block {
                    editor
                        .buffer_mut()
                        .cursor_mut()
                        .set_position(start_line, GraphemeCol(0));
                }
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        KeyCode::Char('=') => {
            if let Some(((start_line, _), (end_line, _))) = editor.visual_selection() {
                let options = editor.indent_options();
                let cursor_before = editor.cursor_position();
                let ((), edits) = editor.buffer_mut().record(|buf| {
                    let _ = helpers::auto_indent_lines(buf, start_line, end_line + 1, options);
                });
                if !edits.is_empty() {
                    let cursor_after = editor.cursor_position();
                    // push_recorded_undo() calls mark_buffer_modified() internally
                    editor.push_recorded_undo(edits, cursor_before, cursor_after);
                }
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        // Count prefix (for motions like 5j, 10w)
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let digit = c.to_digit(10).unwrap() as usize;
            // 0 is handled separately above as a motion
            if digit != 0 || editor.count().is_some() {
                editor.append_count(digit);
            }
        }
        _ => {}
    }
    Ok(())
}
