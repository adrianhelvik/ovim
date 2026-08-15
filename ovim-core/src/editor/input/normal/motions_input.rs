//! Motion commands in normal mode.
//!
//! Simple cursor movement commands:
//! h, j, k, l, w, W, b, B, e, E, 0, $, ^, _, +, -, G, %, {, }, (, ), ;, ,, n, N, *, #, K

use crate::editor::input::helpers;
use crate::editor::{Editor, Motions, Search};
use crate::unicode::GraphemeCol;
use crate::{KeyCode, KeyEvent, Modifiers};
use anyhow::Result;

/// Try to handle a motion command.
///
/// Returns `Ok(true)` if the key was handled, `Ok(false)` otherwise.
pub fn try_handle(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    // Handle Ctrl key combinations first (must be checked before regular keys)
    if key_event.modifiers.contains(Modifiers::CONTROL) {
        return try_handle_ctrl_motion(editor, key_event);
    }

    // Handle regular motions
    match key_event.code {
        // Basic motions
        KeyCode::Char('h') | KeyCode::Left => {
            helpers::move_left(editor);
            Ok(true)
        }
        KeyCode::Char('j') | KeyCode::Down => {
            helpers::move_down(editor);
            Ok(true)
        }
        KeyCode::Char('k') | KeyCode::Up => {
            helpers::move_up(editor);
            Ok(true)
        }
        KeyCode::Char('l') | KeyCode::Right => {
            helpers::move_right(editor);
            Ok(true)
        }

        // K - debug evaluate (when stopped) or LSP hover
        KeyCode::Char('K') => {
            if editor.is_debug_stopped() {
                // Evaluate word under cursor via DAP.
                let word = editor
                    .buffer()
                    .word_under_cursor()
                    .map(|(w, _, _)| w)
                    .unwrap_or_default();
                if !word.is_empty() {
                    editor.dap_manager_mut().pending_action =
                        Some(crate::dap::PendingDebugAction::Evaluate { expression: word });
                }
            } else {
                editor.request_hover();
            }
            editor.clear_count();
            Ok(true)
        }

        // Line motions
        KeyCode::Char('0') => {
            // 0 is either a motion or count digit
            if editor.count().is_some() {
                editor.append_count(0);
            } else {
                editor.buffer_mut().cursor_mut().set_col(GraphemeCol::ZERO);
                editor.clear_count();
            }
            Ok(true)
        }
        KeyCode::Char('$') => {
            let count = editor.effective_count();
            let line_idx = editor.buffer().cursor().line();
            let max_line = editor.buffer().line_count().saturating_sub(1);
            let target_line = (line_idx + count - 1).min(max_line);
            if let Some(line) = editor.buffer().line_text(target_line) {
                let line_len = line.chars().count();
                let col = if line_len > 0 { line_len - 1 } else { 0 };
                let cursor = editor.buffer_mut().cursor_mut();
                cursor.set_position(target_line, GraphemeCol(col));
                cursor.update_desired_col(GraphemeCol(usize::MAX));
            }
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('|') => {
            // {count}| — go to column {count} (default 1), clamped to the
            // last character. Verified `nvim --clean` on a 26-char line:
            // `19|` → col('.') == 19, `99|` → col('.') == 26 (OV-00338).
            let count = editor.effective_count();
            let line_idx = editor.buffer().cursor().line();
            if let Some(line) = editor.buffer().line_text(line_idx) {
                let line_len = line.chars().count();
                let max_col = line_len.saturating_sub(1);
                let col = count.saturating_sub(1).min(max_col);
                let cursor = editor.buffer_mut().cursor_mut();
                cursor.set_col(GraphemeCol(col));
                cursor.update_desired_col(GraphemeCol(col));
            }
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('^') => {
            Motions::first_non_blank(editor.buffer_mut());
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('_') => {
            let count = editor.effective_count();
            if count > 1 {
                let line_idx = editor.buffer().cursor().line();
                let max_line = editor.buffer().line_count().saturating_sub(1);
                let target_line = (line_idx + count - 1).min(max_line);
                editor.buffer_mut().cursor_mut().set_line(target_line);
            }
            Motions::first_non_blank(editor.buffer_mut());
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('+') => {
            let count = editor.effective_count();
            Motions::plus_motion(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('-') => {
            let count = editor.effective_count();
            Motions::minus_motion(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }

        // Count prefix (digits 1-9)
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let digit = c.to_digit(10).unwrap() as usize;
            if digit != 0 || editor.count().is_some() {
                editor.append_count(digit);
            }
            Ok(true)
        }

        // Word motions
        KeyCode::Char('w') => {
            let count = editor.effective_count();
            Motions::word_forward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('W') => {
            let count = editor.effective_count();
            Motions::word_forward_big(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('b') => {
            let count = editor.effective_count();
            Motions::word_backward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('B') => {
            let count = editor.effective_count();
            Motions::word_backward_big(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('e') => {
            let count = editor.effective_count();
            Motions::word_end_forward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('E') => {
            let count = editor.effective_count();
            Motions::word_end_forward_big(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }

        // File motions
        KeyCode::Char('H') => {
            let offset = editor.effective_count().saturating_sub(1);
            let scroll_offset = editor.scroll_offset();
            Motions::move_to_screen_top(editor.buffer_mut(), scroll_offset, offset);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('M') => {
            let scroll_offset = editor.scroll_offset();
            let viewport_height = editor.viewport_height();
            Motions::move_to_screen_middle(editor.buffer_mut(), scroll_offset, viewport_height);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('L') => {
            let offset = editor.effective_count().saturating_sub(1);
            let scroll_offset = editor.scroll_offset();
            let viewport_height = editor.viewport_height();
            Motions::move_to_screen_bottom(
                editor.buffer_mut(),
                scroll_offset,
                viewport_height,
                offset,
            );
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('G') => {
            let max_line = editor.buffer().line_count().saturating_sub(1);
            let target_line = if let Some(count) = editor.count() {
                count.saturating_sub(1).min(max_line)
            } else {
                max_line
            };
            editor.add_jump();
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(target_line, GraphemeCol::ZERO);
            Motions::first_non_blank(editor.buffer_mut());
            editor.add_jump();
            editor.clear_count();
            Ok(true)
        }

        // Jump to matching bracket
        KeyCode::Char('%') => {
            Motions::jump_to_matching_bracket(editor.buffer_mut());
            editor.clear_count();
            Ok(true)
        }

        // Paragraph motions
        KeyCode::Char('}') => {
            let count = editor.effective_count();
            Motions::paragraph_forward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('{') => {
            let count = editor.effective_count();
            Motions::paragraph_backward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }

        // Sentence motions
        KeyCode::Char(')') => {
            let count = editor.effective_count();
            Motions::sentence_forward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('(') => {
            let count = editor.effective_count();
            Motions::sentence_backward(editor.buffer_mut(), count);
            editor.clear_count();
            Ok(true)
        }

        // Search next/previous
        KeyCode::Char('n') => {
            editor.search_next();
            Ok(true)
        }
        KeyCode::Char('N') => {
            editor.search_prev();
            Ok(true)
        }

        // Search word under cursor
        KeyCode::Char('*') => {
            search_word_forward(editor);
            Ok(true)
        }
        KeyCode::Char('#') => {
            search_word_backward(editor);
            Ok(true)
        }

        // Repeat find motion
        KeyCode::Char(';') => {
            editor.repeat_last_find(false);
            Ok(true)
        }
        KeyCode::Char(',') => {
            editor.repeat_last_find(true);
            Ok(true)
        }
        KeyCode::Tab => {
            editor.jump_forward();
            Ok(true)
        }

        _ => Ok(false),
    }
}

/// Handle Ctrl+key combinations for motions and scrolling.
fn try_handle_ctrl_motion(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    match key_event.code {
        // Scroll commands
        KeyCode::Char('d') => {
            let half_page = editor.half_page_scroll();
            let count = editor.count().unwrap_or(half_page);
            let max_line = editor.buffer().line_count().saturating_sub(1);

            let cursor = editor.buffer_mut().cursor_mut();
            let new_line = (cursor.line() + count).min(max_line);
            cursor.set_line(new_line);
            // Preserve the goal column across shorter lines, matching j/k.
            helpers::clamp_cursor_with_goal_column(editor);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('u') => {
            let half_page = editor.half_page_scroll();
            let count = editor.count().unwrap_or(half_page);
            let cursor = editor.buffer_mut().cursor_mut();
            let new_line = cursor.line().saturating_sub(count);
            cursor.set_line(new_line);
            // Preserve the goal column across shorter lines, matching j/k.
            helpers::clamp_cursor_with_goal_column(editor);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('e') => {
            let count = editor.effective_count();
            editor.scroll_viewport_down(count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('y') => {
            let count = editor.effective_count();
            editor.scroll_viewport_up(count);
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('f') => {
            // [count]CTRL-F scrolls [count] pages forward.
            for _ in 0..editor.effective_count() {
                editor.scroll_page_down();
            }
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('b') => {
            // [count]CTRL-B scrolls [count] pages backward.
            for _ in 0..editor.effective_count() {
                editor.scroll_page_up();
            }
            editor.clear_count();
            Ok(true)
        }

        // Go to implementation in new tab
        KeyCode::Char('i') => {
            editor.request_goto_implementation_new_tab();
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('o') => {
            editor.jump_back();
            Ok(true)
        }
        KeyCode::Char('t') => {
            editor.tag_pop();
            Ok(true)
        }

        // Go to definition in new tab
        KeyCode::Char('g') => {
            editor.request_goto_definition_new_tab();
            editor.clear_count();
            Ok(true)
        }

        // Quit
        KeyCode::Char('q') => {
            editor.quit();
            Ok(true)
        }

        // Window commands prefix
        KeyCode::Char('w') => {
            editor.set_pending_command('W');
            Ok(true)
        }

        // Number increment/decrement
        KeyCode::Char('a') => {
            let count = editor.effective_count();
            super::super::numbers::increment_number(editor, count)?;
            editor.clear_count();
            Ok(true)
        }
        KeyCode::Char('x') => {
            let count = editor.effective_count();
            super::super::numbers::decrement_number(editor, count)?;
            editor.clear_count();
            Ok(true)
        }

        _ => Ok(false),
    }
}

/// Search for word under cursor (forward)
fn search_word_forward(editor: &mut Editor) {
    if let Some((word, _, _)) = editor.buffer().word_under_cursor() {
        let pattern = format!(r"\b{}\b", regex::escape(&word));
        let mut search = Search::new_with_options(
            pattern,
            true,
            editor.options.ignorecase,
            editor.options.smartcase,
        );
        let cursor = editor.buffer().cursor();

        if let Some((line, col, _)) = search.find_next(
            editor.buffer(),
            cursor.line(),
            GraphemeCol(cursor.col().0 + 1),
        ) {
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
        editor.set_current_search(search);
    }
}

/// Search for word under cursor (backward)
fn search_word_backward(editor: &mut Editor) {
    if let Some((word, _, _)) = editor.buffer().word_under_cursor() {
        let pattern = format!(r"\b{}\b", regex::escape(&word));
        let mut search = Search::new_with_options(
            pattern,
            false,
            editor.options.ignorecase,
            editor.options.smartcase,
        );
        let cursor = editor.buffer().cursor();

        let search_col = if cursor.col().0 > 0 {
            cursor.col().0 - 1
        } else {
            0
        };
        if let Some((line, col, _)) =
            search.find_next(editor.buffer(), cursor.line(), GraphemeCol(search_col))
        {
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
        editor.set_current_search(search);
    }
}
