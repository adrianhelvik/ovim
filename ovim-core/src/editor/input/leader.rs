//! Leader sequence handler for <Space>-prefixed commands.
//!
//! This module handles the Leader input state, processing commands like:
//! - `<Space>th` - Type hierarchy (LSP)
//! - `<Space>ca` - Code actions (LSP)
//! - `<Space>e` - Show diagnostic at cursor
//! - `<Space>o` - Document outline
//! - etc.

use crate::{KeyCode, KeyEvent};
use anyhow::Result;

use crate::editor::input_state::InputState;
use crate::editor::{Editor, Picker};
use crate::mode::Mode;

/// Handles input when in Leader state.
///
/// Called when the editor is in `InputState::Leader { keys }` state.
/// The `keys` vector contains any keys already pressed after the leader.
pub fn handle_leader_input(editor: &mut Editor, key: KeyEvent, keys: &[char]) -> Result<()> {
    // Handle Escape - cancel leader sequence
    if key.code == KeyCode::Esc {
        editor.reset_input_state();
        return Ok(());
    }

    let KeyCode::Char(c) = key.code else {
        // Non-character key - cancel
        editor.reset_input_state();
        return Ok(());
    };

    if keys.is_empty() {
        // First key after leader
        handle_first_leader_key(editor, c)
    } else {
        // Subsequent key in sequence
        handle_leader_sequence(editor, keys, c)
    }
}

/// Handles the first key after leader (<Space>).
fn handle_first_leader_key(editor: &mut Editor, key: char) -> Result<()> {
    match key {
        // Single-key commands
        'e' => {
            // <Space>e - Show diagnostic at cursor (like vim.diagnostic.open_float())
            editor.show_diagnostic_at_cursor();
            editor.reset_input_state();
        }
        'o' => {
            // <Space>o - Document outline (symbols)
            editor.request_document_symbols();
            editor.reset_input_state();
        }
        'S' => {
            // <Space>S - Workspace symbols
            editor.request_workspace_symbols();
            editor.reset_input_state();
        }
        'i' => {
            // <Space>i - Organize imports
            editor.request_organize_imports();
            editor.reset_input_state();
        }

        // AI chat commands
        ' ' => {
            // <Space><Space> - Open AI chat
            editor.open_ai_chat(crate::ai::chat_types::ChatOpts {
                name: "chat".into(),
                profile: editor.ai_chat_context_profile("chat"),
                allow_edits: true,
                ..Default::default()
            })?;
            editor.reset_input_state();
        }
        '?' => {
            // <Space>? - Open AI query (read-only)
            editor.open_ai_chat(crate::ai::chat_types::ChatOpts {
                name: "query".into(),
                profile: editor.ai_chat_context_profile("query"),
                allow_edits: false,
                ..Default::default()
            })?;
            editor.reset_input_state();
        }

        // Multi-key sequences - accumulate the key
        'd' => {
            // <Space>d... - Debug prefix
            editor.set_input_state(InputState::Leader { keys: vec!['d'] });
        }
        'l' => {
            // <Space>l... - LSP manager prefix
            editor.set_input_state(InputState::Leader { keys: vec!['l'] });
        }
        't' => {
            // <Space>t... - Tests (tn/tf/ta/ts/tl/tv/tt/to) + type hierarchy (th)
            editor.set_input_state(InputState::Leader { keys: vec!['t'] });
        }
        'c' => {
            // <Space>c... - Code actions/call hierarchy prefix
            editor.set_input_state(InputState::Leader { keys: vec!['c'] });
        }
        's' => {
            // <Space>s... - Search prefix
            editor.set_input_state(InputState::Leader { keys: vec!['s'] });
        }
        'g' => {
            // <Space>g... - Git prefix (gd diff review, gf fetch base)
            editor.set_input_state(InputState::Leader { keys: vec!['g'] });
        }

        // Unknown key - cancel sequence
        _ => {
            editor.reset_input_state();
        }
    }

    Ok(())
}

/// Handles subsequent keys in a leader sequence.
fn handle_leader_sequence(editor: &mut Editor, keys: &[char], next_key: char) -> Result<()> {
    match (keys, next_key) {
        // <Space>d... sequences (debug)
        (&['d'], 'b') => {
            // <Space>db - Toggle breakpoint at cursor line
            editor.toggle_breakpoint();
            editor.reset_input_state();
        }
        (&['d'], 'c') => {
            // <Space>dc - Continue (if active) or auto-start debug session
            if editor.is_debug_active() {
                editor.dap_manager_mut().pending_action =
                    Some(crate::dap::PendingDebugAction::Continue);
            } else {
                let dap_start = editor
                    .buffer()
                    .file_path()
                    .and_then(|fp| {
                        crate::language_config::LanguageRegistry::try_get()
                            .and_then(|reg| reg.detect(fp))
                    })
                    .and_then(|lang| lang.dap.as_ref())
                    .and_then(|config| {
                        crate::language_config::find_dap_command(config)
                            .map(|cmd| (cmd, config.args.clone()))
                    });
                if let Some((command, args)) = dap_start {
                    editor.dap_manager_mut().pending_action =
                        Some(crate::dap::PendingDebugAction::Start {
                            command,
                            args,
                            run_config: None,
                        });
                }
            }
            editor.reset_input_state();
        }
        (&['d'], 'n') => {
            // <Space>dn - Step over (next)
            if editor.is_debug_active() {
                editor.dap_manager_mut().pending_action =
                    Some(crate::dap::PendingDebugAction::StepOver);
            }
            editor.reset_input_state();
        }
        (&['d'], 'i') => {
            // <Space>di - Step into
            if editor.is_debug_active() {
                editor.dap_manager_mut().pending_action =
                    Some(crate::dap::PendingDebugAction::StepIn);
            }
            editor.reset_input_state();
        }
        (&['d'], 'o') => {
            // <Space>do - Step out
            if editor.is_debug_active() {
                editor.dap_manager_mut().pending_action =
                    Some(crate::dap::PendingDebugAction::StepOut);
            }
            editor.reset_input_state();
        }
        (&['d'], 's') => {
            // <Space>ds - Stop debug session
            if editor.is_debug_active() {
                editor.dap_manager_mut().pending_action =
                    Some(crate::dap::PendingDebugAction::Stop);
            }
            editor.reset_input_state();
        }
        (&['d'], 'v') => {
            // <Space>dv - Toggle debug panels visibility
            editor.toggle_debug_panels();
            editor.reset_input_state();
        }
        (&['d'], 'k') => {
            // <Space>dk - Select frame up (caller)
            if editor.is_debug_active() {
                editor.select_frame_up();
            }
            editor.reset_input_state();
        }
        (&['d'], 'j') => {
            // <Space>dj - Select frame down (callee)
            if editor.is_debug_active() {
                editor.select_frame_down();
            }
            editor.reset_input_state();
        }

        // <Space>l... sequences
        (&['l'], 'm') => {
            // <Space>lm - LSP Manager panel
            editor.open_lsp_manager();
            editor.reset_input_state();
        }

        // <Space>t... sequences
        (&['t'], 'h') => {
            // <Space>th - Type hierarchy
            editor.request_type_hierarchy();
            editor.reset_input_state();
        }
        (&['t'], 'f') => {
            // <Space>tf - Test file (run tests for current file)
            editor.run_test_file();
            editor.reset_input_state();
        }
        (&['t'], 'n') => {
            // <Space>tn - Test nearest (run test function at/above cursor)
            editor.run_test_nearest();
            editor.reset_input_state();
        }
        (&['t'], 'a') | (&['t'], 's') => {
            // <Space>ta / <Space>ts - Test suite (run full test suite)
            editor.run_test_all();
            editor.reset_input_state();
        }
        (&['t'], 'l') => {
            // <Space>tl - Test last (re-run last test command)
            editor.run_test_last();
            editor.reset_input_state();
        }
        (&['t'], 'v') => {
            // <Space>tv - Test visit (jump to last-tested file/position)
            editor.test_visit();
            editor.reset_input_state();
        }
        (&['t'], 't') => {
            // <Space>tt - Toggle the test panel
            editor.toggle_test_panel();
            editor.reset_input_state();
        }
        (&['t'], 'o') => {
            // <Space>to - Open raw test/make output in a scratch buffer
            editor.open_test_output_buffer();
            editor.reset_input_state();
        }

        // <Space>c... sequences
        (&['c'], 'a') => {
            // <Space>ca - Code actions
            editor.request_code_actions();
            editor.reset_input_state();
        }
        (&['c'], 'i') => {
            // <Space>ci - Incoming calls (call hierarchy)
            editor.request_call_hierarchy_incoming();
            editor.reset_input_state();
        }
        (&['c'], 'o') => {
            // <Space>co - Outgoing calls (call hierarchy)
            editor.request_call_hierarchy_outgoing();
            editor.reset_input_state();
        }

        // <Space>s... sequences
        (&['s'], 'f') => {
            // <Space>sf - Find files
            let (base_dir, preferred_dir) = editor.picker_dirs();
            let picker = Picker::new_file_finder(base_dir, preferred_dir);
            editor.set_picker(picker);
            editor.set_mode(Mode::Picker);
            editor.mark_picker_selection_changed();
            editor.reset_input_state();
        }
        (&['s'], 'g') => {
            // <Space>sg - Live grep
            let (base_dir, preferred_dir) = editor.picker_dirs();
            let picker = Picker::new_live_grep(base_dir, preferred_dir);
            editor.set_picker(picker);
            editor.set_mode(Mode::Picker);
            editor.reset_input_state();
        }

        // <Space>g... sequences (git)
        (&['g'], 'd') => {
            // <Space>gd - Toggle the branch diff review
            editor.toggle_diff_review();
            editor.reset_input_state();
        }
        (&['g'], 'f') => {
            // <Space>gf - Fetch the review base branch
            editor.fetch_review_base();
            editor.reset_input_state();
        }

        // Unknown sequence - cancel
        _ => {
            editor.reset_input_state();
        }
    }

    Ok(())
}
