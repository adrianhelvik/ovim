//! Normal mode input handling.
//!
//! This module dispatches normal mode key events to specialized handlers.
//! Each handler returns `Result<bool>`:
//! - `Ok(true)` - Key was handled, stop dispatching
//! - `Ok(false)` - Key was not handled, try next handler
//! - `Err(_)` - Error occurred

mod editing_commands;
mod mode_transitions;
mod motions_input;
mod operators;
mod pending_commands;
mod text_objects;

use crate::editor::Editor;
use crate::{KeyCode, KeyEvent};
use anyhow::Result;

/// Handle a key event in normal mode.
///
/// This dispatcher tries each handler in priority order until one handles the key.
pub fn handle_normal_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    // The test panel is passive rather than focused, so Escape must hide it
    // even when a pending operator or multi-key command consumes the key.
    if key_event.code == KeyCode::Esc {
        editor.close_test_panel();
    }

    // 1. Try pending operators (dd, dw, yy, etc.)
    if operators::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 2. Try text objects after operator (diw, ci", etc.)
    if text_objects::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 3. Try pending commands (g*, z*, m*, etc.)
    if pending_commands::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 4. Try mode transitions (i, a, v, :, etc.)
    if mode_transitions::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 5. Try editing commands (x, D, p, etc.)
    if editing_commands::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 6. Try motions (h, j, k, l, w, b, etc.)
    if motions_input::try_handle(editor, key_event)? {
        return Ok(());
    }

    // 7. Set up operators and pending commands for multi-key sequences
    if setup_pending_state(editor, key_event)? {
        return Ok(());
    }

    // Clear count on unrecognized key
    editor.clear_count();
    Ok(())
}

/// Set up pending operators or commands for multi-key sequences.
fn setup_pending_state(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    use crate::editor::{CharMotion, InputState, Operator};
    use crate::KeyCode;

    match key_event.code {
        // Operators
        KeyCode::Char('d') => {
            editor.set_pending_operator(Operator::Delete);
            Ok(true)
        }
        KeyCode::Char('y') => {
            editor.set_pending_operator(Operator::Yank);
            Ok(true)
        }
        KeyCode::Char('c') => {
            editor.set_pending_operator(Operator::Change);
            Ok(true)
        }
        KeyCode::Char('>') => {
            editor.set_pending_operator(Operator::Indent);
            Ok(true)
        }
        KeyCode::Char('<') => {
            editor.set_pending_operator(Operator::Dedent);
            Ok(true)
        }
        KeyCode::Char('=') => {
            editor.set_pending_operator(Operator::AutoIndent);
            Ok(true)
        }
        // Pending commands
        KeyCode::Char('g') => {
            editor.set_pending_command('g');
            Ok(true)
        }
        KeyCode::Char('z') => {
            editor.set_pending_command('z');
            Ok(true)
        }
        KeyCode::Char('Z') => {
            editor.set_pending_command('Z');
            Ok(true)
        }
        KeyCode::Char('[') => {
            editor.set_pending_command('[');
            Ok(true)
        }
        KeyCode::Char(']') => {
            editor.set_pending_command(']');
            Ok(true)
        }
        KeyCode::Char('"') => {
            editor.set_pending_command('"');
            Ok(true)
        }
        KeyCode::Char('m') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Mark,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('\'') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::JumpMarkLine,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('`') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::JumpMarkExact,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('q') => {
            if editor.is_recording_macro() {
                editor.stop_macro_recording();
            } else {
                editor.set_pending_command('q');
            }
            Ok(true)
        }
        KeyCode::Char('@') => {
            editor.set_pending_command('@');
            Ok(true)
        }
        KeyCode::Char('r') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Replace,
                operator: None,
            });
            Ok(true)
        }
        // Character motions - use new state machine
        KeyCode::Char('f') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Find,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('F') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::FindBack,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('t') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::Till,
                operator: None,
            });
            Ok(true)
        }
        KeyCode::Char('T') => {
            editor.set_input_state(InputState::AwaitingChar {
                motion: CharMotion::TillBack,
                operator: None,
            });
            Ok(true)
        }
        // Leader key (configurable via vim.g.mapleader, default: space)
        KeyCode::Char(c) if c == editor.leader_key() => {
            editor.set_input_state(InputState::Leader { keys: vec![] });
            Ok(true)
        }
        _ => Ok(false),
    }
}
