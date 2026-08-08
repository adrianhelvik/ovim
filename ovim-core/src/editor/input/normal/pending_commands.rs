//! Pending command handlers in normal mode.
//!
//! Multi-key sequences that start with a single character:
//! g*, z*, Z*, "*, q*, @*, [*, ]*, W* (Ctrl-W)

use crate::editor::input::helpers;
use crate::editor::{CursorPos, Editor, Motions, Operator, PendingChangeRepeat};
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::{char_to_grapheme_col, grapheme_count, grapheme_to_char_col, GraphemeCol};
use crate::{KeyCode, KeyEvent};
use anyhow::Result;

/// Try to handle a pending command.
///
/// Returns `Ok(true)` if the key was handled, `Ok(false)` otherwise.
pub fn try_handle(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    let pending = match editor.pending_command() {
        Some(p) => p,
        None => return Ok(false),
    };

    editor.clear_pending_command();

    match (pending, key_event.code) {
        // =====================================================================
        // 'g' - Go commands
        // =====================================================================
        ('g', KeyCode::Char('g')) => {
            editor.add_jump();
            let max_line = editor.buffer().line_count().saturating_sub(1);
            let target_line = editor.count().unwrap_or(1).saturating_sub(1).min(max_line);
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(target_line, GraphemeCol::ZERO);
            Motions::first_non_blank(editor.buffer_mut());
            editor.add_jump();
            editor.clear_count();
        }
        ('g', KeyCode::Char('d')) => {
            editor.request_goto_definition();
            editor.clear_count();
        }
        ('g', KeyCode::Char('D')) => {
            editor.request_goto_implementation();
            editor.clear_count();
        }
        ('g', KeyCode::Char('y')) => {
            editor.request_goto_type();
            editor.clear_count();
        }
        ('g', KeyCode::Char('R')) => {
            editor.request_find_references();
            editor.clear_count();
        }
        ('g', KeyCode::Char('c')) => {
            editor.request_code_actions();
            editor.clear_count();
        }
        ('g', KeyCode::Char('q')) => {
            editor.request_format_document();
            editor.clear_count();
        }
        ('g', KeyCode::Char('J')) => {
            let count = editor.effective_count();
            helpers::join_lines_no_space(editor, count)?;
            editor.clear_count();
        }
        ('g', KeyCode::Char('e')) => {
            let count = editor.effective_count();
            Motions::word_end_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('g', KeyCode::Char('E')) => {
            let count = editor.effective_count();
            Motions::word_end_backward_big(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('g', KeyCode::Char('_')) => {
            Motions::last_non_blank(editor.buffer_mut());
            editor.clear_count();
        }
        ('g', KeyCode::Char('0')) => {
            move_to_screen_line_boundary(editor, ScreenLineTarget::Start)?;
            editor.clear_count();
        }
        ('g', KeyCode::Char('^')) => {
            move_to_screen_line_boundary(editor, ScreenLineTarget::FirstNonBlank)?;
            editor.clear_count();
        }
        ('g', KeyCode::Char('$')) => {
            move_to_screen_line_boundary(editor, ScreenLineTarget::End)?;
            editor.clear_count();
        }
        ('g', KeyCode::Char('m')) => {
            move_to_screen_line_boundary(editor, ScreenLineTarget::Middle)?;
            editor.clear_count();
        }
        ('g', KeyCode::Char('u')) => {
            editor.set_pending_operator(Operator::Lowercase);
        }
        ('g', KeyCode::Char('U')) => {
            editor.set_pending_operator(Operator::Uppercase);
        }
        ('g', KeyCode::Char('~')) => {
            editor.set_pending_operator(Operator::ToggleCase);
        }
        ('g', KeyCode::Char('r')) => {
            // gr prefix for LSP commands
            editor.set_pending_command('R');
        }
        ('g', KeyCode::Char('i')) => {
            // gi - go to last insert position and enter insert mode
            if let Some((line, col)) = editor.editing.last_insert_position {
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(line, GraphemeCol(col));
            }
            let cursor_before = CursorPos::new(
                editor.buffer().cursor().line(),
                editor.buffer().cursor().col(),
            );
            editor.start_change_building(cursor_before);
            editor.set_mode(Mode::Insert);
        }
        ('g', KeyCode::Char('v')) => {
            // gv - reselect last visual selection
            editor.restore_last_visual_selection();
        }
        ('g', KeyCode::Char('I')) => {
            // gI - insert at column 0
            editor.buffer_mut().cursor_mut().set_col(GraphemeCol::ZERO);
            let cursor_before = CursorPos::new(
                editor.buffer().cursor().line(),
                editor.buffer().cursor().col(),
            );
            editor.start_change_building(cursor_before);
            editor.set_mode(Mode::Insert);
        }
        ('g', KeyCode::Char(';')) => {
            // g; - jump to older changelist position
            let count = editor.effective_count();
            if let Some(pos) = editor.jump_change_older(count) {
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(pos.line, pos.col);
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char(',')) => {
            // g, - jump to newer changelist position
            let count = editor.effective_count();
            if let Some(pos) = editor.jump_change_newer(count) {
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(pos.line, pos.col);
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char('\'')) => {
            editor.set_input_state(crate::editor::InputState::AwaitingChar {
                motion: crate::editor::CharMotion::JumpMarkLine,
                operator: None,
            });
        }
        ('g', KeyCode::Char('t')) => {
            // gt - go to next tab
            if let Some(count) = editor.count() {
                editor.goto_tab(count.saturating_sub(1));
            } else {
                editor.next_tab();
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char('T')) => {
            // gT - go to previous tab
            editor.previous_tab();
            editor.clear_count();
        }
        ('g', KeyCode::Char('n')) => {
            // gn - select next search match
            // Save pending operator before search_select_next, as set_mode() clears it
            let saved_operator = editor.pending_operator();

            if editor.search_select_next() {
                // If we have a pending operator, apply it to the visual selection
                if let Some(op) = saved_operator {
                    apply_operator_to_visual_selection(editor, op)?;
                    editor.clear_pending_operator();
                }
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char('N')) => {
            // gN - select previous search match
            // Save pending operator before search_select_prev, as set_mode() clears it
            let saved_operator = editor.pending_operator();

            if editor.search_select_prev() {
                // If we have a pending operator, apply it to the visual selection
                if let Some(op) = saved_operator {
                    apply_operator_to_visual_selection(editor, op)?;
                    editor.clear_pending_operator();
                }
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char('j')) => {
            // gj - move down one visual (display) line
            // With soft wrap, moves within a wrapped line's visual rows
            let count = editor.count().unwrap_or(1);
            if let Some(wrap_map) = editor.wrap_map() {
                let cursor = editor.buffer().cursor();
                let line = cursor.line();
                let char_col = grapheme_to_char_col(
                    &editor
                        .buffer()
                        .line_text(line)
                        .map(|l| l.to_string())
                        .unwrap_or_default(),
                    cursor.col(),
                );
                let tab_width = editor.options.tab_width;

                // Convert char col to display col for wrap map operations
                let line_text = editor
                    .buffer()
                    .line_text(line)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                let disp_col =
                    crate::display::char_col_to_display_col(&line_text, char_col.0, tab_width);

                let (visual_row, visual_col) =
                    wrap_map.cursor_to_visual(line, disp_col, &line_text);
                let target_row = visual_row + count;
                let (new_line, sub_line) = wrap_map.visual_to_logical(
                    target_row.min(wrap_map.total_visual_lines().saturating_sub(1)),
                );
                let new_line_text = editor
                    .buffer()
                    .line_text(new_line)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                // Compute target display col and convert back to char col
                let (target_row_start, target_row_end) = wrap_map
                    .sub_line_display_range(&new_line_text, sub_line)
                    .unwrap_or((0, 0));
                let target_disp_col = target_row_start
                    + visual_col.min(target_row_end.saturating_sub(target_row_start));
                let new_col = crate::display::display_col_to_char_col(
                    &new_line_text,
                    target_disp_col,
                    tab_width,
                );
                // Clamp col to line length
                let max_col = GraphemeCol(grapheme_count(&new_line_text).saturating_sub(1));
                let target_gcol =
                    char_to_grapheme_col(&new_line_text, crate::unicode::CharCol(new_col));
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(new_line, target_gcol.min(max_col));
            } else {
                // No wrap map - fall back to regular j motion
                for _ in 0..count {
                    helpers::move_down(editor);
                }
            }
            editor.clear_count();
        }
        ('g', KeyCode::Char('b')) => {
            editor.show_blame_info();
            editor.clear_count();
        }
        ('g', KeyCode::Char('B')) => {
            editor.show_blame_diff();
            editor.clear_count();
        }
        ('g', KeyCode::Char('k')) => {
            // gk - move up one visual (display) line
            let count = editor.count().unwrap_or(1);
            if let Some(wrap_map) = editor.wrap_map() {
                let cursor = editor.buffer().cursor();
                let line = cursor.line();
                let char_col = grapheme_to_char_col(
                    &editor
                        .buffer()
                        .line_text(line)
                        .map(|l| l.to_string())
                        .unwrap_or_default(),
                    cursor.col(),
                );
                let tab_width = editor.options.tab_width;

                let line_text = editor
                    .buffer()
                    .line_text(line)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                let disp_col =
                    crate::display::char_col_to_display_col(&line_text, char_col.0, tab_width);

                let (visual_row, visual_col) =
                    wrap_map.cursor_to_visual(line, disp_col, &line_text);
                let target_row = visual_row.saturating_sub(count);
                let (new_line, sub_line) = wrap_map.visual_to_logical(target_row);
                let new_line_text = editor
                    .buffer()
                    .line_text(new_line)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                let (target_row_start, target_row_end) = wrap_map
                    .sub_line_display_range(&new_line_text, sub_line)
                    .unwrap_or((0, 0));
                let target_disp_col = target_row_start
                    + visual_col.min(target_row_end.saturating_sub(target_row_start));
                let new_col = crate::display::display_col_to_char_col(
                    &new_line_text,
                    target_disp_col,
                    tab_width,
                );
                let max_col = GraphemeCol(grapheme_count(&new_line_text).saturating_sub(1));
                let target_gcol =
                    char_to_grapheme_col(&new_line_text, crate::unicode::CharCol(new_col));
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(new_line, target_gcol.min(max_col));
            } else {
                // No wrap map - fall back to regular k motion
                for _ in 0..count {
                    helpers::move_up(editor);
                }
            }
            editor.clear_count();
        }

        // =====================================================================
        // 'R' - LSP gr commands (pending after 'gr')
        // =====================================================================
        ('R', KeyCode::Char('r')) => {
            editor.request_find_references();
            editor.clear_count();
        }
        ('R', KeyCode::Char('n')) => {
            // grn - LSP rename with pre-filled word under cursor
            let word = editor.buffer().word_under_cursor().map(|(w, _, _)| w);
            let word = word.unwrap_or_default();
            editor.set_rename_buffer(word);
            editor.set_mode(Mode::RenameInput);
        }
        ('R', KeyCode::Char('a')) => {
            editor.request_code_actions();
            editor.clear_count();
        }
        ('R', KeyCode::Char('i')) => {
            editor.request_goto_implementation();
            editor.clear_count();
        }
        ('R', KeyCode::Char('t')) => {
            editor.request_goto_type();
            editor.clear_count();
        }

        // =====================================================================
        // 'z' - Fold/scroll commands
        // =====================================================================
        ('z', KeyCode::Char('o')) => {
            let line = editor.buffer().cursor().line();
            editor.buffer_mut().open_fold(line);
        }
        ('z', KeyCode::Char('c')) => {
            let line = editor.buffer().cursor().line();
            editor.buffer_mut().close_fold(line);
        }
        ('z', KeyCode::Char('a')) => {
            let line = editor.buffer().cursor().line();
            editor.buffer_mut().toggle_fold(line);
        }
        ('z', KeyCode::Char('R')) => {
            editor.buffer_mut().fold_manager_mut().open_all();
        }
        ('z', KeyCode::Char('M')) => {
            editor.buffer_mut().fold_manager_mut().close_all();
        }
        ('z', KeyCode::Char('d')) => {
            let line = editor.buffer().cursor().line();
            editor.buffer_mut().fold_manager_mut().delete_fold_at(line);
        }
        ('z', KeyCode::Char('E')) => {
            editor.buffer_mut().clear_folds();
        }
        ('z', KeyCode::Char('f')) => {
            editor.set_pending_operator(Operator::Fold);
        }
        ('z', KeyCode::Char('z')) => {
            // [count]zz — scroll cursor line (or line [count]) to center
            move_cursor_to_count_line(editor);
            editor.center_cursor_in_viewport();
            editor.clear_count();
        }
        ('z', KeyCode::Char('t')) => {
            // [count]zt — scroll cursor line (or line [count]) to top
            move_cursor_to_count_line(editor);
            editor.move_cursor_line_to_top();
            editor.clear_count();
        }
        ('z', KeyCode::Char('b')) => {
            // [count]zb — scroll cursor line (or line [count]) to bottom
            move_cursor_to_count_line(editor);
            editor.move_cursor_line_to_bottom();
            editor.clear_count();
        }
        ('z', KeyCode::Char('s')) => {
            editor.scroll_cursor_to_left_edge();
            editor.clear_count();
        }
        ('z', KeyCode::Char('e')) => {
            editor.scroll_cursor_to_right_edge();
            editor.clear_count();
        }
        ('z', KeyCode::Enter) => {
            // [count]z<CR> — like zt, then first non-blank
            move_cursor_to_count_line(editor);
            editor.move_cursor_line_to_top();
            Motions::first_non_blank(editor.buffer_mut());
            editor.clear_count();
        }
        ('z', KeyCode::Char('-')) => {
            // [count]z- — like zb, then first non-blank
            move_cursor_to_count_line(editor);
            editor.move_cursor_line_to_bottom();
            Motions::first_non_blank(editor.buffer_mut());
            editor.clear_count();
        }
        ('z', KeyCode::Char('.')) => {
            // [count]z. — like zz, then first non-blank
            move_cursor_to_count_line(editor);
            editor.center_cursor_in_viewport();
            Motions::first_non_blank(editor.buffer_mut());
            editor.clear_count();
        }

        // =====================================================================
        // 'Z' - Save/quit commands
        // =====================================================================
        ('Z', KeyCode::Char('Z')) => {
            if editor.is_chat_scratch_buffer() {
                if let Err(error) = editor.finish_chat_scratch(true) {
                    editor.set_status_message(format!("Could not finish chat scratch: {error}"));
                }
            } else {
                if editor.buffer().file_path().is_some()
                    && tokio::runtime::Handle::try_current().is_ok()
                {
                    // OV-00204: Don't quit if save fails — show error instead
                    if let Err(e) = editor.buffer_mut().save() {
                        editor.set_status_message(format!("Save failed: {}", e));
                        return Ok(true);
                    }
                }
                editor.quit();
            }
        }
        ('Z', KeyCode::Char('Q')) => {
            editor.quit();
        }

        // =====================================================================
        // '"' - Register selection
        // =====================================================================
        ('"', KeyCode::Char(ch))
            if ch.is_ascii_alphanumeric()
                || ch == '"'
                || ch == '_'
                || ch == '-'
                || ch == '+'
                || ch == '*'
                || ch == '%'
                || ch == '.'
                || ch == ':'
                || ch == '#'
                || ch == '/' =>
        {
            editor.set_pending_register(ch);
        }

        // =====================================================================
        // 'q' - Start macro recording
        // =====================================================================
        ('q', KeyCode::Char(ch)) if ch.is_ascii_lowercase() => {
            editor.start_macro_recording(ch);
        }

        // =====================================================================
        // '@' - Play macro / '@@' - Repeat last macro
        // =====================================================================
        ('@', KeyCode::Char('@')) => {
            if let Some(register) = editor.last_played_macro() {
                let count = editor.effective_count();
                editor.clear_count();
                if let Some(events) = editor.get_macro(register) {
                    let events = events.clone();
                    editor.clear_macro_abort();
                    'outer_repeat: for _ in 0..count {
                        for event in &events {
                            crate::editor::input::InputHandler::handle_key_event(editor, *event)?;
                            if editor.macro_aborted() {
                                break 'outer_repeat;
                            }
                        }
                    }
                    editor.clear_macro_abort();
                }
            }
        }
        ('@', KeyCode::Char(ch)) if ch.is_ascii_lowercase() => {
            let count = editor.effective_count();
            editor.clear_count();
            editor.set_last_played_macro(ch);
            if let Some(events) = editor.get_macro(ch) {
                let events = events.clone();
                editor.clear_macro_abort();
                'outer: for _ in 0..count {
                    for event in &events {
                        crate::editor::input::InputHandler::handle_key_event(editor, *event)?;
                        if editor.macro_aborted() {
                            break 'outer;
                        }
                    }
                }
                editor.clear_macro_abort();
            }
        }

        // =====================================================================
        // '[' - Section/bracket backward navigation
        // =====================================================================
        ('[', KeyCode::Char('[')) => {
            let count = editor.effective_count();
            Motions::section_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char(']')) => {
            let count = editor.effective_count();
            Motions::section_end_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char('{')) => {
            let count = editor.effective_count();
            Motions::unmatched_brace_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char('(')) => {
            let count = editor.effective_count();
            Motions::unmatched_paren_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char('m')) => {
            let count = editor.effective_count();
            Motions::method_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char('M')) => {
            let count = editor.effective_count();
            Motions::method_end_backward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        ('[', KeyCode::Char('d')) => {
            editor.goto_prev_diagnostic();
            editor.clear_count();
        }
        ('[', KeyCode::Char('a')) => {
            editor.goto_agent_edit(false);
            editor.clear_count();
        }

        // =====================================================================
        // ']' - Section/bracket forward navigation
        // =====================================================================
        (']', KeyCode::Char(']')) => {
            let count = editor.effective_count();
            Motions::section_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char('[')) => {
            let count = editor.effective_count();
            Motions::section_end_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char('}')) => {
            let count = editor.effective_count();
            Motions::unmatched_brace_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char(')')) => {
            let count = editor.effective_count();
            Motions::unmatched_paren_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char('m')) => {
            let count = editor.effective_count();
            Motions::method_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char('M')) => {
            let count = editor.effective_count();
            Motions::method_end_forward(editor.buffer_mut(), count);
            editor.clear_count();
        }
        (']', KeyCode::Char('d')) => {
            editor.goto_next_diagnostic();
            editor.clear_count();
        }
        (']', KeyCode::Char('a')) => {
            editor.goto_agent_edit(true);
            editor.clear_count();
        }

        // =====================================================================
        // 'W' - Window commands (Ctrl-W prefix)
        // =====================================================================
        ('W', KeyCode::Char('w')) => {
            editor.focus_next_window();
        }
        ('W', KeyCode::Char('p')) => {
            editor.focus_prev_window();
        }
        ('W', KeyCode::Char('s')) => {
            editor.split_window_horizontal();
        }
        ('W', KeyCode::Char('v')) => {
            editor.split_window_vertical();
        }
        ('W', KeyCode::Char('o')) => {
            // <C-w>o - close all other windows (like :only)
            editor.close_other_windows();
        }
        ('W', KeyCode::Char('c')) => {
            // <C-w>c - close current window (silently fails if last window)
            let _ = editor.close_current_window();
        }
        ('W', KeyCode::Char('q')) => {
            // <C-w>q - close current window, quit if last window
            editor.close_or_quit_window();
        }
        ('W', KeyCode::Char('h')) => {
            editor.focus_window_left();
        }
        ('W', KeyCode::Char('j')) => {
            editor.focus_window_down();
        }
        ('W', KeyCode::Char('k')) => {
            editor.focus_window_up();
        }
        ('W', KeyCode::Char('l')) => {
            editor.focus_window_right();
        }

        _ => {
            // Unknown command sequence
            editor.clear_count();
        }
    }

    Ok(true)
}

/// Apply a pending operator to a visual selection created by gn/gN.
///
/// After `gn` or `gN` selects a search match and enters visual mode, this function
/// applies the pending operator (if any) to that selection. Supports:
/// - Delete operator (d)
/// - Yank operator (y)
/// - Change operator (c)
fn apply_operator_to_visual_selection(editor: &mut Editor, operator: Operator) -> Result<()> {
    match operator {
        Operator::Delete => {
            helpers::delete_visual_selection(editor)?;
            helpers::exit_visual_mode_to_normal(editor);
        }
        Operator::Yank => {
            if let Some(((start_line, start_col), (end_line, end_col))) = editor.visual_selection()
            {
                helpers::yank_visual_selection(editor)?;
                editor.set_yank_flash_range(
                    start_line,
                    GraphemeCol(start_col),
                    end_line,
                    GraphemeCol(end_col),
                );
            }
            helpers::exit_visual_mode_to_normal(editor);
        }
        Operator::Change => {
            let search_info = editor
                .current_search()
                .map(|s| (s.pattern().to_string(), s.is_forward()));

            let delete_token = helpers::delete_visual_selection_with_token(editor)?;

            let cursor = editor.buffer().cursor();
            let cursor_after_delete = CursorPos::new(cursor.line(), cursor.col());

            if let Some((pattern, forward)) = search_info {
                editor.set_pending_change_repeat(PendingChangeRepeat {
                    delete_action: RepeatAction::DeleteSearchMatch {
                        search_pattern: pattern,
                        search_forward: forward,
                    },
                    linewise: false,
                    delete_token,
                });
            }

            editor.start_change_building(cursor_after_delete);
            helpers::save_and_clear_visual(editor);
            editor.set_mode(Mode::Insert);
        }
        _ => {} // Other operators not supported with gn/gN
    }
    Ok(())
}

enum ScreenLineTarget {
    Start,
    FirstNonBlank,
    End,
    Middle,
}

fn move_to_screen_line_boundary(editor: &mut Editor, target: ScreenLineTarget) -> Result<()> {
    let cursor = editor.buffer().cursor();
    let line_idx = cursor.line();
    let cursor_char_col = cursor.col();

    // Without wrap map, g0/g^/g$/gm reduce to logical line semantics.
    if !editor.options.wrap || editor.wrap_map().is_none() {
        let line_text = editor
            .buffer()
            .line_text(line_idx)
            .map(|l| l.to_string())
            .unwrap_or_default();
        let len = grapheme_count(&line_text);
        let target_col = match target {
            ScreenLineTarget::Start => 0,
            ScreenLineTarget::FirstNonBlank => line_text
                .chars()
                .position(|c| !c.is_whitespace())
                .unwrap_or(0),
            ScreenLineTarget::End => len.saturating_sub(1),
            ScreenLineTarget::Middle => len.saturating_sub(1) / 2,
        };
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line_idx, GraphemeCol(target_col));
        return Ok(());
    }

    let wrap_map = match editor.wrap_map() {
        Some(m) => m,
        None => return Ok(()),
    };
    let tab_width = editor.options.tab_width;

    let line_text = editor
        .buffer()
        .line_text(line_idx)
        .map(|l| l.to_string())
        .unwrap_or_default();
    let line_len = grapheme_count(&line_text);
    let char_col = grapheme_to_char_col(&line_text, cursor_char_col);
    let disp_col = crate::display::char_col_to_display_col(&line_text, char_col.0, tab_width);
    let (visual_row, _visual_col) = wrap_map.cursor_to_visual(line_idx, disp_col, &line_text);
    let (_cursor_line, sub_line) = wrap_map.visual_to_logical(visual_row);

    let (screen_start, screen_end) = wrap_map
        .sub_line_display_range(&line_text, sub_line)
        .unwrap_or((0, 0));
    let screen_end_exclusive = screen_end.max(1);
    let screen_end = screen_end_exclusive.saturating_sub(1);

    let target_disp_col = match target {
        ScreenLineTarget::Start => screen_start,
        ScreenLineTarget::End => screen_end,
        ScreenLineTarget::Middle => screen_start + (screen_end.saturating_sub(screen_start) / 2),
        ScreenLineTarget::FirstNonBlank => {
            let mut col = screen_start;
            while col < screen_end_exclusive {
                let char_idx = crate::display::display_col_to_char_col(&line_text, col, tab_width);
                let ch = line_text.chars().nth(char_idx).unwrap_or(' ');
                if !ch.is_whitespace() {
                    break;
                }
                col += 1;
            }
            col.min(screen_end)
        }
    };

    let target_char_col =
        crate::display::display_col_to_char_col(&line_text, target_disp_col, tab_width);
    let target_col = char_to_grapheme_col(&line_text, crate::unicode::CharCol(target_char_col))
        .min(GraphemeCol(line_len.saturating_sub(1)));
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(line_idx, target_col);
    Ok(())
}

/// If the editor has an explicit count, move cursor to that line (1-indexed).
/// Used by viewport commands (zt, zz, zb, etc.) where [count] means
/// "use line [count]" not "repeat [count] times".
fn move_cursor_to_count_line(editor: &mut Editor) {
    if let Some(count) = editor.count() {
        let target = count.saturating_sub(1); // 1-indexed → 0-indexed
        let max_line = editor.buffer().line_count().saturating_sub(1);
        let line = target.min(max_line);
        let col = editor.buffer().cursor().col();
        editor.buffer_mut().cursor_mut().set_position(line, col);
        editor.buffer_mut().validate_cursor_position();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modifiers;

    #[test]
    fn test_gj_preserves_visual_column_with_tab_and_nonuniform_wrap_segments() {
        let mut editor = Editor::with_content("\t世b\n");

        editor.ensure_wrap_map(5);

        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(0, GraphemeCol::ZERO);
        editor.set_pending_command('g');

        try_handle(
            &mut editor,
            KeyEvent::new(KeyCode::Char('j'), Modifiers::NONE),
        )
        .unwrap();

        assert_eq!(editor.buffer().cursor().line(), 0);
        assert_eq!(editor.buffer().cursor().col(), GraphemeCol(1));
    }

    #[test]
    fn test_g0_targets_wrap_segment_start_with_tab_and_nonuniform_wrap_segments() {
        let mut editor = Editor::with_content("\t世b\n");

        editor.ensure_wrap_map(5);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(0, GraphemeCol(2));

        move_to_screen_line_boundary(&mut editor, ScreenLineTarget::Start).unwrap();

        assert_eq!(editor.buffer().cursor().line(), 0);
        assert_eq!(editor.buffer().cursor().col(), GraphemeCol(1));
    }
}
