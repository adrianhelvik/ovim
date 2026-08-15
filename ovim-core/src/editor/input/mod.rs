use crate::editor::{Editor, InputState, MapMode};
use crate::mode::Mode;
use crate::{KeyCode, KeyEvent, Modifiers};
use anyhow::Result;

/// Command handling submodule
mod commands;

/// Shell command expansion (%, #, modifiers)
pub mod shell_expansion;

/// Number operations (Ctrl-A, Ctrl-X, g Ctrl-A, g Ctrl-X)
mod numbers;

/// Case operations (toggle, upper, lower)
mod case;

/// Helper functions for cursor movement and editing
pub(crate) mod helpers;

/// Character motion handler (f, t, F, T, r, m, ', `) - new state machine
mod char_motion;

/// Leader sequence handler (<Space>...) - new state machine
mod leader;

/// Search mode handler (/, ?)
mod search_mode;

/// Replace mode handler (R)
mod replace_mode;

/// Picker mode handler (file finder, grep, code actions)
mod picker_mode;

/// Hover mode handlers (preview and navigate)
mod hover_mode;

/// File tree mode handler
mod filetree_mode;

/// Substitute confirm mode handler
mod substitute_mode;

/// Dashboard mode handler
mod dashboard_mode;

/// LSP Manager mode handler
mod lsp_manager_mode;

/// Rename input mode handler
mod rename_input_mode;

/// AI chat mode handler
mod ai_chat_mode;

/// Modal code-walkthrough input policy
mod code_explanation_mode;

/// Mouse event handler (click, drag, scroll)
pub mod mouse;

/// Insert mode handler
mod insert_mode;

/// Visual mode handler (Visual, VisualLine, VisualBlock)
mod visual_mode;

/// Normal mode handler (decomposed into submodules)
mod normal;

/// Handles input events for the editor
pub struct InputHandler;

const MAX_MAPPING_REMAP_DEPTH: usize = 32;

impl InputHandler {
    /// Processes a keyboard event and marks the editor dirty.
    /// Use this for single-event callers that want automatic dirty marking.
    pub fn handle_key_event(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
        Self::handle_key_event_no_dirty(editor, key_event)?;
        editor.mark_dirty();
        Ok(())
    }

    /// Processes a keyboard event without marking the editor dirty.
    /// Use this for batch processing where dirty should be marked once at the end.
    pub fn handle_key_event_no_dirty(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
        Self::handle_key_event_internal(editor, key_event, true, true, 0)
    }

    fn handle_key_event_internal(
        editor: &mut Editor,
        key_event: KeyEvent,
        allow_remap: bool,
        record_macro: bool,
        remap_depth: usize,
    ) -> Result<()> {
        // Record the event if we're recording a macro (but don't record the 'q'
        // that stops recording). A 'q' only terminates recording when it reaches
        // the terminator branch — i.e. Normal mode with no pending
        // operator/command/register/input state. A 'q' typed as an *argument*
        // (fq, rq, mq, `q, "q, @q, …) must still be recorded, otherwise replaying
        // the macro silently drops it.
        let is_recording_terminator = key_event.code == KeyCode::Char('q')
            && editor.mode() == Mode::Normal
            && matches!(editor.input_state(), InputState::Normal)
            && editor.pending_operator().is_none()
            && editor.pending_command().is_none()
            && editor.pending_register().is_none();
        let should_record_macro =
            record_macro && editor.is_recording_macro() && !is_recording_terminator;

        if should_record_macro {
            editor.record_macro_event(key_event);
        }

        // Intercept input when LSP install consent dialog is showing
        if editor.has_pending_lsp_install() {
            match key_event.code {
                KeyCode::Enter => {
                    // Approve install (once)
                    editor.resolve_pending_lsp_install(crate::editor::LspInstallConsent::Yes);
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    // Always auto-install (sets autoinstall=auto)
                    editor.resolve_pending_lsp_install(crate::editor::LspInstallConsent::Always);
                }
                KeyCode::Esc => {
                    // Skip install
                    editor.resolve_pending_lsp_install(crate::editor::LspInstallConsent::No);
                }
                _ => {} // Ignore other keys while dialog is showing
            }
            return Ok(());
        }

        // Ovim-owned Codex sign-in is global: it can block both chat and
        // selection inference, regardless of the editor mode underneath.
        if editor.has_codex_auth_dialog() {
            editor.handle_codex_auth_key(key_event);
            return Ok(());
        }

        code_explanation_mode::restore_owning_mode(editor);

        // Global keybindings (work in any mode)
        // Cmd+1 - toggle file tree
        if key_event.code == KeyCode::Char('1') && key_event.modifiers.contains(Modifiers::SUPER) {
            editor.toggle_file_tree();
            return Ok(());
        }

        let mapping_handled = if allow_remap {
            Self::try_handle_mode_mapping(editor, key_event, remap_depth)?
        } else {
            false
        };

        let result = if mapping_handled {
            Ok(())
        } else {
            match editor.mode() {
                Mode::Normal => Self::handle_normal_mode(editor, key_event),
                Mode::Insert => insert_mode::handle_insert_mode(editor, key_event),
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                    visual_mode::handle_visual_mode(editor, key_event)
                }
                Mode::Command => commands::handle_command_mode(editor, key_event),
                Mode::Search => search_mode::handle_search_mode(editor, key_event),
                Mode::Replace => replace_mode::handle_replace_mode(editor, key_event),
                Mode::Picker => picker_mode::handle_picker_mode(editor, key_event),
                Mode::HoverPreview => {
                    // HoverPreview may forward keys to normal mode
                    if let Some(forwarded_key) =
                        hover_mode::handle_hover_preview_mode(editor, key_event)?
                    {
                        Self::handle_normal_mode(editor, forwarded_key)?;
                    }
                    Ok(())
                }
                Mode::HoverNavigate => hover_mode::handle_hover_navigate_mode(editor, key_event),
                Mode::FileTree => filetree_mode::handle_filetree_mode(editor, key_event),
                Mode::SubstituteConfirm => {
                    substitute_mode::handle_substitute_confirm_mode(editor, key_event)
                }
                Mode::Dashboard => dashboard_mode::handle_dashboard_mode(editor, key_event),
                Mode::LspManager => lsp_manager_mode::handle_lsp_manager_mode(editor, key_event),
                Mode::RenameInput => rename_input_mode::handle_rename_input_mode(editor, key_event),
                Mode::AiChat => ai_chat_mode::handle_ai_chat_mode(editor, key_event),
            }
        };

        // Ctrl-O insert-normal: after one normal command, return to insert mode.
        // Only return if we're still in Normal mode (the command didn't switch to
        // Insert, Visual, Command, etc. on its own) and no pending multi-key state.
        if editor.editing.insert_normal_pending && editor.mode() == Mode::Normal {
            // Check if the command is fully resolved (no pending operator/command)
            if editor.pending_operator().is_none()
                && editor.pending_command().is_none()
                && matches!(editor.input_state(), InputState::Normal)
            {
                editor.editing.insert_normal_pending = false;
                editor.set_mode(Mode::Insert);
            }
        }

        // Update scroll offset to keep cursor visible with scrolloff margin
        // Skip if:
        // 1. Viewport commands (zz, zt, zb) explicitly set scroll position
        // 2. There's a pending viewport command (e.g., 'z' waiting for 't')
        //    This prevents scroll changes between multi-key sequences like 'zt'
        // When the mapping layer handled this key it already ran the scroll
        // update on the inner (replayed) dispatch, consuming the one-shot viewport policy.
        // Re-running it here would scroll a second time and, worse, undo a
        // deliberate sub-row scroll (e.g. Ctrl-E) whose cursor sits off-screen by
        // design. Only run the post-command scroll update at this (outer) level
        // when we handled the key directly.
        let is_viewport_pending = matches!(editor.pending_command(), Some('z') | Some('Z'));
        let preserve_viewport = editor.viewport.take_preserve_after_input();
        if !preserve_viewport && !is_viewport_pending && !mapping_handled {
            editor.update_scroll_offset();
        }

        // Safety net: ensure cursor is within buffer bounds after every key event.
        // Individual motions/operators should maintain this invariant themselves, but
        // this catch-all prevents any cursor-out-of-bounds state from persisting.
        //
        // Skip in Insert/Replace modes: `validate_cursor_position` uses Normal mode
        // semantics (cursor must be ON a character), but Insert mode legitimately
        // allows cursor at `line_len` (the append position, e.g. after `A`).
        //
        // VisualBlock also legitimately allows the cursor beyond a short line's
        // end (the block column). Clamping the column there collapses the block
        // over short lines and corrupts a subsequent block delete/yank, so only
        // clamp the line for VisualBlock and leave the column (and its goal)
        // intact.
        match editor.mode() {
            Mode::Insert | Mode::Replace => {}
            Mode::VisualBlock => editor.buffer_mut().validate_cursor_line(),
            _ => editor.buffer_mut().validate_cursor_position(),
        }

        result
    }

    fn active_mapping_mode(editor: &Editor) -> Option<MapMode> {
        match editor.mode() {
            Mode::Normal => Some(MapMode::Normal),
            Mode::Dashboard => Some(MapMode::Normal),
            Mode::Insert => Some(MapMode::Insert),
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => Some(MapMode::Visual),
            Mode::Command => Some(MapMode::Command),
            _ => None,
        }
    }

    fn is_mapping_context(editor: &Editor) -> bool {
        editor.pending_mapping_sequence().is_empty()
            && editor.count().is_none()
            && editor.pending_operator().is_none()
            && editor.pending_command().is_none()
            && editor.pending_register().is_none()
            && matches!(editor.input_state(), InputState::Normal)
    }

    fn try_handle_mode_mapping(
        editor: &mut Editor,
        key_event: KeyEvent,
        remap_depth: usize,
    ) -> Result<bool> {
        let Some(map_mode) = Self::active_mapping_mode(editor) else {
            editor.clear_pending_mapping();
            return Ok(false);
        };

        if !editor.has_pending_mapping() && !Self::is_mapping_context(editor) {
            return Ok(false);
        }

        let Some(encoded_key) = Self::encode_key_for_mapping_lookup(key_event) else {
            if editor.has_pending_mapping() {
                let mut replay = editor.take_pending_mapping_events();
                replay.push(key_event);
                for event in replay {
                    Self::handle_key_event_internal(editor, event, false, false, remap_depth)?;
                }
                return Ok(true);
            }
            return Ok(false);
        };

        editor.append_pending_mapping(&encoded_key, key_event);
        let sequence = editor.pending_mapping_sequence().to_string();

        if let Some(mapping) = editor.keymaps().get_mapping(map_mode, &sequence).cloned() {
            editor.clear_pending_mapping();
            if remap_depth >= MAX_MAPPING_REMAP_DEPTH {
                editor.set_status_message("Mapping recursion limit reached".to_string());
                return Ok(true);
            }

            Self::execute_mapping_rhs(editor, &mapping.rhs, !mapping.noremap, remap_depth + 1)?;
            return Ok(true);
        }

        if editor.keymaps().has_prefix(map_mode, &sequence) {
            return Ok(true);
        }

        let replay = editor.take_pending_mapping_events();
        for event in replay {
            Self::handle_key_event_internal(editor, event, false, false, remap_depth)?;
        }
        Ok(true)
    }

    fn execute_mapping_rhs(
        editor: &mut Editor,
        rhs: &str,
        allow_remap: bool,
        remap_depth: usize,
    ) -> Result<()> {
        let events = Self::decode_mapping_rhs(rhs);
        for event in events {
            Self::handle_key_event_internal(editor, event, allow_remap, false, remap_depth)?;
        }
        Ok(())
    }

    fn encode_key_for_mapping_lookup(key_event: KeyEvent) -> Option<String> {
        if key_event.modifiers.contains(Modifiers::SUPER)
            || key_event.modifiers.contains(Modifiers::ALT)
        {
            return None;
        }

        match key_event.code {
            KeyCode::Char(c) => {
                if key_event.modifiers.contains(Modifiers::CONTROL) {
                    if c.is_ascii_alphabetic() {
                        let ctrl = ((c.to_ascii_lowercase() as u8) - b'a' + 1) as char;
                        return Some(ctrl.to_string());
                    }
                    return None;
                }

                if key_event.modifiers == Modifiers::NONE || key_event.modifiers == Modifiers::SHIFT
                {
                    Some(c.to_string())
                } else {
                    None
                }
            }
            KeyCode::Enter if key_event.modifiers == Modifiers::NONE => Some("\n".to_string()),
            KeyCode::Esc if key_event.modifiers == Modifiers::NONE => Some("\x1b".to_string()),
            KeyCode::Tab if key_event.modifiers == Modifiers::NONE => Some("\t".to_string()),
            KeyCode::Backspace if key_event.modifiers == Modifiers::NONE => {
                Some("\x7f".to_string())
            }
            KeyCode::Up if key_event.modifiers == Modifiers::NONE => Some("\x1b[A".to_string()),
            KeyCode::Down if key_event.modifiers == Modifiers::NONE => Some("\x1b[B".to_string()),
            KeyCode::Right if key_event.modifiers == Modifiers::NONE => Some("\x1b[C".to_string()),
            KeyCode::Left if key_event.modifiers == Modifiers::NONE => Some("\x1b[D".to_string()),
            _ => None,
        }
    }

    fn decode_mapping_rhs(rhs: &str) -> Vec<KeyEvent> {
        let chars: Vec<char> = rhs.chars().collect();
        let mut result = Vec::new();
        let mut i = 0usize;

        while i < chars.len() {
            let ch = chars[i];
            match ch {
                '\n' => {
                    result.push(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
                    i += 1;
                }
                '\t' => {
                    result.push(KeyEvent::new(KeyCode::Tab, Modifiers::NONE));
                    i += 1;
                }
                '\x7f' => {
                    result.push(KeyEvent::new(KeyCode::Backspace, Modifiers::NONE));
                    i += 1;
                }
                '\x1b' => {
                    if i + 2 < chars.len() && chars[i + 1] == '[' {
                        let arrow_key = match chars[i + 2] {
                            'A' => Some(KeyCode::Up),
                            'B' => Some(KeyCode::Down),
                            'C' => Some(KeyCode::Right),
                            'D' => Some(KeyCode::Left),
                            _ => None,
                        };
                        if let Some(code) = arrow_key {
                            result.push(KeyEvent::new(code, Modifiers::NONE));
                            i += 3;
                            continue;
                        }
                    }
                    result.push(KeyEvent::new(KeyCode::Esc, Modifiers::NONE));
                    i += 1;
                }
                c if c.is_ascii() => {
                    let byte = c as u8;
                    if (1..=26).contains(&byte) {
                        let ctrl_char = (byte - 1 + b'a') as char;
                        result.push(KeyEvent::new(KeyCode::Char(ctrl_char), Modifiers::CONTROL));
                    } else {
                        result.push(KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
                    }
                    i += 1;
                }
                c => {
                    result.push(KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
                    i += 1;
                }
            }
        }

        result
    }

    /// Handles input in Normal mode
    fn handle_normal_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
        // =====================================================================
        // STATE MACHINE DISPATCH
        // =====================================================================
        // Check the InputState first. This handles states that were
        // previously causing collisions (e.g., <Space>t vs t motion).
        match editor.input_state().clone() {
            InputState::AwaitingChar { motion, operator } => {
                // Handle f/t/F/T/r/m/'/` second character
                return char_motion::handle_char_motion(editor, key_event, motion, operator);
            }
            InputState::Leader { ref keys } => {
                // Handle leader sequences (<Space>...)
                let keys_clone = keys.clone();
                return leader::handle_leader_input(editor, key_event, &keys_clone);
            }
            InputState::Normal => {
                // Fall through to normal mode dispatcher
            }
            _ => {
                // For unhandled states, reset and fall through
                editor.reset_input_state();
            }
        }

        // =====================================================================
        // DELEGATE TO NORMAL MODE DISPATCHER
        // =====================================================================
        // All other normal mode handling is in the normal/ submodule
        normal::handle_normal_mode(editor, key_event)
    }

    // Removed ~3,100 lines of legacy normal mode handlers.
    // Now handled by normal/ submodule with focused handlers:
    // - normal/operators.rs       - Operator+motion combos (dd, dw, yy, cc, etc.)
    // - normal/text_objects.rs    - Text objects (diw, ci", dap, etc.)
    // - normal/pending_commands.rs - Multi-key sequences (g*, z*, m*, etc.)
    // - normal/mode_transitions.rs - Mode switches (i, a, v, :, etc.)
    // - normal/editing_commands.rs - Direct edits (x, D, p, J, u, etc.)
    // - normal/motions_input.rs   - Motions (h, j, k, l, w, b, G, etc.)

    /// Wrapper to call commands module's execute_command_string
    pub fn execute_command_string(editor: &mut Editor, command: &str) -> Result<()> {
        commands::execute_command_string(editor, command)
    }

    /// Execute a command string for the headless API / CLI, returning a
    /// structured result. Unlike the standard commands module, this reaches
    /// substitute, global, and range commands (full parity with the
    /// interactive command line).
    pub fn execute_command_api(
        editor: &mut Editor,
        command: &str,
    ) -> crate::command_result::CommandResult {
        commands::execute_command_string_api(editor, command)
    }

    /// Wrapper to call commands module's handle_command_mode
    pub fn handle_command_mode_wrapper(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
        commands::handle_command_mode(editor, key_event)
    }

    /// Wrapper to call commands module's parse_range
    pub fn parse_range_wrapper(editor: &Editor, range_str: &str) -> Option<(usize, usize)> {
        commands::parse_range(editor, range_str)
    }
}
