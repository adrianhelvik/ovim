//! Insert mode handler
//!
//! Handles all input events in Insert mode including:
//! - Character insertion
//! - Backspace/Delete handling
//! - Ctrl+W (delete word backward)
//! - Ctrl+U (delete to line start)
//! - Ctrl+N/Ctrl+P (completion navigation)
//! - Visual block insert state handling
//! - Tab/auto-indent

use crate::editor::{Change, Editor, InsertEntryMode};
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::{CharCol, GraphemeCol};
use crate::{KeyCode, KeyEvent, Modifiers};
use anyhow::Result;

use super::helpers;

fn is_completion_trigger_char(c: char) -> bool {
    matches!(c, '.')
}

// Looser than the motion-word rule on purpose: hyphens count so Tailwind
// classes like `w-1/2` stay as one prefix and the menu doesn't collapse
// mid-token. Keep in sync with `is_completion_keyword_char` in
// `ovim-core/src/editor/lsp_modules/completion.rs`.
fn is_completion_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Cleans up whitespace-only lines before exiting insert mode.
///
/// Vim behavior: if the current line contains only whitespace when exiting insert mode,
/// remove the whitespace (e.g., o<Esc> should leave an empty line, not an indented one).
///
/// This must be called BEFORE finalize_change_building() so it's part of the undo group.
///
/// Returns true if cleanup was performed (which means cursor shouldn't move left).
fn cleanup_whitespace_only_line(editor: &mut Editor) -> bool {
    let current_line_idx = editor.buffer().cursor().line();
    if let Some(line) = editor.buffer().line_text(current_line_idx) {
        let line_without_newline = line;
        // Check if line is non-empty but only whitespace
        if !line_without_newline.is_empty()
            && line_without_newline.chars().all(|c| c.is_whitespace())
        {
            // Delete the whitespace, leaving just the newline.
            // Whitespace is ASCII, so char count == grapheme count here.
            let whitespace_len = line_without_newline.chars().count();

            // Record the deletion for undo. `delete_range_positioning_cursor`
            // lands the cursor at char col 0 (== grapheme col 0).
            if !editor.record_session_edit(|buf| {
                buf.delete_range_positioning_cursor(
                    current_line_idx,
                    CharCol::ZERO,
                    current_line_idx,
                    CharCol(whitespace_len),
                )
                .0
            }) {
                return false;
            }
            return true;
        }
    }
    false
}

/// Shared logic for exiting insert mode (Esc, Ctrl-[, Ctrl-C)
fn exit_insert_mode(editor: &mut Editor) {
    // Save last insert position BEFORE moving cursor (this is where we can continue inserting)
    let cursor = editor.buffer().cursor();
    editor.editing.last_insert_position = Some((cursor.line(), cursor.col().0));

    // Cleanup whitespace-only lines before finalizing changes
    cleanup_whitespace_only_line(editor);

    // Track whether finalize actually pushed an insert-mode undo entry.
    // For cases like `cw<Esc>`/`C<Esc>` where no text was typed, finalize
    // pushes nothing and we must not pop unrelated history.
    let undo_len_before_finalize = editor.buffer().change_manager().undo_stack.len();
    editor.finalize_change_building();
    let insert_change_pushed =
        editor.buffer().change_manager().undo_stack.len() > undo_len_before_finalize;

    // Check for pending change repeat (cc, C, s, S, cj, ck, cw, cgn, etc.)
    if let Some(pending) = editor.take_pending_change_repeat() {
        // Pop the insert session's `Recorded` only if it pushed one.
        let insert_undo = if insert_change_pushed {
            editor.pop_last_change()
        } else {
            None
        };
        let inserted_text = insert_undo
            .as_ref()
            .map(|c| c.get_inserted_text())
            .unwrap_or_default();
        let insert_cursor_before = insert_undo.as_ref().map(|c| c.cursor_before());
        let insert_edits = insert_undo.and_then(|c| c.into_edits()).unwrap_or_default();

        // Pop delete undo only if the delete phase actually produced edits.
        let delete_undo = pending
            .delete_token
            .and_then(|token| editor.pop_by_token(token));

        let cursor_before = delete_undo
            .as_ref()
            .map(|c| c.cursor_before())
            .or(insert_cursor_before)
            .unwrap_or_else(|| editor.cursor_position());
        let cursor_after = editor.cursor_position();

        let delete_edits = delete_undo.and_then(|c| c.into_edits()).unwrap_or_default();
        let mut merged = delete_edits;
        merged.extend(insert_edits);
        if !merged.is_empty() {
            editor
                .buffer_mut()
                .change_manager_mut()
                .push_change(Change::recorded(merged, cursor_before, cursor_after));
        }

        // Set semantic repeat action
        editor.set_repeat_action(RepeatAction::Change {
            delete: Box::new(pending.delete_action),
            inserted_text,
            linewise: pending.linewise,
        });
    }

    // For o/O insert sessions, promote dot-repeat to RepeatAction::OpenLine
    // so the replay opens a new line at the current cursor instead of
    // replaying the original session's newline-insert edit verbatim.
    let open_line_repeat = match editor.buffer().change_manager().last_repeat_action.as_ref() {
        Some(RepeatAction::InsertSession {
            entry_mode: mode @ (InsertEntryMode::OpenBelow | InsertEntryMode::OpenAbove),
            edits,
            ..
        }) => {
            // Skip the first edit — that's the synthetic newline created by
            // `insert_line_below` / `insert_line_above` before the user's
            // keystrokes. `RepeatAction::OpenLine` will recreate its own.
            let inserted_text: String = edits
                .iter()
                .skip(1)
                .filter_map(|e| match e {
                    crate::edit::Edit::Insert { text, .. } => Some(text.as_str()),
                    crate::edit::Edit::Delete { .. } => None,
                })
                .collect();
            Some(RepeatAction::OpenLine {
                above: matches!(mode, InsertEntryMode::OpenAbove),
                inserted_text,
                options: editor.options.indent_options(),
            })
        }
        _ => None,
    };

    // Update the . register with the last inserted text
    editor.update_last_inserted_register();
    if let Some(action) = open_line_repeat {
        editor.set_repeat_action(action);
    }

    // If we were in visual block insert/append mode, replay the changes on all other lines.
    // For visual-block change (`Ctrl-V ... c ...`), also capture a semantic repeat template.
    let pending_visual_block_change = editor.take_pending_visual_block_change_repeat();
    let pending_visual_block_delete_token = editor
        .editing
        .pending_visual_block_change_delete_token
        .take();
    let mut visual_block_change_inserted_text: Option<String> = None;
    let should_move_to_end_line = if let Some((start_line, end_line, col, is_append, move_to_end)) =
        editor.visual_block_insert_state()
    {
        // Pull the first-line session's `Recorded` so we can extend its
        // edits with the replays on sibling lines and push the combined
        // result as a single undo entry.
        if let Some(last_change) = editor.last_change().cloned() {
            let inserted_text = last_change.get_inserted_text();
            if !is_append && !move_to_end {
                visual_block_change_inserted_text = Some(inserted_text.clone());
            }
            let cursor_before = last_change.cursor_before();
            let mut all_edits: Vec<crate::edit::Edit> =
                last_change.into_edits().unwrap_or_default();

            // `$A` (block append to end-of-line) appends at each line's EOL;
            // a plain `A` appends at the fixed block column on every line.
            // Consume the flag here since this path doesn't route through
            // exit_visual_mode_to_normal (which is where it normally resets).
            let block_dollar = editor.visual_block_dollar();
            editor.set_visual_block_dollar(false);

            // Replay the typed text on each sibling line inside a record()
            // session so the edits are captured (and the edit_log populated)
            // without the caller having to track each insert_text_at manually.
            let ((), sibling_edits) = editor.buffer_mut().record(|buf| {
                for line_idx in (start_line + 1)..=end_line {
                    if is_append && block_dollar {
                        // `$A`: append at end of each line.
                        if let Some(line) = buf.line_text(line_idx) {
                            let line_len = line.chars().count();
                            buf.insert_text_at(line_idx, CharCol(line_len), &inserted_text);
                        }
                    } else if is_append {
                        // `A`: append at the block append column, padding short
                        // lines so the column lines up (matching Vim).
                        if let Some(line) = buf.line_text(line_idx) {
                            let line_len = line.chars().count();
                            if col <= line_len {
                                buf.insert_text_at(line_idx, CharCol(col), &inserted_text);
                            } else {
                                let padding = " ".repeat(col - line_len);
                                let padded = format!("{padding}{inserted_text}");
                                buf.insert_text_at(line_idx, CharCol(line_len), &padded);
                            }
                        }
                    } else {
                        // Insert mode: insert at the block column (`col` is
                        // grapheme-space from visual-block state — pre-existing
                        // Class-2 assumption that equals char-space for ASCII).
                        if let Some(line_text) = buf.line_text(line_idx) {
                            let insert_col = col.min(line_text.chars().count());
                            buf.insert_text_at(line_idx, CharCol(insert_col), &inserted_text);
                        }
                    }
                }
            });

            // If sibling lines produced edits, rewrite the undo entry to
            // contain all of them. The first-line Recorded we popped above
            // stands in for the whole visual-block insert/append.
            if !sibling_edits.is_empty() {
                editor.pop_last_change();
                all_edits.extend(sibling_edits);

                // Visual-block change (`c`) has a preceding delete Recorded.
                // Redeem it by token so we never pop unrelated history.
                if let Some(token) = pending_visual_block_delete_token {
                    if let Some(prev_change) = editor.pop_by_token(token) {
                        let mut merged = prev_change.into_edits().unwrap_or_default();
                        merged.extend(all_edits);
                        all_edits = merged;
                    }
                }

                let cursor_after = editor.cursor_position();
                editor
                    .buffer_mut()
                    .change_manager_mut()
                    .push_change(Change::recorded(all_edits, cursor_before, cursor_after));
            }
        }

        // Clear the visual block insert state
        editor.set_visual_block_insert_state(None);
        Some((start_line, end_line, col, is_append, move_to_end))
    } else {
        None
    };

    if let (Some((line_count, width)), Some(inserted_text)) = (
        pending_visual_block_change,
        visual_block_change_inserted_text,
    ) {
        editor.set_repeat_action(RepeatAction::ChangeVisualBlock {
            line_count,
            width,
            inserted_text,
        });
    }

    // Mark buffer modified for LSP didChange — placed after visual block replay
    // so the server sees ALL changes (first line + replayed lines). The
    // sibling replay is wrapped in `buffer.record()` so `edit_log` already
    // includes its edits — no fixup needed.
    editor.mark_buffer_modified();

    // Clear insert-normal flag on full exit
    editor.editing.insert_normal_pending = false;

    editor.set_mode(Mode::Normal);

    // Move cursor left when exiting insert mode (unless at column 0)

    // If we were in visual block mode, move cursor to appropriate line
    if let Some((start_line, end_line, _col, is_append, move_to_end)) = should_move_to_end_line {
        // For visual block, calculate the correct final cursor position
        let target_line = if move_to_end { end_line } else { start_line };

        if is_append {
            // For append mode, position cursor on the last character of target line
            if let Some(line_text) = editor.buffer().line_text(target_line) {
                let line_len = line_text.chars().count();
                let final_col = if line_len > 0 { line_len - 1 } else { 0 };
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(target_line, GraphemeCol(final_col));
            }
        } else {
            // For insert mode, use the same column as on the first line
            let cursor = editor.buffer().cursor();
            let current_col = cursor.col().0;
            let inserted_col = if current_col > 0 { current_col - 1 } else { 0 };
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(target_line, GraphemeCol(inserted_col));
        }
    } else {
        let cursor = editor.buffer_mut().cursor_mut();
        if cursor.col().0 > 0 {
            cursor.move_left(1);
        }
    }
}

/// Handles input in Insert mode
pub fn handle_insert_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    // Handle pending register insert (Ctrl-R {reg})
    if editor.editing.pending_register_insert {
        editor.editing.pending_register_insert = false;
        if let KeyCode::Char(c) = key_event.code {
            let text = editor.registers().get(Some(c));
            if !text.is_empty() {
                for ch in text.chars() {
                    if ch == '\n' {
                        helpers::insert_newline(editor)?;
                    } else {
                        helpers::insert_char(editor, ch)?;
                    }
                }
            }
        }
        return Ok(());
    }

    match key_event.code {
        KeyCode::Esc => {
            if editor.completion_menu().is_visible() {
                editor.hide_completion_menu();
            }
            exit_insert_mode(editor);
        }
        // Ctrl-[ is equivalent to Esc
        KeyCode::Char('[') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.completion_menu().is_visible() {
                editor.hide_completion_menu();
            }
            exit_insert_mode(editor);
        }
        // Ctrl-C exits insert mode (like Esc but without triggering InsertLeave)
        KeyCode::Char('c') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.completion_menu().is_visible() {
                editor.hide_completion_menu();
            }
            exit_insert_mode(editor);
        }
        // Ctrl-W - Delete word backward
        KeyCode::Char('w') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            helpers::delete_word_backward_insert(editor)?;
        }
        // Ctrl-U - Delete to start of line
        KeyCode::Char('u') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            helpers::delete_to_line_start_insert(editor)?;
        }
        // Ctrl-T - Indent current line in insert mode
        KeyCode::Char('t') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            helpers::indent_line_insert(editor)?;
        }
        // Ctrl-D - Dedent current line in insert mode
        KeyCode::Char('d') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            helpers::dedent_line_insert(editor)?;
        }
        // Ctrl-H is equivalent to Backspace
        KeyCode::Char('h') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            helpers::delete_char_before_cursor(editor)?;
        }
        // Ctrl-R - Insert register contents
        KeyCode::Char('r') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.editing.pending_register_insert = true;
        }
        // Ctrl-Space - Request code completion
        KeyCode::Char(' ') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.request_completion();
        }
        // Ctrl-O - Execute one normal mode command, then return to insert
        KeyCode::Char('o') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.editing.insert_normal_pending = true;
            editor.set_mode(Mode::Normal);
        }
        // Ctrl-N - Next completion item
        KeyCode::Char('n') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.completion_menu().is_visible() {
                editor.completion_next();
            }
        }
        // Ctrl-P - Previous completion item
        KeyCode::Char('p') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.completion_menu().is_visible() {
                editor.completion_previous();
            }
        }
        // Ctrl-Y - Accept completion (Vim behavior)
        KeyCode::Char('y') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if editor.completion_menu().is_visible() {
                editor.accept_completion();
            }
        }
        // Tab - Accept completion if menu is visible, otherwise insert tab
        KeyCode::Tab if editor.completion_menu().is_visible() => {
            editor.accept_completion();
        }
        KeyCode::Tab => {
            helpers::insert_tab(editor)?;
        }
        KeyCode::Char(c) => {
            helpers::electric_dedent_close_bracket(editor, c)?;
            helpers::insert_char(editor, c)?;
            // Basic autocomplete:
            // - Trigger on '.' (member access) immediately
            // - Trigger on '::' after typing the second ':' (Rust/C++ style paths)
            // - Trigger when typing an identifier prefix of length >= 2
            // - If menu is already visible, keep it updated while typing
            if editor.completion_menu().is_visible() {
                if is_completion_trigger_char(c) || is_completion_ident_char(c) {
                    let prefix = editor.completion_prefix_from_trigger_col();
                    editor.completion_menu_mut().filter(&prefix);
                    editor.request_completion();
                } else {
                    editor.hide_completion_menu();
                }
            } else if is_completion_trigger_char(c) {
                editor.request_completion();
            } else if c == ':' {
                let cursor = editor.buffer().cursor();
                if cursor.col().0 >= 2 {
                    let line_text = editor
                        .buffer()
                        .line_text(cursor.line())
                        .unwrap_or_default()
                        .to_string();
                    if crate::unicode::grapheme_at_index(
                        &line_text,
                        cursor.col().0.saturating_sub(1),
                    ) == Some(":")
                        && crate::unicode::grapheme_at_index(
                            &line_text,
                            cursor.col().0.saturating_sub(2),
                        ) == Some(":")
                    {
                        editor.request_completion();
                    }
                }
            } else if is_completion_ident_char(c) {
                let (_, prefix) = editor.completion_trigger_context();
                if prefix.chars().count() >= 2 {
                    editor.request_completion();
                }
            }
        }
        KeyCode::Enter => {
            // If completion menu is visible, accept the selected completion
            if editor.completion_menu().is_visible() {
                editor.accept_completion();
            } else {
                helpers::insert_newline(editor)?;
            }
        }
        KeyCode::Backspace => {
            helpers::delete_char_before_cursor(editor)?;
            if editor.completion_menu().is_visible() {
                let prefix = editor.completion_prefix_from_trigger_col();
                if prefix.is_empty() {
                    // If we deleted back to the trigger column, keep showing member completions only
                    editor.request_completion();
                } else {
                    editor.completion_menu_mut().filter(&prefix);
                    editor.request_completion();
                }
            }
        }
        KeyCode::Left => {
            if editor.completion_menu().is_visible() {
                editor.hide_completion_menu();
            }
            let cursor = editor.buffer_mut().cursor_mut();
            if cursor.col().0 > 0 {
                cursor.move_left(1);
            }
        }
        KeyCode::Right => {
            if editor.completion_menu().is_visible() {
                editor.hide_completion_menu();
            }
            helpers::move_right(editor);
        }
        KeyCode::Up => {
            if editor.completion_menu().is_visible() {
                editor.completion_previous();
            } else {
                helpers::move_up(editor);
            }
        }
        KeyCode::Down => {
            if editor.completion_menu().is_visible() {
                editor.completion_next();
            } else {
                helpers::move_down(editor);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{ApplyPos, CursorPos, PendingChangeRepeat};

    #[test]
    fn exit_insert_mode_pending_change_repeat_no_insert_no_delete_keeps_prior_undo() {
        let mut editor = Editor::with_content("line\n");

        // Seed history so an accidental pop/replace is observable. Opens a
        // throwaway session around the seed edit because `record_session_edit`
        // requires an active recording session post-Signal-A cleanup.
        let cursor = editor.cursor_position();
        let apply = ApplyPos::new(cursor.line, CharCol(cursor.col.0));
        editor.start_change_building(cursor);
        assert!(editor.record_session_edit(|buf| {
            buf.insert_text_at_positioning_cursor(apply.line, apply.col, "X")
        }));
        editor.finalize_change_building();
        let undo_len_before = editor.buffer().change_manager().undo_stack.len();

        // Simulate a no-op change operator (e.g., C at EOL) entering insert mode,
        // then immediate <Esc> (no delete edits + no insert edits).
        editor.set_pending_change_repeat(PendingChangeRepeat {
            delete_action: RepeatAction::DeleteToEndOfLine,
            linewise: false,
            delete_token: None,
        });
        editor.start_change_building(CursorPos::ZERO);
        editor.set_mode(Mode::Insert);

        exit_insert_mode(&mut editor);

        let undo_stack = &editor.buffer().change_manager().undo_stack;
        assert_eq!(undo_stack.len(), undo_len_before);
        // After step 4.3 the direct-path push is a `Recorded`, not `InsertText`.
        assert!(matches!(
            undo_stack.last().map(|entry| &entry.change),
            Some(Change::Recorded { .. })
        ));
    }
}
