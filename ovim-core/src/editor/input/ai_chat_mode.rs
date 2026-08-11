use crate::ai::chat_types::{ChatFocus, ChatRole};
use crate::editor::ai_chat_input::{
    move_chat_input_cursor_vertical, next_chat_input_word_boundary,
    previous_chat_input_word_boundary, wrap_chat_input_rows,
};
use crate::editor::ai_chat_state::AgentComposerActionKind;
use crate::editor::Editor;
use crate::{KeyCode, KeyEvent, Modifiers};
use anyhow::Result;

use super::code_explanation_mode;

pub fn handle_ai_chat_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    if editor.ai_chat_has_exa_setup_dialog() {
        editor.handle_exa_setup_key(key_event);
        return Ok(());
    }
    if editor.ai_chat_image_modal_path().is_some() {
        if matches!(key_event.code, KeyCode::Esc | KeyCode::Enter) {
            editor.close_ai_chat_image_modal();
        }
        return Ok(());
    }
    if editor.ai_shell_inspector_is_open() {
        handle_shell_inspector(editor, key_event);
        return Ok(());
    }
    if code_explanation_mode::handle_key(editor, key_event) {
        return Ok(());
    }

    // --- Review mode: explicit allowlist only (no normal-mode delegation) ---
    let review_mode = editor.ai_chat_review_mode();

    if key_event.code == KeyCode::Char('c') && key_event.modifiers.contains(Modifiers::CONTROL) {
        // Ctrl-C keeps its established "stop" meaning while agent work is
        // active. When the composer is idle, it clears the draft in place so
        // recalling a historical message remains easy to cancel.
        if !editor.cancel_ai_chat_generation()
            && !review_mode
            && editor.ai_chat_focus() == ChatFocus::TextInput
        {
            editor.clear_ai_chat_input();
        }
        return Ok(());
    }

    if review_mode {
        let pending_work = editor.ai_chat_has_pending_work();

        // <C-r> toggles back to chat
        if key_event.code == KeyCode::Char('r') && key_event.modifiers.contains(Modifiers::CONTROL)
        {
            editor.ai_chat_exit_review_mode();
            return Ok(());
        }
        if key_event.code == KeyCode::Left {
            editor.goto_agent_edit(false);
            return Ok(());
        }
        if key_event.code == KeyCode::Right {
            editor.goto_agent_edit(true);
            return Ok(());
        }
        if key_event.code == KeyCode::Enter {
            if pending_work {
                editor.set_status_message(
                    "AI work is still pending. Wait before accepting review.".to_string(),
                );
                return Ok(());
            }
            editor.ai_chat_accept_review();
            return Ok(());
        }
        // Hiding review must not block background agent work. Accepting the
        // review still waits for the turn to finish, but Esc simply returns
        // control to the editor and preserves the live review state.
        if key_event.code == KeyCode::Esc {
            editor.close_ai_chat();
            return Ok(());
        }
        if key_event.code == KeyCode::PageUp {
            editor.scroll_page_up();
            return Ok(());
        }
        if key_event.code == KeyCode::PageDown {
            editor.scroll_page_down();
            return Ok(());
        }
        return Ok(());
    }

    if editor.ai_chat_has_pending_no_repo_folder_approval() {
        if key_event.code == KeyCode::Enter
            || (key_event.code == KeyCode::Char('y')
                && key_event.modifiers.contains(Modifiers::CONTROL))
            || (key_event.code == KeyCode::Char('a')
                && key_event.modifiers.contains(Modifiers::CONTROL))
        {
            editor.ai_chat_resolve_pending_no_repo_folder_approval(true);
            return Ok(());
        }
        if key_event.code == KeyCode::Esc
            || (key_event.code == KeyCode::Char('n')
                && key_event.modifiers.contains(Modifiers::CONTROL))
        {
            editor.ai_chat_resolve_pending_no_repo_folder_approval(false);
            return Ok(());
        }
        return Ok(());
    }

    if editor.ai_chat_has_pending_tool_approval() {
        if key_event.code == KeyCode::Enter
            || (key_event.code == KeyCode::Char('y')
                && key_event.modifiers.contains(Modifiers::CONTROL))
        {
            editor.ai_chat_resolve_pending_tool_approval(true, false);
            return Ok(());
        }
        if key_event.code == KeyCode::Char('a') && key_event.modifiers.contains(Modifiers::CONTROL)
        {
            editor.ai_chat_resolve_pending_tool_approval(true, true);
            return Ok(());
        }
        if key_event.code == KeyCode::Esc
            || (key_event.code == KeyCode::Char('n')
                && key_event.modifiers.contains(Modifiers::CONTROL))
        {
            editor.ai_chat_resolve_pending_tool_approval(false, false);
            return Ok(());
        }
        return Ok(());
    }

    // A delegated child that pauses for approval must not seize the foreground:
    // it runs in the background while the root turn continues, so the composer,
    // Enter (submit), and Esc (hide) all stay live. Only the explicit
    // Ctrl-Y/Ctrl-N chords resolve the oldest pending child approval from any
    // focus; the agent tree (Ctrl-T) resolves a specific selected child with
    // a/d. Every other key falls through to normal chat handling.
    if editor.ai_agent_has_pending_approval() && key_event.modifiers.contains(Modifiers::CONTROL) {
        if key_event.code == KeyCode::Char('y') {
            if let Err(error) = editor.ai_agent_resolve_pending_approval(true) {
                editor.set_status_message(format!("Failed to allow child approval: {error}"));
            }
            return Ok(());
        }
        if key_event.code == KeyCode::Char('n') {
            if let Err(error) = editor.ai_agent_resolve_pending_approval(false) {
                editor.set_status_message(format!("Failed to deny child approval: {error}"));
            }
            return Ok(());
        }
    }

    let focus = editor.ai_chat_focus();

    // --- Global keys (all zones) ---
    if key_event.code == KeyCode::Esc {
        if editor.cancel_ai_agent_composer_action() {
            return Ok(());
        }
        return handle_escape(editor, focus);
    }

    if key_event.code == KeyCode::Char('c') && key_event.modifiers.contains(Modifiers::SUPER) {
        if !editor.copy_ai_chat_text_selection() {
            editor.copy_ai_chat_conversation();
        }
        return Ok(());
    }

    // Scroll buffer viewport while chat is open.
    if key_event.code == KeyCode::PageUp {
        editor.scroll_page_up();
        return Ok(());
    }
    if key_event.code == KeyCode::PageDown {
        editor.scroll_page_down();
        return Ok(());
    }

    // <C-r> toggles review mode
    if key_event.code == KeyCode::Char('r') && key_event.modifiers.contains(Modifiers::CONTROL) {
        editor.ai_chat_enter_review_mode();
        return Ok(());
    }

    // <C-y> copies conversation to clipboard (unless used for pending tool approval)
    if key_event.code == KeyCode::Char('y') && key_event.modifiers.contains(Modifiers::CONTROL) {
        if !editor.copy_ai_chat_text_selection() {
            editor.copy_ai_chat_conversation();
        }
        return Ok(());
    }

    // <C-t> remains the primary-conversation branch tree. Delegated agents
    // use the switcher reached with Down from an empty composer.
    if key_event.code == KeyCode::Char('t') && key_event.modifiers.contains(Modifiers::CONTROL) {
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.tree_panel_open = !chat.tree_panel_open;
            if chat.tree_panel_open {
                chat.focus = ChatFocus::TreePanel;
                chat.agent_tree_focused = false;
            } else if chat.focus == ChatFocus::TreePanel {
                chat.focus = ChatFocus::TextInput;
            }
        }
        return Ok(());
    }

    match focus {
        ChatFocus::TextInput => handle_text_input(editor, key_event),
        ChatFocus::MessageHistory => handle_message_history(editor, key_event),
        ChatFocus::ModelSelector => handle_model_selector(editor, key_event),
        ChatFocus::TreePanel => handle_tree_panel(editor, key_event),
    }
}

fn handle_escape(editor: &mut Editor, focus: ChatFocus) -> Result<()> {
    if focus != ChatFocus::TextInput {
        // Return to text input
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.focus = ChatFocus::TextInput;
        }
        return Ok(());
    }

    // Chat is a panel over a background-capable agent, not an editor-wide
    // modal lock. A single Esc hides it and leaves the turn, draft, queue, and
    // review state intact.
    editor.close_ai_chat();
    Ok(())
}

fn handle_text_input(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    let word_modifier = key_event
        .modifiers
        .intersects(Modifiers::ALT | Modifiers::CONTROL);

    // Completion owns these keys only while the command-name fragment has
    // candidates. All normal composer behavior remains the fallback.
    if !editor.ai_chat_slash_completions().is_empty() {
        match key_event.code {
            KeyCode::Up | KeyCode::BackTab => {
                editor.move_ai_chat_slash_completion(false);
                return Ok(());
            }
            KeyCode::Down => {
                editor.move_ai_chat_slash_completion(true);
                return Ok(());
            }
            KeyCode::Tab | KeyCode::Enter if !key_event.modifiers.contains(Modifiers::SHIFT) => {
                editor.accept_ai_chat_slash_completion(None);
                return Ok(());
            }
            _ => {}
        }
    }

    match key_event.code {
        KeyCode::Char('v')
            if key_event.modifiers.contains(Modifiers::CONTROL)
                || key_event.modifiers.contains(Modifiers::SUPER) =>
        {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pasted = editor.registers.get_clipboard();
                chat.input.insert_str(chat.input_cursor, &pasted);
                chat.input_cursor += pasted.len();
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Char(ch)
            if !key_event.modifiers.contains(Modifiers::CONTROL)
                && !key_event.modifiers.contains(Modifiers::ALT) =>
        {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pos = chat.input_cursor;
                chat.input.insert(pos, ch);
                chat.input_cursor = pos + ch.len_utf8();
            }
            editor.reset_ai_chat_slash_completion();
        }
        // Match the editor and conventional terminal composers: Ctrl-U
        // removes everything between the cursor and the start of its logical
        // line. This is especially useful for clearing an accidental slash
        // command without closing (and therefore preserving) the chat.
        KeyCode::Char('u') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let end = chat.input_cursor;
                let start = chat.input[..end]
                    .rfind('\n')
                    .map(|newline| newline + 1)
                    .unwrap_or(0);
                chat.input.drain(start..end);
                chat.input_cursor = start;
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Backspace if word_modifier => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let end = chat.input_cursor;
                let start = previous_chat_input_word_boundary(&chat.input, end);
                chat.input.drain(start..end);
                chat.input_cursor = start;
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Backspace => {
            let mut remove_image = false;
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pos = chat.input_cursor;
                if pos > 0 {
                    let prev = chat.input[..pos]
                        .char_indices()
                        .next_back()
                        .map(|(idx, _)| idx)
                        .unwrap_or(0);
                    chat.input.remove(prev);
                    chat.input_cursor = prev;
                } else if chat.input.is_empty() {
                    remove_image = true;
                }
            }
            if remove_image {
                editor.remove_last_ai_chat_image();
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Delete if word_modifier => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let start = chat.input_cursor;
                let end = next_chat_input_word_boundary(&chat.input, start);
                chat.input.drain(start..end);
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Delete => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pos = chat.input_cursor;
                if pos < chat.input.len() {
                    chat.input.remove(pos);
                }
            }
            editor.reset_ai_chat_slash_completion();
        }
        KeyCode::Left if word_modifier => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.input_cursor =
                    previous_chat_input_word_boundary(&chat.input, chat.input_cursor);
            }
        }
        KeyCode::Left => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pos = chat.input_cursor;
                if pos > 0 {
                    let prev = chat.input[..pos]
                        .char_indices()
                        .next_back()
                        .map(|(idx, _)| idx)
                        .unwrap_or(0);
                    chat.input_cursor = prev;
                }
            }
        }
        KeyCode::Right if word_modifier => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.input_cursor = next_chat_input_word_boundary(&chat.input, chat.input_cursor);
            }
        }
        KeyCode::Right => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                let pos = chat.input_cursor;
                if pos < chat.input.len() {
                    let next = chat.input[pos..]
                        .char_indices()
                        .nth(1)
                        .map(|(idx, _)| pos + idx)
                        .unwrap_or(chat.input.len());
                    chat.input_cursor = next;
                }
            }
        }
        KeyCode::Home => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.input_cursor = 0;
            }
        }
        KeyCode::End => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.input_cursor = chat.input.len();
            }
        }
        KeyCode::Up => {
            let mut moved_to_history = false;
            let wrap_width = editor.render_cache.ai_chat_input_content_width;
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                if wrap_width > 0 {
                    let rows =
                        wrap_chat_input_rows(&chat.input, wrap_width, editor.options.tab_width);
                    if let Some(target) = move_chat_input_cursor_vertical(
                        &chat.input,
                        chat.input_cursor,
                        &rows,
                        -1,
                        editor.options.tab_width,
                    ) {
                        chat.input_cursor = target;
                    } else {
                        chat.focus = ChatFocus::MessageHistory;
                        moved_to_history = true;
                    }
                } else {
                    let (line, _total) = cursor_line_info(&chat.input, chat.input_cursor);
                    if line == 0 {
                        chat.focus = ChatFocus::MessageHistory;
                        moved_to_history = true;
                    } else {
                        chat.input_cursor =
                            move_cursor_vertical(&chat.input, chat.input_cursor, -1);
                    }
                }
            }
            if moved_to_history {
                editor.ai_chat_reset_history_cursor();
            }
        }
        KeyCode::Down => {
            let can_open_agent_switcher = editor
                .ai_state
                .chat
                .as_ref()
                .is_some_and(|chat| chat.input.is_empty())
                && editor
                    .ai_agent_current_snapshot()
                    .ok()
                    .flatten()
                    .is_some_and(|snapshot| !snapshot.agents.is_empty());
            if can_open_agent_switcher {
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    chat.focus = ChatFocus::TreePanel;
                    chat.agent_tree_focused = true;
                    chat.agent_tree_cursor = 0;
                }
                return Ok(());
            }
            let wrap_width = editor.render_cache.ai_chat_input_content_width;
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                if wrap_width > 0 {
                    let rows =
                        wrap_chat_input_rows(&chat.input, wrap_width, editor.options.tab_width);
                    if let Some(target) = move_chat_input_cursor_vertical(
                        &chat.input,
                        chat.input_cursor,
                        &rows,
                        1,
                        editor.options.tab_width,
                    ) {
                        chat.input_cursor = target;
                    }
                } else {
                    let (line, total) = cursor_line_info(&chat.input, chat.input_cursor);
                    if line < total - 1 {
                        chat.input_cursor = move_cursor_vertical(&chat.input, chat.input_cursor, 1);
                    }
                }
            }
        }
        KeyCode::Enter if key_event.modifiers.contains(Modifiers::SHIFT) => {
            insert_chat_input_newline(editor);
        }
        // Legacy terminals may encode Shift-Enter as LF. In raw mode Crossterm
        // exposes that indistinguishable byte as Ctrl-J, so accept it as the
        // composer newline fallback without changing Ctrl-J in other modes.
        KeyCode::Char('j') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            insert_chat_input_newline(editor);
        }
        KeyCode::Enter => match editor.submit_ai_agent_composer_action() {
            Ok(true) => {}
            Ok(false) => {
                if let Err(error) = editor.submit_ai_chat_message() {
                    editor.set_status_message(format!("Unable to submit AI chat message: {error}"));
                }
            }
            Err(error) => editor.set_status_message(error),
        },
        KeyCode::Tab => {
            editor.schedule_ai_chat_message()?;
        }
        KeyCode::Char('g') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.open_chat_scratch_editor();
        }
        _ => {}
    }
    Ok(())
}

fn insert_chat_input_newline(editor: &mut Editor) {
    if let Some(chat) = editor.ai_state.chat.as_mut() {
        let cursor = chat.input_cursor;
        chat.input.insert(cursor, '\n');
        chat.input_cursor = cursor + 1;
    }
    editor.reset_ai_chat_slash_completion();
}

fn handle_message_history(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    match key_event.code {
        KeyCode::Char('y') if editor.ai_chat_has_text_selection() => {
            editor.copy_ai_chat_text_selection();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            editor.ai_chat_history_cursor_move_older(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let at_latest = editor.ai_chat_history_is_latest_selected();
            if at_latest {
                if editor.ai_chat_scroll_viewport_down(1) {
                    if let Some(chat) = editor.ai_state.chat.as_mut() {
                        chat.focus = ChatFocus::TextInput;
                    }
                }
            } else {
                editor.ai_chat_history_cursor_move_newer(1);
            }
        }
        // Ctrl-U — scroll up half page
        KeyCode::Char('u') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.ai_chat_scroll_viewport_up(10);
        }
        // Ctrl-D — scroll down half page
        KeyCode::Char('d') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.ai_chat_scroll_viewport_down(10);
        }
        KeyCode::Enter => {
            if let Some(id) = editor.ai_chat_history_selected_queued_id() {
                editor.recall_queued_ai_chat_input(id);
                return Ok(());
            }
            if let Some(tool_call_id) = editor
                .ai_chat_history_selected_shell_tool_id()
                .map(str::to_owned)
            {
                editor.open_ai_shell_process_inspector(&tool_call_id);
                return Ok(());
            }
            let node_ids = editor
                .conversation()
                .map(|c| c.node_ids_for_active_branch().to_vec())
                .unwrap_or_default();
            let messages = editor.ai_chat_messages();
            if let Some(idx) = editor.ai_chat_history_selected_index() {
                if idx >= messages.len() || idx >= node_ids.len() {
                    return Ok(());
                }
                let node_id = node_ids[idx];
                let role = messages[idx].role.clone();
                let tool_call_id = messages[idx].tool_call_id.clone();

                if role == ChatRole::Tool {
                    if let Some(tool_call_id) = tool_call_id.as_deref() {
                        if !editor.open_ai_shell_process_inspector(tool_call_id) {
                            editor.toggle_ai_chat_tool_event(tool_call_id);
                        }
                    }
                } else if role == ChatRole::Thinking {
                    // Toggle thinking expand/collapse
                    if let Some(chat) = editor.ai_state.chat.as_mut() {
                        if !chat.expanded_thinking.remove(&node_id) {
                            chat.expanded_thinking.insert(node_id);
                        }
                    }
                } else if role == ChatRole::User {
                    let content = messages[idx].content.clone();
                    let images = messages[idx].images.clone();
                    // Fork: set active_leaf to parent of this user message
                    let parent_id = editor
                        .conversation()
                        .and_then(|c| c.node(node_id))
                        .and_then(|n| n.parent);
                    if let Some(pid) = parent_id {
                        editor.fork_ai_chat_runtime_from(pid);
                    }
                    // Return to text input for the new message
                    if let Some(chat) = editor.ai_state.chat.as_mut() {
                        chat.input = content;
                        chat.input_cursor = chat.input.len();
                        chat.pending_images = images;
                        chat.history.selected_node_id = None;
                        chat.focus = ChatFocus::TextInput;
                    }
                    editor.reset_ai_chat_slash_completion();
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_shell_inspector(editor: &mut Editor, key_event: KeyEvent) {
    let search_active = editor
        .ai_shell_inspector_view()
        .is_some_and(|view| view.search_input.is_some());
    if search_active {
        match key_event.code {
            KeyCode::Esc => editor.ai_shell_inspector_cancel_search(),
            KeyCode::Enter => editor.ai_shell_inspector_submit_search(),
            KeyCode::Backspace => editor.ai_shell_inspector_search_backspace(),
            KeyCode::Char(character)
                if !key_event
                    .modifiers
                    .intersects(Modifiers::CONTROL | Modifiers::SUPER) =>
            {
                editor.ai_shell_inspector_search_insert(character);
            }
            _ => {}
        }
        return;
    }

    match key_event.code {
        KeyCode::Esc | KeyCode::Char('q') => editor.close_ai_shell_process_inspector(),
        KeyCode::Char('c') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.interrupt_ai_shell_process(false);
        }
        KeyCode::Char('k') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.interrupt_ai_shell_process(true);
        }
        KeyCode::Up | KeyCode::Char('k') => editor.ai_shell_inspector_scroll_up(1),
        KeyCode::Down | KeyCode::Char('j') => editor.ai_shell_inspector_scroll_down(1),
        KeyCode::PageUp => editor.ai_shell_inspector_scroll_up(10),
        KeyCode::Char('u') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.ai_shell_inspector_scroll_up(10);
        }
        KeyCode::PageDown => editor.ai_shell_inspector_scroll_down(10),
        KeyCode::Char('d') if key_event.modifiers.contains(Modifiers::CONTROL) => {
            editor.ai_shell_inspector_scroll_down(10);
        }
        KeyCode::Home | KeyCode::Char('g') => editor.ai_shell_inspector_scroll_to_top(),
        KeyCode::End | KeyCode::Char('G') => editor.ai_shell_inspector_follow_latest(),
        KeyCode::Char('/') => editor.ai_shell_inspector_begin_search(),
        KeyCode::Char('n') => editor.ai_shell_inspector_next_match(),
        _ => {}
    }
}

fn handle_model_selector(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    use crate::editor::ChatModelPickerSection;

    match key_event.code {
        KeyCode::Tab | KeyCode::BackTab => {
            let section = if editor.ai_chat_model_picker_section() == ChatModelPickerSection::Model
            {
                ChatModelPickerSection::Effort
            } else {
                ChatModelPickerSection::Model
            };
            editor.open_ai_chat_model_picker(section);
        }
        KeyCode::Up | KeyCode::Left | KeyCode::Char('k') | KeyCode::Char('h') => {
            if editor.ai_chat_model_picker_section() == ChatModelPickerSection::Model {
                editor.ai_cycle_profile(false);
            } else {
                editor.cycle_ai_chat_reasoning_effort(false);
            }
        }
        KeyCode::Down | KeyCode::Right | KeyCode::Char('j') | KeyCode::Char('l') => {
            if editor.ai_chat_model_picker_section() == ChatModelPickerSection::Model {
                editor.ai_cycle_profile(true);
            } else {
                editor.cycle_ai_chat_reasoning_effort(true);
            }
        }
        KeyCode::Enter => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.focus = ChatFocus::TextInput;
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_tree_panel(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    let snapshot = editor.ai_agent_current_snapshot().ok().flatten();
    let agent_count = snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.agents.len());
    let agent_focused = agent_count > 0 && editor.ai_agent_tree_focused();

    if key_event.code == KeyCode::Tab && agent_count > 0 {
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.agent_tree_focused = !chat.agent_tree_focused;
        }
        return Ok(());
    }

    if agent_focused {
        // Cursor zero is the primary conversation. Delegated agents follow in
        // hierarchy order so Up always has an obvious route home.
        let selected = editor
            .ai_agent_tree_cursor()
            .checked_sub(1)
            .and_then(|index| {
                snapshot.as_ref().and_then(|snapshot| {
                    snapshot
                        .hierarchy()
                        .get(index)
                        .map(|agent| (agent.agent_id.to_string(), agent.lifecycle.clone()))
                })
            });
        let selected_id = selected.as_ref().map(|(agent_id, _)| agent_id.clone());
        match key_event.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(chat) = editor.ai_state.chat.as_mut()
                    && agent_count > 0
                {
                    chat.agent_tree_cursor = (chat.agent_tree_cursor + 1).min(agent_count);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    if chat.agent_tree_cursor == 0 {
                        chat.focus = ChatFocus::TextInput;
                    } else {
                        chat.agent_tree_cursor -= 1;
                    }
                }
            }
            KeyCode::Enter => {
                if let Some((agent_id, lifecycle)) = selected {
                    editor.cancel_ai_agent_composer_action();
                    if let Some(chat) = editor.ai_state.chat.as_mut() {
                        chat.selected_agent_id = Some(agent_id.clone());
                    }
                    let kind = if matches!(
                        lifecycle.as_str(),
                        "starting"
                            | "running"
                            | "waiting_for_agent"
                            | "waiting_for_tool"
                            | "waiting_for_user"
                    ) {
                        Some(AgentComposerActionKind::Message)
                    } else if matches!(lifecycle.as_str(), "completed" | "interrupted") {
                        Some(AgentComposerActionKind::Followup)
                    } else {
                        None
                    };
                    if let Some(kind) = kind {
                        if let Err(error) = editor.begin_ai_agent_composer_action(agent_id, kind) {
                            editor.set_status_message(error);
                        }
                    } else if let Some(chat) = editor.ai_state.chat.as_mut() {
                        chat.focus = ChatFocus::MessageHistory;
                    }
                } else {
                    editor.cancel_ai_agent_composer_action();
                    if let Some(chat) = editor.ai_state.chat.as_mut() {
                        chat.selected_agent_id = None;
                        chat.focus = ChatFocus::TextInput;
                    }
                }
            }
            KeyCode::Char('f') | KeyCode::Char('w') => {
                if let (Some(chat), Some(agent_id)) = (editor.ai_state.chat.as_mut(), selected_id) {
                    if chat.followed_agent_id.as_deref() == Some(agent_id.as_str()) {
                        chat.followed_agent_id = None;
                    } else {
                        chat.selected_agent_id = Some(agent_id.clone());
                        chat.followed_agent_id = Some(agent_id);
                    }
                }
            }
            KeyCode::Char('i') => {
                if let Some(agent_id) = selected_id
                    && let Err(error) = editor.interrupt_ai_agent_from_ui(agent_id)
                {
                    editor.set_status_message(format!("Could not interrupt child: {error}"));
                }
            }
            KeyCode::Char('m') => {
                if let Some(agent_id) = selected_id
                    && let Err(error) = editor
                        .begin_ai_agent_composer_action(agent_id, AgentComposerActionKind::Message)
                {
                    editor.set_status_message(error);
                }
            }
            KeyCode::Char('r') => {
                if let Some(agent_id) = selected_id
                    && let Err(error) = editor
                        .begin_ai_agent_composer_action(agent_id, AgentComposerActionKind::Followup)
                {
                    editor.set_status_message(error);
                }
            }
            KeyCode::Char('a') => {
                if let Err(error) =
                    editor.ai_agent_resolve_approval_for(selected_id.as_deref(), true)
                {
                    editor.set_status_message(error);
                }
            }
            KeyCode::Char('d') => {
                if let Err(error) =
                    editor.ai_agent_resolve_approval_for(selected_id.as_deref(), false)
                {
                    editor.set_status_message(error);
                }
            }
            KeyCode::Char('q') => {
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    chat.tree_panel_open = false;
                    chat.focus = ChatFocus::TextInput;
                }
            }
            _ => {}
        }
        return Ok(());
    }

    match key_event.code {
        KeyCode::Down | KeyCode::Char('j') => {
            let total = tree_panel_node_count(editor);
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                if total > 0 {
                    chat.tree_panel_cursor = (chat.tree_panel_cursor + 1).min(total - 1);
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.tree_panel_cursor = chat.tree_panel_cursor.saturating_sub(1);
            }
        }
        KeyCode::Enter => {
            let node_id = tree_panel_selected_node_id(editor);
            if let Some(id) = node_id {
                editor.switch_ai_chat_runtime_branch(id);
                if let Some(chat) = editor.ai_state.chat.as_mut() {
                    chat.focus = ChatFocus::TextInput;
                }
            }
        }
        KeyCode::Char('q') => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.tree_panel_open = false;
                chat.focus = ChatFocus::TextInput;
            }
        }
        KeyCode::BackTab if agent_count > 0 => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.agent_tree_focused = true;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Count total nodes for tree panel navigation.
fn tree_panel_node_count(editor: &Editor) -> usize {
    editor
        .conversation()
        .map(|c| c.all_nodes().len())
        .unwrap_or(0)
}

/// Get the NodeId at the current tree panel cursor position.
/// Uses DFS order from root.
fn tree_panel_selected_node_id(editor: &Editor) -> Option<u64> {
    let conv = editor.conversation()?;
    let root_id = conv.root_id()?;
    let cursor = editor.ai_chat_tree_panel_cursor();
    let mut dfs_order = Vec::new();
    dfs_collect(conv, root_id, &mut dfs_order);
    dfs_order.get(cursor).copied()
}

fn dfs_collect(conv: &crate::ai::chat_types::ConversationTree, node_id: u64, out: &mut Vec<u64>) {
    out.push(node_id);
    if let Some(node) = conv.node(node_id) {
        for &child_id in &node.children {
            dfs_collect(conv, child_id, out);
        }
    }
}

/// Returns (current_line_index, total_lines) for the cursor position in input.
fn cursor_line_info(input: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(input.len());
    let line = input[..cursor].matches('\n').count();
    let total = input.matches('\n').count() + 1;
    (line, total)
}

/// Move cursor up (direction=-1) or down (direction=1) within multi-line input.
/// Tries to maintain column offset. Returns the new byte offset.
fn move_cursor_vertical(input: &str, cursor: usize, direction: i8) -> usize {
    let cursor = cursor.min(input.len());

    // Find start of current line and column offset
    let line_start = input[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col: usize = input[line_start..cursor].chars().count();

    if direction < 0 {
        // Move up: find the previous line
        if line_start == 0 {
            return cursor; // already on first line
        }
        let prev_line_end = line_start - 1; // the '\n' before current line
        let prev_line_start = input[..prev_line_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line = &input[prev_line_start..prev_line_end];
        let prev_line_chars = prev_line.chars().count();
        let target_col = col.min(prev_line_chars);
        // Convert char offset to byte offset
        let byte_offset: usize = prev_line
            .chars()
            .take(target_col)
            .map(|c| c.len_utf8())
            .sum();
        prev_line_start + byte_offset
    } else {
        // Move down: find the next line
        let next_newline = input[cursor..].find('\n');
        match next_newline {
            None => cursor, // already on last line
            Some(rel) => {
                let next_line_start = cursor + rel + 1;
                let next_line_end = input[next_line_start..]
                    .find('\n')
                    .map(|i| next_line_start + i)
                    .unwrap_or(input.len());
                let next_line = &input[next_line_start..next_line_end];
                let next_line_chars = next_line.chars().count();
                let target_col = col.min(next_line_chars);
                let byte_offset: usize = next_line
                    .chars()
                    .take(target_col)
                    .map(|c| c.len_utf8())
                    .sum();
                next_line_start + byte_offset
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::ai::chat_types::ToolCallInfo;
    use crate::editor::render_cache::{ChatTextPoint, ChatTextSelection};
    use std::path::PathBuf;

    fn open_test_chat(editor: &mut Editor) {
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
    }

    fn append_recorded_test_turn(editor: &mut Editor, user: &str, assistant: &str) -> (u64, u64) {
        let turn = editor.begin_ai_runtime_turn(user).expect("begin turn");
        let user_event = turn.initiating_event.caused_by.clone().expect("user event");
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));
        let user_node = editor
            .conversation_mut()
            .unwrap()
            .append_user_message(user.into());
        editor.record_ai_chat_node(user_node, user_event);
        let assistant_event = editor
            .ai_runtime_append_agent_message(assistant)
            .expect("assistant event");
        let assistant_node = editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message(assistant.into(), "test".into());
        editor.record_ai_chat_node(assistant_node, assistant_event);
        editor.ai_runtime_complete_turn();
        (user_node, assistant_node)
    }

    #[test]
    fn review_mode_ignores_unmapped_keys() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor.ai_chat_enter_review_mode();

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('i'), Modifiers::NONE),
        )
        .expect("handle key");

        assert!(editor.ai_chat_review_mode());
        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
    }

    #[test]
    fn command_c_copies_and_clears_chat_text_selection() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor.render_cache.ai_chat_rendered_text_rows = vec!["selected text".to_string()];
        editor.render_cache.ai_chat_text_selection = Some(ChatTextSelection {
            anchor: ChatTextPoint { row: 0, column: 0 },
            head: ChatTextPoint { row: 0, column: 7 },
            moved: true,
        });

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('c'), Modifiers::SUPER),
        )
        .expect("copy selection");

        assert!(!editor.ai_chat_has_text_selection());
        assert!(editor.render_cache.ai_chat_text_selection.is_none());
    }

    #[test]
    fn review_mode_blocks_accept_but_can_hide_while_pending() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let buffer_id = editor.buffer().id();
        editor.ai_chat_enter_review_mode();
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.waiting = true;
            chat.agent_edits.record_edit(buffer_id, 0, 0);
        }

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            .expect("enter");
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
            .expect("esc");

        let chat = editor.ai_state.chat.as_ref().expect("chat");
        assert!(chat.view_mode == crate::editor::ai_chat_state::ChatViewMode::ReviewFocused);
        assert_eq!(chat.agent_edits.total_edit_count(), 1);
        assert_ne!(editor.mode(), crate::mode::Mode::AiChat);
        assert!(editor.ai_chat_waiting());
    }

    #[test]
    fn escape_hides_running_chat_on_first_press_without_stopping_it() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor.ai_state.chat.as_mut().unwrap().waiting = true;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
            .expect("esc");

        assert_ne!(editor.mode(), crate::mode::Mode::AiChat);
        assert!(editor.ai_chat_waiting());
        assert!(editor.ai_state.chat.is_some());
    }

    #[test]
    fn escape_cancels_targeted_agent_composer_and_restores_root_draft() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "message for child".into();
        chat.input_cursor = chat.input.len();
        chat.pending_agent_composer_action =
            Some(crate::editor::ai_chat_state::PendingAgentComposerAction {
                kind: AgentComposerActionKind::Message,
                agent_id: "agt_child".into(),
                previous_input: "preserved root draft".into(),
                previous_cursor: 9,
            });
        chat.selected_agent_id = Some("agt_child".into());

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
            .expect("esc");

        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
        assert_eq!(editor.ai_chat_input(), "preserved root draft");
        assert_eq!(editor.ai_chat_input_cursor(), 9);
        assert!(editor.ai_agent_composer_action().is_none());
        assert!(editor.ai_agent_selected_id().is_none());
    }

    #[test]
    fn control_c_stops_generation_without_closing_chat() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.waiting = true;
            chat.input = "keep this draft".into();
            chat.input_cursor = chat.input.len();
        }

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL),
        )
        .expect("control-c");

        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
        assert!(!editor.ai_chat_waiting());
        assert_eq!(editor.ai_chat_input(), "keep this draft");
        assert!(editor
            .ai_chat_messages()
            .iter()
            .any(|message| message.content == "Generation stopped by user."));
    }

    #[test]
    fn control_c_clears_idle_composer_without_closing_chat() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "discard this draft".into();
        chat.input_cursor = 7;

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL),
        )
        .expect("control-c");

        assert_eq!(editor.mode(), crate::mode::Mode::AiChat);
        assert_eq!(editor.ai_chat_focus(), ChatFocus::TextInput);
        assert_eq!(editor.ai_chat_input(), "");
        assert_eq!(editor.ai_chat_input_cursor(), 0);
    }

    #[test]
    fn enter_on_historical_user_message_forks_and_restores_it_to_composer() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (_, first_reply) =
            append_recorded_test_turn(&mut editor, "first question", "first answer");
        let (second_user, _) =
            append_recorded_test_turn(&mut editor, "question to revise", "second answer");
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "replace this existing draft".into();
        chat.input_cursor = chat.input.len();
        chat.history.selected_node_id = Some(second_user);
        chat.focus = ChatFocus::MessageHistory;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            .expect("recall historical message");

        assert_eq!(editor.ai_chat_focus(), ChatFocus::TextInput);
        assert_eq!(editor.ai_chat_input(), "question to revise");
        assert_eq!(editor.ai_chat_input_cursor(), "question to revise".len());
        assert_eq!(
            editor.conversation().unwrap().active_leaf_id(),
            Some(first_reply)
        );
        assert_eq!(
            editor
                .ai_chat_messages()
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first question", "first answer"]
        );
    }

    #[test]
    fn control_g_opens_recalled_historical_message_in_scratch_buffer() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let (_, first_reply) = append_recorded_test_turn(&mut editor, "first", "answer");
        let (historical_user, _) = append_recorded_test_turn(
            &mut editor,
            "a longer historical message\nwith another line",
            "done",
        );
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.history.selected_node_id = Some(historical_user);
        chat.focus = ChatFocus::MessageHistory;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            .expect("recall historical message");
        assert_eq!(
            editor.conversation().unwrap().active_leaf_id(),
            Some(first_reply)
        );
        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('g'), Modifiers::CONTROL),
        )
        .expect("open scratch editor");

        assert_eq!(editor.mode(), crate::mode::Mode::Normal);
        assert!(editor.is_chat_scratch_buffer());
        assert_eq!(
            editor.buffer().rope().to_string(),
            "a longer historical message\nwith another line"
        );
    }

    #[test]
    fn shift_enter_inserts_newline_without_submitting_chat_input() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "helloworld".into();
        chat.input_cursor = 5;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT))
            .expect("shift-enter");

        assert_eq!(editor.ai_chat_input(), "hello\nworld");
        assert_eq!(editor.ai_chat_input_cursor(), 6);
        assert!(editor.ai_chat_messages().is_empty());
    }

    #[test]
    fn slash_completion_navigates_before_history_and_accepts_without_executing() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/".into();
        chat.input_cursor = 1;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Down, Modifiers::NONE))
            .expect("select next completion");
        assert_eq!(editor.ai_chat_slash_completion_selected(), 1);
        assert_eq!(editor.ai_chat_focus(), ChatFocus::TextInput);

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            .expect("accept completion");
        assert_eq!(editor.ai_chat_input(), "/compact");
        assert!(editor.ai_chat_messages().is_empty());
    }

    #[test]
    fn slash_completion_tab_accepts_instead_of_scheduling() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/cl".into();
        chat.input_cursor = 3;

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Tab, Modifiers::NONE))
            .expect("accept completion");

        assert_eq!(editor.ai_chat_input(), "/clear");
        assert!(editor.ai_chat_queued_inputs().next().is_none());
    }

    #[test]
    fn control_j_inserts_newline_for_legacy_shift_enter_encoding() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "hello".into();
        chat.input_cursor = chat.input.len();

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('j'), Modifiers::CONTROL),
        )
        .expect("control-j");

        assert_eq!(editor.ai_chat_input(), "hello\n");
        assert_eq!(editor.ai_chat_input_cursor(), 6);
        assert!(editor.ai_chat_messages().is_empty());
    }

    #[test]
    fn pending_folder_approval_accepts_enter_and_denies_esc() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.pending_no_repo_folder_approval = Some(PathBuf::from("/tmp/demo"));
        }

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            .expect("enter");
        assert!(!editor.ai_chat_has_pending_no_repo_folder_approval());

        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.pending_no_repo_folder_approval = Some(PathBuf::from("/tmp/demo"));
        }
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
            .expect("esc");
        assert!(!editor.ai_chat_has_pending_no_repo_folder_approval());
    }

    #[test]
    fn pending_tool_approval_accepts_enter_and_denies_esc() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let mut editor = Editor::default();
            open_test_chat(&mut editor);
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.pending_tool_approval =
                    Some(crate::editor::ai_chat_state::PendingToolApproval {
                        tool_call: ToolCallInfo {
                            id: "call1".to_string(),
                            name: "read_file".to_string(),
                            arguments: serde_json::json!({}),
                        },
                        reason: "test approval".into(),
                        runtime_tool: None,
                        runtime_tool_started: false,
                        remaining_tool_calls: Vec::new(),
                        model_name: "test-model".to_string(),
                        requested_path: PathBuf::from("/tmp/demo.txt"),
                        approval_root: PathBuf::from("/tmp"),
                        dynamic_response: None,
                        dynamic_turn: None,
                    });
            }

            handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .expect("enter");
            assert!(!editor.ai_chat_has_pending_tool_approval());

            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.pending_tool_approval =
                    Some(crate::editor::ai_chat_state::PendingToolApproval {
                        tool_call: ToolCallInfo {
                            id: "call2".to_string(),
                            name: "read_file".to_string(),
                            arguments: serde_json::json!({}),
                        },
                        reason: "test approval".into(),
                        runtime_tool: None,
                        runtime_tool_started: false,
                        remaining_tool_calls: Vec::new(),
                        model_name: "test-model".to_string(),
                        requested_path: PathBuf::from("/tmp/demo.txt"),
                        approval_root: PathBuf::from("/tmp"),
                        dynamic_response: None,
                        dynamic_turn: None,
                    });
            }
            handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
                .expect("esc");
            assert!(!editor.ai_chat_has_pending_tool_approval());
        });
    }

    #[test]
    fn up_moves_within_soft_wrapped_composer_before_leaving_input() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor.render_cache.ai_chat_input_content_width = 7;
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "alpha beta gamma".to_string();
        chat.input_cursor = chat.input.len();

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();

        assert_eq!(editor.ai_chat_focus(), ChatFocus::TextInput);
        assert!(editor.ai_chat_input_cursor() < editor.ai_chat_input().len());
    }

    #[test]
    fn queued_inputs_are_selected_before_messages_and_enter_recalls_them() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor
            .conversation_mut()
            .unwrap()
            .append_user_message("earlier message".into());
        let turn = editor.begin_ai_runtime_turn("active request").unwrap();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));

        for content in ["first queued", "/clear"] {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = content.into();
            chat.input_cursor = chat.input.len();
            editor.schedule_ai_chat_message().unwrap();
        }
        let queued_ids = editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .queued_inputs
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();
        assert_eq!(
            editor.ai_chat_history_selected_queued_id(),
            Some(queued_ids[1])
        );

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();
        assert_eq!(
            editor.ai_chat_history_selected_queued_id(),
            Some(queued_ids[0])
        );

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();
        assert!(editor.ai_chat_history_selected_queued_id().is_none());
        assert_eq!(editor.ai_chat_history_selected_index(), Some(0));

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Down, Modifiers::NONE)).unwrap();
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Down, Modifiers::NONE)).unwrap();
        assert_eq!(
            editor.ai_chat_history_selected_queued_id(),
            Some(queued_ids[1])
        );

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE)).unwrap();
        assert_eq!(editor.ai_chat_focus(), ChatFocus::TextInput);
        assert_eq!(editor.ai_chat_input(), "/clear");
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().queued_inputs.len(),
            1
        );
        assert_eq!(
            editor.ai_state.chat.as_ref().unwrap().queued_inputs[0].content,
            "first queued"
        );
    }

    #[test]
    fn live_shell_row_opens_process_inspector_and_routes_modal_keys() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        editor
            .conversation_mut()
            .unwrap()
            .append_user_message("run the build".into());
        let call = ToolCallInfo {
            id: "shell-live".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "npm run build" }),
        };
        let mut transcript = crate::editor::ai_chat_state::ShellTranscript::new(
            call.clone(),
            "npm run build".into(),
            ".".into(),
        );
        transcript.append(
            crate::editor::ai_chat_state::ShellOutputStream::Stdout,
            b"starting\nbuilding\n".to_vec(),
        );
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.streaming_tool_calls.push(call);
        chat.shell_transcripts
            .insert("shell-live".into(), transcript);

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();
        assert_eq!(
            editor.ai_chat_history_selected_shell_tool_id(),
            Some("shell-live")
        );
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE)).unwrap();
        assert!(editor.ai_shell_inspector_is_open());

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Up, Modifiers::NONE)).unwrap();
        assert!(!editor.ai_shell_inspector_view().unwrap().follow_latest);
        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
        )
        .unwrap();
        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('b'), Modifiers::NONE),
        )
        .unwrap();
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Enter, Modifiers::NONE)).unwrap();
        assert_eq!(
            editor
                .ai_shell_inspector_view()
                .unwrap()
                .search_query
                .as_deref(),
            Some("b")
        );
        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE)).unwrap();
        assert!(!editor.ai_shell_inspector_is_open());
    }

    #[test]
    fn option_and_control_edit_chat_input_by_words() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = "one two three".to_string();
            chat.input_cursor = chat.input.len();
        }

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Left, Modifiers::ALT)).unwrap();
        assert_eq!(editor.ai_chat_input_cursor(), 8);

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Left, Modifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(editor.ai_chat_input_cursor(), 4);

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Backspace, Modifiers::ALT),
        )
        .unwrap();
        assert_eq!(editor.ai_chat_input(), "two three");
        assert_eq!(editor.ai_chat_input_cursor(), 0);

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Delete, Modifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(editor.ai_chat_input(), " three");
        assert_eq!(editor.ai_chat_input_cursor(), 0);
    }

    #[test]
    fn option_and_control_right_move_to_word_ends() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = "one  two".to_string();
            chat.input_cursor = 0;
        }

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Right, Modifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(editor.ai_chat_input_cursor(), 3);

        handle_ai_chat_mode(&mut editor, KeyEvent::new(KeyCode::Right, Modifiers::ALT)).unwrap();
        assert_eq!(editor.ai_chat_input_cursor(), editor.ai_chat_input().len());
    }

    #[test]
    fn control_u_deletes_to_logical_line_start() {
        let mut editor = Editor::default();
        open_test_chat(&mut editor);
        {
            let chat = editor.ai_state.chat.as_mut().unwrap();
            chat.input = "keep this\nremove this suffix".to_string();
            chat.input_cursor = "keep this\nremove this".len();
        }

        handle_ai_chat_mode(
            &mut editor,
            KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL),
        )
        .unwrap();

        assert_eq!(editor.ai_chat_input(), "keep this\n suffix");
        assert_eq!(editor.ai_chat_input_cursor(), "keep this\n".len());
    }
}
