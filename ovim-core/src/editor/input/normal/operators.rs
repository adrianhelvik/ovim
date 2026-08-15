//! Operator + motion handling in normal mode.
//!
//! Handles pending operators combined with motions:
//! - `dd`, `dw`, `dW`, `de`, `dE`, `db`, `dB`, `d$`, `dh`, `dl`, `d0`, `d^`
//!   `dj`, `dk`, `d{`, `d}`, `d%`, `dG`, `dgg`, `df`, `dt`, `dF`, `dT`
//! - `yy`, `yw`, `yW`, `ye`, `yE`, `yb`, `yB`, `y$`, `yh`, `y0`, `y^`
//!   `yj`, `yk`, `y{`, `y}`, `yG`, `ygg`, `yf`, `yt`, `yF`, `yT`
//! - `cc`, `cw`, `cW`, `ce`, `cE`, `cb`, `cB`, `c$`, `ch`, `cl`, `c0`, `c^`
//!   `cj`, `ck`, `c{`, `c}`, `cG`, `cgg`, `cf`, `ct`, `cF`, `cT`
//! - `>>`, `>j`, `>k`, `>G`, `>gg`
//! - `<<`, `<j`, `<k`, `<G`, `<gg`
//! - `zf{motion}`
//! - `gu*`, `gU*`, `g~*`

use crate::editor::input::helpers;
use crate::editor::{
    CharMotion, CursorPos, Editor, InputState, Motions, Operator, PendingChangeRepeat, RegisterType,
};
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::{CharCol, GraphemeCol};
use crate::{KeyCode, KeyEvent};
use anyhow::Result;

use super::super::case;

/// Try to handle a pending operator with motion.
///
/// Returns `Ok(true)` if the key was handled, `Ok(false)` otherwise.
pub fn try_handle(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    // Skip if we have a text object prefix ('i' or 'a')
    let has_text_obj_prefix = matches!(editor.pending_command(), Some('i') | Some('a'));
    if has_text_obj_prefix {
        return Ok(false);
    }

    let operator = match editor.pending_operator() {
        Some(op) => op,
        None => return Ok(false),
    };

    let count = editor.effective_count();

    // K is not a motion, so operator+K should just cancel the operator
    if key_event.code == KeyCode::Char('K') {
        editor.clear_pending_operator();
        editor.clear_count();
        return Ok(true);
    }

    // Handle character motions with operators (df, dt, cf, ct, yf, yt, etc.)
    if let Some(handled) = try_handle_char_motion_with_operator(editor, operator, key_event)? {
        return Ok(handled);
    }

    // Handle 'g' prefix for gg motion and gn/gN motions
    // All linewise operators support gg: dgg, ygg, cgg, >gg, <gg, zfgg
    // dgn, ygn, cgn ARE also supported (gn is a search motion)
    if key_event.code == KeyCode::Char('g')
        && editor.pending_command() != Some('g')
        && matches!(
            operator,
            Operator::Indent
                | Operator::Dedent
                | Operator::AutoIndent
                | Operator::Fold
                | Operator::Change
                | Operator::Delete
                | Operator::Yank
        )
    {
        editor.set_pending_command('g');
        return Ok(true);
    }

    // Handle gg motion with operators (dgg, ygg, cgg, >gg, <gg, zfgg)
    if editor.pending_command() == Some('g') && key_event.code == KeyCode::Char('g') {
        return handle_gg_motion(editor, operator, count);
    }

    // Handle gn and gN motions (search next/prev) - delegate to pending_commands
    if editor.pending_command() == Some('g')
        && matches!(key_event.code, KeyCode::Char('n') | KeyCode::Char('N'))
    {
        // Don't clear the operator - let pending_commands handle it
        return Ok(false);
    }

    // Handle G motion with operators
    if key_event.code == KeyCode::Char('G') {
        return handle_g_motion(editor, operator, count);
    }

    // Clear pending operator for the main match (will be restored if needed)
    editor.clear_pending_operator();

    let handled = match (operator, key_event.code) {
        // =====================================================================
        // Delete operations
        // =====================================================================
        (Operator::Delete, KeyCode::Char('d')) => {
            handle_dd(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('l')) | (Operator::Delete, KeyCode::Right) => {
            handle_dl(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('w')) => {
            handle_dw(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('$')) => {
            handle_d_dollar(editor)?;
            true
        }
        (Operator::Delete, KeyCode::Char('j')) => {
            handle_dj(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('k')) => {
            handle_dk(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('}')) => {
            handle_d_paragraph_forward(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('{')) => {
            handle_d_paragraph_backward(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('%')) => {
            handle_d_percent(editor)?;
            true
        }
        (Operator::Delete, KeyCode::Char('b')) => {
            handle_db(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('e')) => {
            handle_de(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('B')) => {
            handle_d_big_b(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('E')) => {
            handle_d_big_e(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('h')) | (Operator::Delete, KeyCode::Left) => {
            handle_dh(editor, count)?;
            true
        }
        (Operator::Delete, KeyCode::Char('0')) => {
            handle_d0(editor)?;
            true
        }
        (Operator::Delete, KeyCode::Char('^')) => {
            handle_d_caret(editor)?;
            true
        }
        (Operator::Delete, KeyCode::Char('W')) => {
            handle_d_big_w(editor, count)?;
            true
        }

        // =====================================================================
        // Yank operations
        // =====================================================================
        (Operator::Yank, KeyCode::Char('y')) => {
            let start_line = editor.buffer().cursor().line();
            let end_line = (start_line + count).min(editor.buffer().line_count()) - 1;
            let yanked = helpers::yank_line(editor.buffer(), count)?;
            editor.yank_to_register_with_type(yanked, RegisterType::Line);
            editor.set_yank_flash_lines(start_line, end_line);
            editor.clear_count();
            true
        }
        (Operator::Yank, KeyCode::Char('w')) => {
            let start_line = editor.buffer().cursor().line();
            let start_col = editor.buffer().cursor().col().0;
            let yanked = helpers::yank_word(editor.buffer_mut(), count)?;
            let end_col = start_col + yanked.chars().count().saturating_sub(1);
            editor.yank_to_register(yanked);
            editor.set_yank_flash_range(
                start_line,
                GraphemeCol(start_col),
                start_line,
                GraphemeCol(end_col),
            );
            editor.clear_count();
            true
        }
        (Operator::Yank, KeyCode::Char('$')) => {
            let line = editor.buffer().cursor().line();
            let start_col = editor.buffer().cursor().col().0;
            let yanked = helpers::yank_to_end_of_line(editor.buffer())?;
            let end_col = start_col + yanked.chars().count().saturating_sub(1);
            editor.yank_to_register(yanked);
            editor.set_yank_flash_range(line, GraphemeCol(start_col), line, GraphemeCol(end_col));
            editor.clear_count();
            true
        }
        (Operator::Yank, KeyCode::Char('j')) => {
            handle_yj(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('k')) => {
            handle_yk(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('}')) => {
            handle_y_paragraph_forward(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('{')) => {
            handle_y_paragraph_backward(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('b')) => {
            handle_yb(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('e')) => {
            handle_ye(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('B')) => {
            handle_y_big_b(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('E')) => {
            handle_y_big_e(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('h')) | (Operator::Yank, KeyCode::Left) => {
            handle_yh(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('0')) => {
            handle_y0(editor)?;
            true
        }
        (Operator::Yank, KeyCode::Char('^')) => {
            handle_y_caret(editor)?;
            true
        }
        (Operator::Yank, KeyCode::Char('W')) => {
            handle_y_big_w(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('l')) | (Operator::Yank, KeyCode::Right) => {
            handle_yl(editor, count)?;
            true
        }
        (Operator::Yank, KeyCode::Char('%')) => {
            handle_y_percent(editor)?;
            true
        }

        // =====================================================================
        // Change operations
        // =====================================================================
        (Operator::Change, KeyCode::Char('c')) => {
            handle_cc(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('w')) => {
            handle_cw(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('$')) => {
            handle_c_dollar(editor)?;
            true
        }
        (Operator::Change, KeyCode::Char('l')) | (Operator::Change, KeyCode::Right) => {
            handle_cl(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('j')) => {
            handle_cj(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('k')) => {
            handle_ck(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('}')) => {
            handle_c_paragraph_forward(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('{')) => {
            handle_c_paragraph_backward(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('b')) => {
            handle_cb(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('e')) => {
            handle_ce(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('B')) => {
            handle_c_big_b(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('E')) => {
            handle_c_big_e(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('h')) | (Operator::Change, KeyCode::Left) => {
            handle_ch(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('0')) => {
            handle_c0(editor)?;
            true
        }
        (Operator::Change, KeyCode::Char('^')) => {
            handle_c_caret(editor)?;
            true
        }
        (Operator::Change, KeyCode::Char('W')) => {
            handle_c_big_w(editor, count)?;
            true
        }
        (Operator::Change, KeyCode::Char('%')) => {
            handle_c_percent(editor)?;
            true
        }

        // =====================================================================
        // Case change operations
        // =====================================================================
        (Operator::Lowercase, KeyCode::Char('u')) => {
            case::change_case_line(editor, count, case::CaseChange::Lowercase)?;
            editor.clear_count();
            true
        }
        (Operator::Uppercase, KeyCode::Char('U')) => {
            case::change_case_line(editor, count, case::CaseChange::Uppercase)?;
            editor.clear_count();
            true
        }
        (Operator::ToggleCase, KeyCode::Char('~')) => {
            case::change_case_line(editor, count, case::CaseChange::Toggle)?;
            editor.clear_count();
            true
        }
        (Operator::Lowercase, KeyCode::Char('w')) => {
            case::change_case_motion(
                editor,
                count,
                case::CaseChange::Lowercase,
                false,
                |buf, cnt| {
                    Motions::word_forward(buf, cnt);
                },
            )?;
            editor.clear_count();
            true
        }
        (Operator::Uppercase, KeyCode::Char('w')) => {
            case::change_case_motion(
                editor,
                count,
                case::CaseChange::Uppercase,
                false,
                |buf, cnt| {
                    Motions::word_forward(buf, cnt);
                },
            )?;
            editor.clear_count();
            true
        }
        (Operator::ToggleCase, KeyCode::Char('w')) => {
            case::change_case_motion(
                editor,
                count,
                case::CaseChange::Toggle,
                false,
                |buf, cnt| {
                    Motions::word_forward(buf, cnt);
                },
            )?;
            editor.clear_count();
            true
        }
        (Operator::Lowercase, KeyCode::Char('e')) => {
            case::change_case_motion(
                editor,
                count,
                case::CaseChange::Lowercase,
                true,
                |buf, cnt| {
                    Motions::word_end_forward(buf, cnt);
                },
            )?;
            editor.clear_count();
            true
        }
        (Operator::Uppercase, KeyCode::Char('e')) => {
            case::change_case_motion(
                editor,
                count,
                case::CaseChange::Uppercase,
                true,
                |buf, cnt| {
                    Motions::word_end_forward(buf, cnt);
                },
            )?;
            editor.clear_count();
            true
        }
        (Operator::ToggleCase, KeyCode::Char('e')) => {
            case::change_case_motion(editor, count, case::CaseChange::Toggle, true, |buf, cnt| {
                Motions::word_end_forward(buf, cnt);
            })?;
            editor.clear_count();
            true
        }
        (Operator::Lowercase, KeyCode::Char('$')) => {
            case::change_case_to_end_of_line(editor, case::CaseChange::Lowercase)?;
            editor.clear_count();
            true
        }
        (Operator::Uppercase, KeyCode::Char('$')) => {
            case::change_case_to_end_of_line(editor, case::CaseChange::Uppercase)?;
            editor.clear_count();
            true
        }
        (Operator::ToggleCase, KeyCode::Char('$')) => {
            case::change_case_to_end_of_line(editor, case::CaseChange::Toggle)?;
            editor.clear_count();
            true
        }

        // =====================================================================
        // Fold operations
        // =====================================================================
        (Operator::Fold, KeyCode::Char('j')) => {
            let start_line = editor.buffer().cursor().line();
            let end_line = (start_line + count).min(editor.buffer().line_count().saturating_sub(1));
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
            editor.clear_count();
            true
        }
        (Operator::Fold, KeyCode::Char('k')) => {
            let end_line = editor.buffer().cursor().line() + 1;
            let start_line = editor.buffer().cursor().line().saturating_sub(count);
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
            editor.clear_count();
            true
        }
        (Operator::Fold, KeyCode::Char('}')) => {
            let start_line = editor.buffer().cursor().line();
            Motions::paragraph_forward(editor.buffer_mut(), count);
            let end_line = editor.buffer().cursor().line();
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
            editor.clear_count();
            true
        }
        (Operator::Fold, KeyCode::Char('{')) => {
            let end_line = editor.buffer().cursor().line();
            Motions::paragraph_backward(editor.buffer_mut(), count);
            let start_line = editor.buffer().cursor().line();
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
            editor.clear_count();
            true
        }
        (Operator::Fold, KeyCode::Char('%')) => {
            handle_zf_percent(editor)?;
            true
        }

        // =====================================================================
        // Indent operations
        // =====================================================================
        (Operator::Indent, KeyCode::Char('>')) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let start_line = cursor.line();
            let end_line = start_line + count;
            helpers::indent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }
        (Operator::Indent, KeyCode::Char('j')) | (Operator::Indent, KeyCode::Down) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let start_line = cursor.line();
            let end_line = start_line + count + 1;
            helpers::indent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }
        (Operator::Indent, KeyCode::Char('k')) | (Operator::Indent, KeyCode::Up) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let current_line = cursor.line();
            let start_line = current_line.saturating_sub(count);
            let end_line = current_line + 1;
            helpers::indent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }

        // =====================================================================
        // Auto-indent operations
        // =====================================================================
        (Operator::AutoIndent, KeyCode::Char('=')) => {
            let cursor = editor.buffer().cursor();
            let start_line = cursor.line();
            let end_line = start_line + count;
            let options = editor.options.indent_options();
            helpers::auto_indent_lines_with_tracking(editor, start_line, end_line, options)?;
            editor.clear_count();
            true
        }
        (Operator::AutoIndent, KeyCode::Char('j')) | (Operator::AutoIndent, KeyCode::Down) => {
            let cursor = editor.buffer().cursor();
            let start_line = cursor.line();
            let end_line = start_line + count + 1;
            let options = editor.options.indent_options();
            helpers::auto_indent_lines_with_tracking(editor, start_line, end_line, options)?;
            editor.clear_count();
            true
        }
        (Operator::AutoIndent, KeyCode::Char('k')) | (Operator::AutoIndent, KeyCode::Up) => {
            let cursor = editor.buffer().cursor();
            let current_line = cursor.line();
            let start_line = current_line.saturating_sub(count);
            let end_line = current_line + 1;
            let options = editor.options.indent_options();
            helpers::auto_indent_lines_with_tracking(editor, start_line, end_line, options)?;
            editor.clear_count();
            true
        }

        // =====================================================================
        // Dedent operations
        // =====================================================================
        (Operator::Dedent, KeyCode::Char('<')) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let start_line = cursor.line();
            let end_line = start_line + count;
            helpers::dedent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }
        (Operator::Dedent, KeyCode::Char('j')) | (Operator::Dedent, KeyCode::Down) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let start_line = cursor.line();
            let end_line = start_line + count + 1;
            helpers::dedent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }
        (Operator::Dedent, KeyCode::Char('k')) | (Operator::Dedent, KeyCode::Up) => {
            let cursor = editor.buffer().cursor();
            let cursor_before = CursorPos::new(cursor.line(), cursor.col());
            let current_line = cursor.line();
            let start_line = current_line.saturating_sub(count);
            let end_line = current_line + 1;
            helpers::dedent_lines_with_tracking(editor, start_line, end_line, cursor_before)?;
            editor.clear_count();
            true
        }

        // =====================================================================
        // Count digits after operator (e.g., d2w)
        // =====================================================================
        (_, KeyCode::Char(c)) if c.is_ascii_digit() && c != '0' => {
            let digit = c.to_digit(10).unwrap() as usize;
            editor.append_count(digit);
            editor.set_pending_operator(operator); // Restore operator
            true
        }

        // =====================================================================
        // Text object prefixes
        // =====================================================================
        (_, KeyCode::Char('i')) => {
            editor.set_pending_operator(operator);
            editor.set_pending_command('i');
            true
        }
        (_, KeyCode::Char('a')) => {
            editor.set_pending_operator(operator);
            editor.set_pending_command('a');
            true
        }

        _ => {
            editor.clear_count();
            false
        }
    };

    Ok(handled)
}

/// Handle character motions with operators (df, dt, cf, ct, yf, yt, etc.)
fn try_handle_char_motion_with_operator(
    editor: &mut Editor,
    operator: Operator,
    key_event: KeyEvent,
) -> Result<Option<bool>> {
    let motion = match key_event.code {
        KeyCode::Char('f')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::Find
        }
        KeyCode::Char('t')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::Till
        }
        KeyCode::Char('F')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::FindBack
        }
        KeyCode::Char('T')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::TillBack
        }
        KeyCode::Char('`')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::JumpMarkExact
        }
        KeyCode::Char('\'')
            if matches!(
                operator,
                Operator::Delete | Operator::Change | Operator::Yank
            ) =>
        {
            CharMotion::JumpMarkLine
        }
        _ => return Ok(None),
    };

    editor.set_input_state(InputState::AwaitingChar {
        motion,
        operator: Some(operator),
    });
    Ok(Some(true))
}

/// Handle G motion with operator (dG, cG, yG, >G, <G, zfG)
fn handle_g_motion(editor: &mut Editor, operator: Operator, count: usize) -> Result<bool> {
    editor.clear_pending_operator();
    let cursor = editor.buffer().cursor();
    let cursor_before = CursorPos::new(cursor.line(), cursor.col());
    let cursor_line = cursor.line();
    let max_line = editor.buffer().line_count().saturating_sub(1);
    let target_line = if editor.count().is_some() {
        count.saturating_sub(1).min(max_line)
    } else {
        max_line
    };

    // Normalize so start_line <= end_line
    let start_line = cursor_line.min(target_line);
    let end_line = cursor_line.max(target_line);

    match operator {
        Operator::Indent => {
            helpers::indent_lines_with_tracking(editor, start_line, end_line + 1, cursor_before)?;
        }
        Operator::Dedent => {
            helpers::dedent_lines_with_tracking(editor, start_line, end_line + 1, cursor_before)?;
        }
        Operator::AutoIndent => {
            let options = editor.options.indent_options();
            helpers::auto_indent_lines_with_tracking(editor, start_line, end_line + 1, options)?;
        }
        Operator::Delete => {
            let deleted = editor.record_operation(
                |buf| buf.delete_to_last_line(target_line),
                Some(RepeatAction::DeleteToLastLine { target_line }),
            );
            if !deleted.is_empty() {
                editor.delete_to_register_with_type(deleted, RegisterType::Line);
            }
        }
        Operator::Yank => {
            // Yank from start_line to end_line (inclusive, line-wise).
            // `line_text` strips terminators; re-add `\n` per line so the
            // register stores linewise content with the standard one-`\n`-
            // per-line shape.
            let mut yanked = String::new();
            for line_idx in start_line..=end_line {
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    yanked.push_str(&line);
                    yanked.push('\n');
                }
            }
            editor.yank_to_register_with_type(yanked, RegisterType::Line);
            editor.set_yank_flash_lines(start_line, end_line);
            // Cursor stays at original position for yank
        }
        Operator::Fold => {
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
        }
        Operator::Change => {
            // Get indent from the cursor's line (top of the range we're changing)
            let indent = editor
                .buffer()
                .line_text(start_line)
                .map(|l| {
                    l.chars()
                        .take_while(|c| c.is_whitespace() && *c != '\n')
                        .collect::<String>()
                })
                .unwrap_or_default();

            let (deleted, edits) = editor.buffer_mut().record(|buf| {
                let cur = buf.cursor().line();
                let tgt = target_line;
                let del_start = cur.min(tgt);
                let del_end = (cur.max(tgt) + 1).min(buf.line_count());
                let deleted = buf.delete_range(del_start, CharCol::ZERO, del_end, CharCol::ZERO);
                // Insert a blank line at where the deletion started
                let insert_at = del_start.min(buf.line_count());
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
            editor.delete_to_register_with_type(deleted, RegisterType::Line);
            editor.mark_buffer_modified();

            editor.set_pending_change_repeat(PendingChangeRepeat {
                delete_action: RepeatAction::DeleteToLastLine { target_line },
                linewise: true,
                delete_token,
            });
            editor.start_change_building(editor.cursor_position());
            editor.set_mode(Mode::Insert);
        }
        _ => {}
    }

    editor.clear_count();
    Ok(true)
}

/// Handle gg motion with operator (dgg, ygg, cgg, >gg, <gg, zfgg)
fn handle_gg_motion(editor: &mut Editor, operator: Operator, count: usize) -> Result<bool> {
    editor.clear_pending_operator();
    editor.clear_pending_command();

    let cursor_line = editor.buffer().cursor().line();
    let cursor_before = CursorPos::new(cursor_line, editor.buffer().cursor().col());
    let max_line = editor.buffer().line_count().saturating_sub(1);
    let target_line = if editor.count().is_some() {
        count.saturating_sub(1).min(max_line)
    } else {
        0
    };

    // Normalize so start_line <= end_line
    let start_line = cursor_line.min(target_line);
    let end_line = cursor_line.max(target_line);

    match operator {
        Operator::Delete => {
            let deleted = editor.record_operation(
                |buf| buf.delete_to_first_line(target_line),
                Some(RepeatAction::DeleteToFirstLine { target_line }),
            );
            if !deleted.is_empty() {
                editor.delete_to_register_with_type(deleted, RegisterType::Line);
            }
        }
        Operator::Yank => {
            // Yank from start_line to end_line (inclusive, line-wise).
            // `line_text` strips terminators; re-add `\n` per line so the
            // register stores linewise content with the standard one-`\n`-
            // per-line shape.
            let mut yanked = String::new();
            for line_idx in start_line..=end_line {
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    yanked.push_str(&line);
                    yanked.push('\n');
                }
            }
            editor.yank_to_register_with_type(yanked, RegisterType::Line);
            editor.set_yank_flash_lines(start_line, end_line);
            // Cursor stays at original position for yank
        }
        Operator::Indent => {
            helpers::indent_lines_with_tracking(editor, start_line, end_line + 1, cursor_before)?;
        }
        Operator::Dedent => {
            helpers::dedent_lines_with_tracking(editor, start_line, end_line + 1, cursor_before)?;
        }
        Operator::AutoIndent => {
            let options = editor.options.indent_options();
            helpers::auto_indent_lines_with_tracking(editor, start_line, end_line + 1, options)?;
        }
        Operator::Fold => {
            editor
                .buffer_mut()
                .fold_manager_mut()
                .create_fold(start_line, end_line);
        }
        Operator::Change => {
            let indent = editor
                .buffer()
                .line_text(start_line)
                .map(|l| {
                    l.chars()
                        .take_while(|c| c.is_whitespace() && *c != '\n')
                        .collect::<String>()
                })
                .unwrap_or_default();

            let (deleted, edits) = editor.buffer_mut().record(|buf| {
                let cur = buf.cursor().line();
                let tgt = target_line;
                let del_start = cur.min(tgt);
                let del_end = (cur.max(tgt) + 1).min(buf.line_count());
                let deleted = buf.delete_range(del_start, CharCol::ZERO, del_end, CharCol::ZERO);
                // Insert a blank line at where the deletion started
                let insert_at = del_start.min(buf.line_count());
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
            editor.delete_to_register_with_type(deleted, RegisterType::Line);
            editor.mark_buffer_modified();

            editor.set_pending_change_repeat(PendingChangeRepeat {
                delete_action: RepeatAction::DeleteToFirstLine { target_line },
                linewise: true,
                delete_token,
            });
            editor.start_change_building(editor.cursor_position());
            editor.set_mode(Mode::Insert);
        }
        _ => {}
    }

    editor.clear_count();
    Ok(true)
}

// =====================================================================
// Individual operator handlers
// =====================================================================

fn handle_dd(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_lines(count),
        Some(RepeatAction::DeleteLines { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register_with_type(deleted, RegisterType::Line);
    }
    editor.clear_count();
    Ok(())
}

fn handle_dl(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_chars_forward(count),
        Some(RepeatAction::DeleteCharForward { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_dw(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_forward(count),
        Some(RepeatAction::DeleteWordForward { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_dollar(editor: &mut Editor) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_to_end_of_line(),
        Some(RepeatAction::DeleteToEndOfLine),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_dj(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_line_down(count),
        Some(RepeatAction::DeleteLineDown { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register_with_type(deleted, RegisterType::Line);
    }
    editor.clear_count();
    Ok(())
}

fn handle_dk(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_line_up(count),
        Some(RepeatAction::DeleteLineUp { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register_with_type(deleted, RegisterType::Line);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_paragraph_forward(editor: &mut Editor, count: usize) -> Result<()> {
    let (deleted, wise) = editor.record_operation(
        |buf| buf.delete_paragraph_forward(count),
        Some(RepeatAction::DeleteParagraphForward { count }),
    );
    if !deleted.is_empty() {
        // Register type follows the motion's classification: a mid-line d}
        // is charwise (OV-00293), a line-start d} is linewise.
        editor.delete_to_register_with_type(deleted, register_type_for_wise(wise));
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_paragraph_backward(editor: &mut Editor, count: usize) -> Result<()> {
    let (deleted, wise) = editor.record_operation(
        |buf| buf.delete_paragraph_backward(count),
        Some(RepeatAction::DeleteParagraphBackward { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register_with_type(deleted, register_type_for_wise(wise));
    }
    editor.clear_count();
    Ok(())
}

fn register_type_for_wise(wise: crate::motion_range::Wise) -> RegisterType {
    match wise {
        crate::motion_range::Wise::Linewise => RegisterType::Line,
        crate::motion_range::Wise::Charwise => RegisterType::Character,
    }
}

fn handle_d_percent(editor: &mut Editor) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_to_matching_bracket(),
        Some(RepeatAction::DeleteToMatchingBracket),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_yj(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let end_line = (start_line + count + 1).min(editor.buffer().line_count());

    // Linewise register: each line keeps its terminator (line_text strips it),
    // otherwise the pasted lines glue into one merged line.
    let mut yanked = String::new();
    for line_idx in start_line..end_line {
        if let Some(line) = editor.buffer().line_text(line_idx) {
            yanked.push_str(&line);
            yanked.push('\n');
        }
    }
    editor.yank_to_register_with_type(yanked, RegisterType::Line);
    editor.set_yank_flash_lines(start_line, end_line.saturating_sub(1));
    editor.clear_count();
    Ok(())
}

fn handle_yk(editor: &mut Editor, count: usize) -> Result<()> {
    let end_line = editor.buffer().cursor().line() + 1;
    let start_line = editor.buffer().cursor().line().saturating_sub(count);

    // Linewise register: keep a terminator on every yanked line.
    let mut yanked = String::new();
    for line_idx in start_line..end_line {
        if let Some(line) = editor.buffer().line_text(line_idx) {
            yanked.push_str(&line);
            yanked.push('\n');
        }
    }
    editor.yank_to_register_with_type(yanked, RegisterType::Line);
    editor.set_yank_flash_lines(start_line, end_line.saturating_sub(1));
    editor.clear_count();
    Ok(())
}

fn handle_y_paragraph_forward(editor: &mut Editor, count: usize) -> Result<()> {
    use crate::motion_range::{MotionRange, Wise};

    let start_line = editor.buffer().cursor().line();
    let start_grapheme = editor.buffer().cursor().col();
    let start_col = editor.buffer().cursor_char_col();

    Motions::paragraph_forward(editor.buffer_mut(), count);
    let end = (
        editor.buffer().cursor().line(),
        editor.buffer().cursor_char_col(),
    );

    // } is exclusive: a mid-line y} yanks charwise up to the end of the
    // paragraph's last line; a line-start y} yanks linewise (OV-00293).
    let range = MotionRange::from_exclusive(editor.buffer(), (start_line, start_col), end);
    let yanked = editor.buffer().yank_motion_range(range);
    editor.yank_to_register_with_type(yanked, register_type_for_wise(range.wise));
    match range.wise {
        Wise::Linewise => editor.set_yank_flash_lines(range.start.0, range.end.0),
        Wise::Charwise => {
            flash_charwise_range(editor, range);
        }
    }
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(start_line, start_grapheme);
    editor.clear_count();
    Ok(())
}

fn handle_y_paragraph_backward(editor: &mut Editor, count: usize) -> Result<()> {
    use crate::motion_range::{MotionRange, Wise};

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor_char_col();

    Motions::paragraph_backward(editor.buffer_mut(), count);
    let start = (
        editor.buffer().cursor().line(),
        editor.buffer().cursor_char_col(),
    );

    // { is exclusive: the original cursor character is not yanked
    // (OV-00293).
    let range = MotionRange::from_exclusive(editor.buffer(), start, (end_line, end_col));
    let yanked = editor.buffer().yank_motion_range(range);
    editor.yank_to_register_with_type(yanked, register_type_for_wise(range.wise));
    match range.wise {
        Wise::Linewise => editor.set_yank_flash_lines(range.start.0, range.end.0),
        Wise::Charwise => {
            flash_charwise_range(editor, range);
        }
    }
    // Vim leaves the cursor at the start of the yanked region.
    let start_text = editor
        .buffer()
        .line_text(range.start.0)
        .unwrap_or_default()
        .to_string();
    let start_grapheme = crate::unicode::char_to_grapheme_col(&start_text, range.start.1);
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(range.start.0, start_grapheme);
    editor.clear_count();
    Ok(())
}

/// Flash a charwise motion range, converting its char cols to the
/// grapheme space the flash API expects.
fn flash_charwise_range(editor: &mut Editor, range: crate::motion_range::MotionRange) {
    let start_text = editor
        .buffer()
        .line_text(range.start.0)
        .unwrap_or_default()
        .to_string();
    let end_text = editor
        .buffer()
        .line_text(range.end.0)
        .unwrap_or_default()
        .to_string();
    let start_grapheme = crate::unicode::char_to_grapheme_col(&start_text, range.start.1);
    let end_grapheme = crate::unicode::char_to_grapheme_col(&end_text, range.end.1);
    editor.set_yank_flash_range(
        range.start.0,
        start_grapheme,
        range.end.0,
        GraphemeCol(end_grapheme.0.saturating_sub(1)),
    );
}

/// Shared ceremony for a **charwise** change operator (`cw`, `ce`, `cb`, `c%`,
/// …): record the deletion, push an undo token, yank the deleted text to the
/// register, register the dot-repeat action, and drop into insert mode.
///
/// The `delete` closure performs the deletion and must leave the cursor at the
/// insert point. Change deletes must NOT clamp the cursor to the normal-mode
/// bound — in insert mode `col == line_len` (append at EOL) is valid. Use the
/// `Buffer::change_*` methods (un-clamped), not the `delete_*` ones used by the
/// `d` operators. Linewise changes (`cc`, `cj`, `ck`) don't use this.
fn change_with(
    editor: &mut Editor,
    delete_action: RepeatAction,
    delete: impl FnOnce(&mut crate::buffer::Buffer) -> String,
) -> Result<()> {
    let cursor_before = editor.cursor_position();

    let (deleted, edits) = editor.buffer_mut().record(delete);
    let delete_token = if !edits.is_empty() {
        let cursor_after = editor.cursor_position();
        Some(editor.push_recorded_undo(edits, cursor_before, cursor_after))
    } else {
        None
    };
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
        editor.mark_buffer_modified();
    }

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action,
        linewise: false,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_cc(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor_before = editor.cursor_position();
    let start_line = editor.buffer().cursor().line();
    let end_line = (start_line + count).min(editor.buffer().line_count());

    // Capture indentation BEFORE deleting
    let indent = editor
        .buffer()
        .line_text(start_line)
        .map(|l| {
            l.chars()
                .take_while(|c| c.is_whitespace() && *c != '\n')
                .collect::<String>()
        })
        .unwrap_or_default();

    // Phase 1: Delete lines + open blank line with indent (recorded for undo)
    let (deleted, edits) = editor.buffer_mut().record(|buf| {
        let deleted = buf.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
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
    editor.delete_to_register_with_type(deleted, RegisterType::Line);
    editor.mark_buffer_modified();

    // Phase 2: Set up for insert mode
    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteLines { count },
        linewise: true,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_cw(editor: &mut Editor, count: usize) -> Result<()> {
    // cw delete phase: vim special-cases this — ce-like on a non-blank,
    // dw-like on a blank. See Buffer::change_word_forward.
    change_with(editor, RepeatAction::DeleteWordChange { count }, |buf| {
        buf.change_word_forward(count)
    })
}

fn handle_c_dollar(editor: &mut Editor) -> Result<()> {
    let cursor_before = editor.cursor_position();
    let line_idx = cursor_before.line;
    let col = cursor_before.col.0;

    let (deleted, edits) = editor.buffer_mut().record(|buf| {
        let line_len = buf
            .line_text(line_idx)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        if col < line_len {
            // Phase-15 debt: cursor cols are grapheme, delete_range needs char.
            let deleted = buf.delete_range(line_idx, CharCol(col), line_idx, CharCol(line_len));
            buf.cursor_mut().set_position(line_idx, GraphemeCol(col));
            deleted
        } else {
            String::new()
        }
    });
    let delete_token = if !edits.is_empty() {
        let cursor_after = editor.cursor_position();
        let token = editor.push_recorded_undo(edits, cursor_before, cursor_after);
        editor.delete_to_register(deleted);
        editor.mark_buffer_modified();
        Some(token)
    } else {
        None
    };

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteToEndOfLine,
        linewise: false,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_cl(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor = editor.buffer().cursor();
    let cursor_before = CursorPos::new(cursor.line(), cursor.col());
    let line_idx = cursor.line();
    let start_col = cursor.col().0;

    if let Some(line_text) = editor.buffer().line_text(line_idx) {
        let line_len = line_text.chars().count();
        let end_col = (start_col + count).min(line_len);

        if start_col < end_col {
            let (deleted, edits) = editor.buffer_mut().record(|buf| {
                // Phase-15 debt: cursor cols are grapheme, delete_range needs char.
                let d = buf.delete_range(line_idx, CharCol(start_col), line_idx, CharCol(end_col));
                buf.cursor_mut()
                    .set_position(line_idx, GraphemeCol(start_col));
                d
            });
            let delete_token = if !edits.is_empty() {
                let cursor_after = editor.cursor_position();
                let token = editor.push_recorded_undo(edits, cursor_before, cursor_after);
                editor.delete_to_register(deleted);
                editor.mark_buffer_modified();
                Some(token)
            } else {
                None
            };

            editor.set_pending_change_repeat(PendingChangeRepeat {
                delete_action: RepeatAction::DeleteCharForward { count },
                linewise: false,
                delete_token,
            });
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(line_idx, GraphemeCol(start_col));

            editor.start_change_building(editor.cursor_position());
            editor.clear_count();
            editor.set_mode(Mode::Insert);
            return Ok(());
        }
    }
    editor.clear_count();
    Ok(())
}

fn handle_cj(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor_before = editor.cursor_position();
    let start_line = editor.buffer().cursor().line();
    let end_line = (start_line + count + 1).min(editor.buffer().line_count());

    let indent = editor
        .buffer()
        .line_text(start_line)
        .map(|l| {
            l.chars()
                .take_while(|c| c.is_whitespace() && *c != '\n')
                .collect::<String>()
        })
        .unwrap_or_default();

    let (deleted, edits) = editor.buffer_mut().record(|buf| {
        let deleted = buf.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
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
    editor.delete_to_register_with_type(deleted, RegisterType::Line);
    editor.mark_buffer_modified();

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteLineDown { count },
        linewise: true,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_ck(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor_before = editor.cursor_position();
    let end_line = editor.buffer().cursor().line() + 1;
    let start_line = editor.buffer().cursor().line().saturating_sub(count);

    let indent = editor
        .buffer()
        .line_text(start_line)
        .map(|l| {
            l.chars()
                .take_while(|c| c.is_whitespace() && *c != '\n')
                .collect::<String>()
        })
        .unwrap_or_default();

    let (deleted, edits) = editor.buffer_mut().record(|buf| {
        let deleted = buf.delete_range(start_line, CharCol::ZERO, end_line, CharCol::ZERO);
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
    editor.delete_to_register_with_type(deleted, RegisterType::Line);
    editor.mark_buffer_modified();

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteLineUp { count },
        linewise: true,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_c_paragraph_forward(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor_before = editor.cursor_position();

    let ((deleted, wise), edits) = editor
        .buffer_mut()
        .record(|buf| buf.delete_paragraph_forward(count));
    let delete_token = if !edits.is_empty() {
        let cursor_after = editor.cursor_position();
        Some(editor.push_recorded_undo(edits, cursor_before, cursor_after))
    } else {
        None
    };
    editor.delete_to_register_with_type(deleted, register_type_for_wise(wise));
    editor.mark_buffer_modified();

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteParagraphForward { count },
        linewise: false,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_c_paragraph_backward(editor: &mut Editor, count: usize) -> Result<()> {
    let cursor_before = editor.cursor_position();

    let ((deleted, wise), edits) = editor
        .buffer_mut()
        .record(|buf| buf.delete_paragraph_backward(count));
    let delete_token = if !edits.is_empty() {
        let cursor_after = editor.cursor_position();
        Some(editor.push_recorded_undo(edits, cursor_before, cursor_after))
    } else {
        None
    };
    editor.delete_to_register_with_type(deleted, register_type_for_wise(wise));
    editor.mark_buffer_modified();

    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteParagraphBackward { count },
        linewise: false,
        delete_token,
    });
    editor.start_change_building(editor.cursor_position());
    editor.clear_count();
    editor.set_mode(Mode::Insert);
    Ok(())
}

fn handle_zf_percent(editor: &mut Editor) -> Result<()> {
    let cursor = editor.buffer().cursor();
    let start_line = cursor.line();
    let start_col = cursor.col().0;

    let rope = editor.buffer().rope();
    let text = rope.to_string();
    let chars: Vec<char> = text.chars().collect();

    let mut abs_start = 0;
    for i in 0..start_line {
        if i < rope.len_lines() {
            abs_start += rope.line(i).len_chars();
        }
    }
    abs_start += start_col;

    if abs_start >= chars.len() {
        editor.clear_count();
        return Ok(());
    }

    let current_char = chars[abs_start];

    let (is_opening, matching_char) = match current_char {
        '(' => (true, ')'),
        ')' => (false, '('),
        '[' => (true, ']'),
        ']' => (false, '['),
        '{' => (true, '}'),
        '}' => (false, '{'),
        '<' => (true, '>'),
        '>' => (false, '<'),
        _ => {
            editor.clear_count();
            return Ok(());
        }
    };

    let match_abs_pos = if is_opening {
        Motions::find_matching_bracket_forward(&chars, abs_start, current_char, matching_char)
    } else {
        Motions::find_matching_bracket_backward(&chars, abs_start, matching_char, current_char)
    };

    if let Some(abs_end) = match_abs_pos {
        let (fold_start_line, _) = Motions::abs_pos_to_line_col(rope, abs_start.min(abs_end));
        let (fold_end_line, _) = Motions::abs_pos_to_line_col(rope, abs_start.max(abs_end));
        editor
            .buffer_mut()
            .fold_manager_mut()
            .create_fold(fold_start_line, fold_end_line);
    }

    editor.clear_count();
    Ok(())
}

// =====================================================================
// Delete handlers for new motions (db, de, dB, dE, dh, d0, d^, dW)
// =====================================================================

fn handle_db(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_backward(count),
        Some(RepeatAction::DeleteWordBackward { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_de(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_end(count),
        Some(RepeatAction::DeleteWordEnd { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_big_b(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_backward_big(count),
        Some(RepeatAction::DeleteWordBackwardBig { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_big_e(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_end_big(count),
        Some(RepeatAction::DeleteWordEndBig { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_dh(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_char_left(count),
        Some(RepeatAction::DeleteCharLeft { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d0(editor: &mut Editor) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_to_start_of_line(),
        Some(RepeatAction::DeleteToStartOfLine),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_caret(editor: &mut Editor) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_to_first_non_blank(),
        Some(RepeatAction::DeleteToFirstNonBlank),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

fn handle_d_big_w(editor: &mut Editor, count: usize) -> Result<()> {
    let deleted = editor.record_operation(
        |buf| buf.delete_word_forward_big(count),
        Some(RepeatAction::DeleteWordForwardBig { count }),
    );
    if !deleted.is_empty() {
        editor.delete_to_register(deleted);
    }
    editor.clear_count();
    Ok(())
}

// =====================================================================
// Yank handlers for new motions (yb, ye, yB, yE, yh, y0, y^, yW)
// =====================================================================

fn handle_yb(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let start_col = editor.buffer().cursor().col().0;

    Motions::word_backward(editor.buffer_mut(), count);

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor().col().0;

    // Phase-15 debt: cursor cols are grapheme, yank_range needs char.
    let yanked = yank_range(
        editor,
        end_line,
        CharCol(end_col),
        start_line,
        CharCol(start_col),
    );
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        end_line,
        GraphemeCol(end_col),
        start_line,
        GraphemeCol(start_col.saturating_sub(1)),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(end_line, GraphemeCol(end_col));
    editor.clear_count();
    Ok(())
}

fn handle_ye(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let start_col = editor.buffer().cursor().col().0;

    Motions::word_end_forward(editor.buffer_mut(), count);

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor().col().0;

    // Inclusive: include the char motion lands on.
    // Phase-15 debt: cursor cols are grapheme, yank_range needs char.
    let yanked = yank_range(
        editor,
        start_line,
        CharCol(start_col),
        end_line,
        CharCol(end_col + 1),
    );
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        start_line,
        GraphemeCol(start_col),
        end_line,
        GraphemeCol(end_col),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(start_line, GraphemeCol(start_col));
    editor.clear_count();
    Ok(())
}

fn handle_y_big_b(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let start_col = editor.buffer().cursor().col().0;

    Motions::word_backward_big(editor.buffer_mut(), count);

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor().col().0;

    // Phase-15 debt: cursor cols are grapheme, yank_range needs char.
    let yanked = yank_range(
        editor,
        end_line,
        CharCol(end_col),
        start_line,
        CharCol(start_col),
    );
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        end_line,
        GraphemeCol(end_col),
        start_line,
        GraphemeCol(start_col.saturating_sub(1)),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(end_line, GraphemeCol(end_col));
    editor.clear_count();
    Ok(())
}

fn handle_y_big_e(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let start_col = editor.buffer().cursor().col().0;

    Motions::word_end_forward_big(editor.buffer_mut(), count);

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor().col().0;

    // Phase-15 debt: cursor cols are grapheme, yank_range needs char.
    let yanked = yank_range(
        editor,
        start_line,
        CharCol(start_col),
        end_line,
        CharCol(end_col + 1),
    );
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        start_line,
        GraphemeCol(start_col),
        end_line,
        GraphemeCol(end_col),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(start_line, GraphemeCol(start_col));
    editor.clear_count();
    Ok(())
}

fn handle_yh(editor: &mut Editor, count: usize) -> Result<()> {
    let line_idx = editor.buffer().cursor().line();
    let col = editor.buffer().cursor().col().0;
    if col == 0 {
        editor.clear_count();
        return Ok(());
    }
    let start_col = col.saturating_sub(count);
    // Cursor cols are grapheme; convert to char for yank_range (OV-00299).
    let line_text = editor
        .buffer()
        .line_text(line_idx)
        .unwrap_or_default()
        .to_string();
    let start_char = crate::unicode::grapheme_to_char_col(&line_text, GraphemeCol(start_col));
    let end_char = crate::unicode::grapheme_to_char_col(&line_text, GraphemeCol(col));
    let yanked = yank_range(editor, line_idx, start_char, line_idx, end_char);
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        line_idx,
        GraphemeCol(start_col),
        line_idx,
        GraphemeCol(col.saturating_sub(1)),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(line_idx, GraphemeCol(start_col));
    editor.clear_count();
    Ok(())
}

fn handle_y0(editor: &mut Editor) -> Result<()> {
    let line_idx = editor.buffer().cursor().line();
    let col = editor.buffer().cursor().col().0;
    if col == 0 {
        editor.clear_count();
        return Ok(());
    }
    // Cursor cols are grapheme; convert to char for yank_range (OV-00299).
    let end_char = editor.buffer().cursor_char_col();
    let yanked = yank_range(editor, line_idx, CharCol::ZERO, line_idx, end_char);
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        line_idx,
        GraphemeCol(0),
        line_idx,
        GraphemeCol(col.saturating_sub(1)),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(line_idx, GraphemeCol(0));
    editor.clear_count();
    Ok(())
}

fn handle_y_caret(editor: &mut Editor) -> Result<()> {
    let line_idx = editor.buffer().cursor().line();
    // Compare in char space: first_non_blank_col is char, so convert the
    // cursor's grapheme col rather than comparing across spaces (OV-00299).
    let cursor_char = editor.buffer().cursor_char_col();
    let fnb = editor.buffer().first_non_blank_col(line_idx);
    if fnb == cursor_char {
        editor.clear_count();
        return Ok(());
    }
    let (start, end): (CharCol, CharCol) = if fnb < cursor_char {
        (fnb, cursor_char)
    } else {
        (cursor_char, fnb)
    };
    let yanked = yank_range(editor, line_idx, start, line_idx, end);
    editor.yank_to_register(yanked);
    // Flash cols are grapheme-space: convert back per line.
    let line_text = editor
        .buffer()
        .line_text(line_idx)
        .unwrap_or_default()
        .to_string();
    let start_grapheme = crate::unicode::char_to_grapheme_col(&line_text, start);
    let end_grapheme = crate::unicode::char_to_grapheme_col(&line_text, end);
    editor.set_yank_flash_range(
        line_idx,
        start_grapheme,
        line_idx,
        GraphemeCol(end_grapheme.0.saturating_sub(1)),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(line_idx, start_grapheme);
    editor.clear_count();
    Ok(())
}

fn handle_yl(editor: &mut Editor, count: usize) -> Result<()> {
    let line_idx = editor.buffer().cursor().line();
    let col = editor.buffer().cursor().col().0;
    let line_len = editor
        .buffer()
        .line_text(line_idx)
        .map(|l| l.chars().count())
        .unwrap_or(0);
    if col >= line_len.saturating_sub(1) {
        // At or past last char — yank single char if on last char
        if col < line_len {
            let yanked = yank_range(editor, line_idx, CharCol(col), line_idx, CharCol(col + 1));
            editor.yank_to_register(yanked);
            editor.set_yank_flash_range(line_idx, GraphemeCol(col), line_idx, GraphemeCol(col));
        }
        editor.clear_count();
        return Ok(());
    }
    let end_col = (col + count).min(line_len);
    let yanked = yank_range(editor, line_idx, CharCol(col), line_idx, CharCol(end_col));
    editor.yank_to_register(yanked);
    editor.set_yank_flash_range(
        line_idx,
        GraphemeCol(col),
        line_idx,
        GraphemeCol(end_col.saturating_sub(1)),
    );
    editor.clear_count();
    Ok(())
}

fn handle_y_percent(editor: &mut Editor) -> Result<()> {
    use crate::editor::Motions;

    let buf = editor.buffer();
    let start_line = buf.cursor().line();
    let start_col = buf.cursor().col().0;
    let rope = buf.rope();

    // Build absolute position
    let mut abs_start = 0;
    for i in 0..start_line {
        if i < rope.len_lines() {
            abs_start += rope.line(i).len_chars();
        }
    }
    abs_start += start_col;

    let text = rope.to_string();
    let chars: Vec<char> = text.chars().collect();

    if abs_start >= chars.len() {
        editor.clear_count();
        return Ok(());
    }

    let current_char = chars[abs_start];
    let (is_opening, matching_char) = match current_char {
        '(' => (true, ')'),
        ')' => (false, '('),
        '[' => (true, ']'),
        ']' => (false, '['),
        '{' => (true, '}'),
        '}' => (false, '{'),
        '<' => (true, '>'),
        '>' => (false, '<'),
        _ => {
            editor.clear_count();
            return Ok(());
        }
    };

    let match_pos = if is_opening {
        Motions::find_matching_bracket_forward(&chars, abs_start, current_char, matching_char)
    } else {
        Motions::find_matching_bracket_backward(&chars, abs_start, matching_char, current_char)
    };

    let Some(abs_end) = match_pos else {
        editor.clear_count();
        return Ok(());
    };

    // Convert absolute positions to (line, col)
    let (lo, hi) = if abs_start < abs_end {
        (abs_start, abs_end)
    } else {
        (abs_end, abs_start)
    };

    let lo_line = rope.char_to_line(lo);
    let lo_col = lo - rope.line_to_char(lo_line);
    let hi_line = rope.char_to_line(hi);
    let hi_col = hi - rope.line_to_char(hi_line);
    // Include the matching bracket character itself
    let yanked = yank_range(
        editor,
        lo_line,
        CharCol(lo_col),
        hi_line,
        CharCol(hi_col + 1),
    );
    if yanked.contains('\n') {
        editor.yank_to_register_with_type(yanked, RegisterType::Line);
    } else {
        editor.yank_to_register(yanked);
    }
    editor.set_yank_flash_range(lo_line, GraphemeCol(lo_col), hi_line, GraphemeCol(hi_col));
    editor.clear_count();
    Ok(())
}

fn handle_y_big_w(editor: &mut Editor, count: usize) -> Result<()> {
    let start_line = editor.buffer().cursor().line();
    let start_col = editor.buffer().cursor().col().0;

    Motions::word_forward_big(editor.buffer_mut(), count);

    let end_line = editor.buffer().cursor().line();
    let end_col = editor.buffer().cursor().col().0;

    let yanked = yank_range(
        editor,
        start_line,
        CharCol(start_col),
        end_line,
        CharCol(end_col),
    );
    editor.yank_to_register(yanked);
    let flash_end_col = if end_col > 0 { end_col - 1 } else { 0 };
    editor.set_yank_flash_range(
        start_line,
        GraphemeCol(start_col),
        end_line,
        GraphemeCol(flash_end_col),
    );
    editor
        .buffer_mut()
        .cursor_mut()
        .set_position(start_line, GraphemeCol(start_col));
    editor.clear_count();
    Ok(())
}

/// Helper to yank a range of text without modifying the buffer.
fn yank_range(
    editor: &Editor,
    start_line: usize,
    start_col: CharCol,
    end_line: usize,
    end_col: CharCol,
) -> String {
    let buf = editor.buffer();
    let mut result = String::new();
    for line_idx in start_line..=end_line {
        if let Some(line) = buf.line_text(line_idx) {
            let chars: Vec<char> = line.chars().collect();
            let from = if line_idx == start_line {
                start_col.0
            } else {
                0
            };
            let to = if line_idx == end_line {
                end_col.0.min(chars.len())
            } else {
                chars.len()
            };
            if from < to {
                result.extend(&chars[from..to]);
            }
        }
    }
    result
}

// =====================================================================
// Change handlers for new motions (cb, ce, cB, cE, ch, c0, c^, cW)
// =====================================================================

fn handle_cb(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(editor, RepeatAction::DeleteWordBackward { count }, |buf| {
        buf.delete_word_backward(count)
    })
}

fn handle_ce(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(editor, RepeatAction::ChangeWordEnd { count }, |buf| {
        buf.change_word_end(count)
    })
}

fn handle_c_big_b(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(
        editor,
        RepeatAction::DeleteWordBackwardBig { count },
        |buf| buf.delete_word_backward_big(count),
    )
}

fn handle_c_big_e(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(editor, RepeatAction::ChangeWordEndBig { count }, |buf| {
        buf.change_word_end_big(count)
    })
}

fn handle_ch(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(editor, RepeatAction::DeleteCharLeft { count }, |buf| {
        buf.delete_char_left(count)
    })
}

fn handle_c0(editor: &mut Editor) -> Result<()> {
    change_with(editor, RepeatAction::DeleteToStartOfLine, |buf| {
        buf.delete_to_start_of_line()
    })
}

fn handle_c_caret(editor: &mut Editor) -> Result<()> {
    change_with(editor, RepeatAction::DeleteToFirstNonBlank, |buf| {
        buf.delete_to_first_non_blank()
    })
}

fn handle_c_percent(editor: &mut Editor) -> Result<()> {
    change_with(editor, RepeatAction::ChangeToMatchingBracket, |buf| {
        buf.change_to_matching_bracket()
    })
}

fn handle_c_big_w(editor: &mut Editor, count: usize) -> Result<()> {
    change_with(editor, RepeatAction::DeleteWordChangeBig { count }, |buf| {
        buf.change_word_forward_big(count)
    })
}
