use crate::markdown_conceal::{apply_conceal, extract_concealed_links, scan_markdown_conceal};
use crate::{MouseButton, MouseEvent, MouseEventKind};
use anyhow::Result;

use crate::display::{char_display_width, display_col_to_char_col};
use crate::editor::ai_chat_input::{chat_input_byte_for_display_column, ChatInputRow};
use crate::editor::Editor;
use crate::mode::Mode;
use crate::unicode::{char_to_grapheme_col, grapheme_count, GraphemeCol};

use super::code_explanation_mode;

/// Top-level mouse event dispatcher.
/// Returns `Ok(Some(url))` when a concealed markdown link was clicked and should be opened.
pub fn handle_mouse_event(editor: &mut Editor, event: MouseEvent) -> Result<Option<String>> {
    if code_explanation_mode::blocks_pointer_event(editor, &event.kind) {
        return Ok(None);
    }

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            return handle_left_click(editor, event.column, event.row);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            handle_left_drag(editor, event.column, event.row)?;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            handle_left_release(editor)?;
        }
        MouseEventKind::ScrollUp => {
            handle_scroll(editor, true, event.column, event.row)?;
        }
        MouseEventKind::ScrollDown => {
            handle_scroll(editor, false, event.column, event.row)?;
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            handle_middle_click(editor, event.column, event.row)?;
        }
        _ => {} // Right click, mouse move, horizontal scroll: ignored
    }
    Ok(None)
}

/// Converts screen coordinates to buffer (line, col).
/// Returns `None` if the click is outside the buffer area.
fn screen_to_buffer(editor: &Editor, screen_col: u16, screen_row: u16) -> Option<(usize, usize)> {
    let area = editor.render_cache.last_buffer_area?;
    let gutter_width = editor.render_cache.last_gutter_width;

    // Hit-test: must be within buffer area
    if screen_col < area.x
        || screen_row < area.y
        || screen_col >= area.x + area.width
        || screen_row >= area.y + area.height
    {
        return None;
    }

    let rel_col = (screen_col - area.x) as usize;
    let rel_row = (screen_row - area.y) as usize;

    // Click in gutter region — handled separately
    if rel_col < gutter_width {
        return None;
    }

    let display_col_in_row = rel_col - gutter_width;
    let raw_line_text = |line: usize| {
        editor
            .buffer()
            .line_text(line)
            .map(|l| l.to_string())
            .unwrap_or_default()
    };

    // Markdown conceal shortens every line *except* the cursor line (which the
    // renderer reveals). Display columns the user clicked therefore live in
    // *concealed* space for non-cursor lines, so we must hit-test against the
    // concealed text and then map the result back to source columns. Mirror the
    // renderer's `.md` + cursor-line gate exactly.
    let cursor_line = editor.buffer().cursor().line();
    let conceal_active = editor.options.markdown_conceal
        && editor
            .buffer()
            .file_path()
            .map(|p| p.ends_with(".md"))
            .unwrap_or(false);
    // The text as drawn on screen for `line` (concealed for non-cursor lines).
    let render_text = |line: usize| -> String {
        let raw = raw_line_text(line);
        if conceal_active && line != cursor_line {
            let spans = scan_markdown_conceal(&raw);
            if spans.is_empty() {
                raw
            } else {
                apply_conceal(&raw, &spans).text
            }
        } else {
            raw
        }
    };

    // Determine buffer line and full display column, accounting for wrap
    let (buffer_line, display_col) = if editor.options.wrap {
        if let Some(wrap_map) = editor.wrap_map() {
            let viewport_visual_row =
                wrap_map.viewport_top_visual_row(editor.scroll_offset(), editor.scroll_subrow());
            let absolute_visual_row = rel_row + viewport_visual_row;
            let (logical_line, sub_line) = wrap_map.visual_to_logical(absolute_visual_row);
            let line = logical_line.min(editor.buffer().line_count().saturating_sub(1));
            // `sub_line_display_range` must run on the *rendered* text so its
            // segment boundaries match the (conceal-aware) wrap map.
            let row_text = render_text(line);
            let (row_start, _) = wrap_map
                .sub_line_display_range(&row_text, sub_line)
                .unwrap_or((0, 0));
            // In wrap mode, display columns are contiguous across segment starts.
            let col = row_start + display_col_in_row;
            (line, col)
        } else {
            let line = (rel_row + editor.scroll_offset())
                .min(editor.buffer().line_count().saturating_sub(1));
            (line, display_col_in_row + editor.horizontal_offset())
        }
    } else {
        let line =
            (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1));
        (line, display_col_in_row + editor.horizontal_offset())
    };

    // Convert display column to character column using tab/wide-char aware
    // function. For a concealed line, the display column indexes the concealed
    // text — resolve a *view* char index there, then map it back to a source
    // char column via the conceal transform.
    let raw_line = raw_line_text(buffer_line);
    let tab_width = editor.options.tab_width;
    let char_col = if conceal_active && buffer_line != cursor_line {
        let spans = scan_markdown_conceal(&raw_line);
        if spans.is_empty() {
            display_col_to_char_col(&raw_line, display_col, tab_width)
        } else {
            let transform = apply_conceal(&raw_line, &spans);
            let view_char = display_col_to_char_col(&transform.text, display_col, tab_width);
            crate::markdown_conceal::view_char_to_src_char(&raw_line, &transform, view_char)
        }
    } else {
        display_col_to_char_col(&raw_line, display_col, tab_width)
    };
    let grapheme_col = char_to_grapheme_col(&raw_line, crate::unicode::CharCol(char_col)).0;

    // Clamp to line length (Normal mode: last char, Insert mode: past last char)
    let line_len = grapheme_count(&raw_line);
    let max_col = if editor.mode() == Mode::Insert {
        line_len
    } else {
        line_len.saturating_sub(1)
    };
    let clamped_col = grapheme_col.min(max_col);

    Some((buffer_line, clamped_col))
}

/// Checks if a click at the given screen coordinates lands on a concealed markdown link.
/// Returns the URL if it does.
fn check_concealed_link_click(editor: &Editor, screen_col: u16, screen_row: u16) -> Option<String> {
    // Only active for markdown files with concealment on
    if !editor.options.markdown_conceal {
        return None;
    }
    let is_md = editor
        .buffer()
        .file_path()
        .map(|p| p.ends_with(".md"))
        .unwrap_or(false);
    if !is_md {
        return None;
    }

    let area = editor.render_cache.last_buffer_area?;
    let gutter_width = editor.render_cache.last_gutter_width;

    if screen_col < area.x
        || screen_row < area.y
        || screen_col >= area.x + area.width
        || screen_row >= area.y + area.height
    {
        return None;
    }

    let rel_col = (screen_col - area.x) as usize;
    let rel_row = (screen_row - area.y) as usize;

    if rel_col < gutter_width {
        return None;
    }

    let display_col_in_row = rel_col - gutter_width;

    // Determine buffer line (same logic as screen_to_buffer)
    let buffer_line = if editor.options.wrap {
        if let Some(wrap_map) = editor.wrap_map() {
            let viewport_visual_row =
                wrap_map.viewport_top_visual_row(editor.scroll_offset(), editor.scroll_subrow());
            let absolute_visual_row = rel_row + viewport_visual_row;
            let (logical_line, _sub_line) = wrap_map.visual_to_logical(absolute_visual_row);
            logical_line.min(editor.buffer().line_count().saturating_sub(1))
        } else {
            (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
        }
    } else {
        (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
    };

    // Don't check cursor line (concealment is disabled there)
    if buffer_line == editor.buffer().cursor().line() {
        return None;
    }

    let display_col = if editor.options.wrap {
        // In wrap mode, display_col_in_row is the column within the sub-line
        // For simplicity, only handle the first sub-line (sub_line == 0)
        display_col_in_row
    } else {
        display_col_in_row + editor.horizontal_offset()
    };

    // Get the line text and scan for concealed links
    let line_text = editor
        .buffer()
        .line_text(buffer_line)
        .map(|l| l.to_string())
        .unwrap_or_default();

    let spans = scan_markdown_conceal(&line_text);
    if spans.is_empty() {
        return None;
    }

    let transform = apply_conceal(&line_text, &spans);
    let links = extract_concealed_links(&spans, &transform);

    // The display_col is in display space (after tab expansion).
    // The link view_start/view_end are in concealed char space (no tab expansion).
    // For simplicity, treat them as equivalent (links in markdown rarely coexist with tabs).
    for link in &links {
        if display_col >= link.view_start && display_col < link.view_end {
            return Some(link.url.clone());
        }
    }

    None
}

/// Returns the buffer line if the click lands in the blame column area.
fn is_blame_click(editor: &Editor, screen_col: u16, screen_row: u16) -> Option<usize> {
    let area = editor.render_cache.last_buffer_area?;
    let blame_width = editor.render_cache.last_blame_width;

    if blame_width == 0 {
        return None;
    }

    if screen_col < area.x
        || screen_row < area.y
        || screen_col >= area.x + area.width
        || screen_row >= area.y + area.height
    {
        return None;
    }

    let rel_col = (screen_col - area.x) as usize;
    let rel_row = (screen_row - area.y) as usize;

    if rel_col < blame_width {
        let buffer_line = if editor.options.wrap {
            if let Some(wrap_map) = editor.wrap_map() {
                let viewport_visual_row = wrap_map
                    .viewport_top_visual_row(editor.scroll_offset(), editor.scroll_subrow());
                let absolute_visual_row = rel_row + viewport_visual_row;
                let (logical_line, _sub_line) = wrap_map.visual_to_logical(absolute_visual_row);
                logical_line.min(editor.buffer().line_count().saturating_sub(1))
            } else {
                (rel_row + editor.scroll_offset())
                    .min(editor.buffer().line_count().saturating_sub(1))
            }
        } else {
            (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
        };
        Some(buffer_line)
    } else {
        None
    }
}

/// Returns the buffer line if the click lands in the sign column (first 2 chars of gutter).
fn is_sign_column_click(editor: &Editor, screen_col: u16, screen_row: u16) -> Option<usize> {
    let area = editor.render_cache.last_buffer_area?;
    let gutter_width = editor.render_cache.last_gutter_width;
    let blame_width = editor.render_cache.last_blame_width;

    if gutter_width == 0 {
        return None;
    }

    if screen_col < area.x
        || screen_row < area.y
        || screen_col >= area.x + area.width
        || screen_row >= area.y + area.height
    {
        return None;
    }

    let rel_col = (screen_col - area.x) as usize;

    // Sign column starts after blame, spans 2 columns (SIGN_WIDTH)
    let sign_start = blame_width;
    let sign_end = blame_width + 2; // SIGN_WIDTH

    if rel_col < sign_start || rel_col >= sign_end {
        return None;
    }

    let rel_row = (screen_row - area.y) as usize;
    let buffer_line = if editor.options.wrap {
        if let Some(wrap_map) = editor.wrap_map() {
            let viewport_visual_row =
                wrap_map.viewport_top_visual_row(editor.scroll_offset(), editor.scroll_subrow());
            let absolute_visual_row = rel_row + viewport_visual_row;
            let (logical_line, _sub_line) = wrap_map.visual_to_logical(absolute_visual_row);
            logical_line.min(editor.buffer().line_count().saturating_sub(1))
        } else {
            (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
        }
    } else {
        (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
    };
    Some(buffer_line)
}

/// Returns the buffer line if the click lands in the gutter area.
fn is_gutter_click(editor: &Editor, screen_col: u16, screen_row: u16) -> Option<usize> {
    let area = editor.render_cache.last_buffer_area?;
    let gutter_width = editor.render_cache.last_gutter_width;

    if gutter_width == 0 {
        return None;
    }

    if screen_col < area.x
        || screen_row < area.y
        || screen_col >= area.x + area.width
        || screen_row >= area.y + area.height
    {
        return None;
    }

    let rel_col = (screen_col - area.x) as usize;
    let rel_row = (screen_row - area.y) as usize;

    if rel_col < gutter_width {
        let buffer_line = if editor.options.wrap {
            if let Some(wrap_map) = editor.wrap_map() {
                let viewport_visual_row = wrap_map
                    .viewport_top_visual_row(editor.scroll_offset(), editor.scroll_subrow());
                let absolute_visual_row = rel_row + viewport_visual_row;
                let (logical_line, _sub_line) = wrap_map.visual_to_logical(absolute_visual_row);
                logical_line.min(editor.buffer().line_count().saturating_sub(1))
            } else {
                (rel_row + editor.scroll_offset())
                    .min(editor.buffer().line_count().saturating_sub(1))
            }
        } else {
            (rel_row + editor.scroll_offset()).min(editor.buffer().line_count().saturating_sub(1))
        };
        Some(buffer_line)
    } else {
        None
    }
}

/// Whether the current mode is one we should ignore mouse clicks in
/// (except scroll, which is handled separately).
fn should_ignore_click(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Command
            | Mode::Search
            | Mode::HoverPreview
            | Mode::HoverNavigate
            | Mode::Dashboard
            | Mode::FileTree
            | Mode::SubstituteConfirm
    )
}

fn handle_left_click(editor: &mut Editor, col: u16, row: u16) -> Result<Option<String>> {
    let mode = editor.mode();

    if mode == Mode::AiChat {
        if editor.ai_chat_has_exa_setup_dialog() {
            if editor
                .render_cache
                .ai_chat_exa_dashboard_hitbox
                .is_some_and(|area| area.contains(col, row))
            {
                return Ok(Some(editor.ai_chat_exa_dashboard_url().to_string()));
            }
            return Ok(None);
        }
        if let Some(profile) = editor
            .render_cache
            .ai_chat_interactions
            .model_picker_options
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, profile)| profile.clone())
        {
            editor.ai_set_profile(&profile);
            return Ok(None);
        }
        if let Some(effort) = editor
            .render_cache
            .ai_chat_interactions
            .effort_picker_options
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, effort)| effort.clone())
        {
            editor.set_ai_chat_reasoning_effort(&effort);
            return Ok(None);
        }
        if editor
            .render_cache
            .ai_chat_interactions
            .model_picker_trigger
            .is_some_and(|area| area.contains(col, row))
        {
            editor.open_ai_chat_model_picker(crate::editor::ChatModelPickerSection::Model);
            return Ok(None);
        }
        if editor
            .render_cache
            .ai_chat_interactions
            .effort_picker_trigger
            .is_some_and(|area| area.contains(col, row))
        {
            editor.open_ai_chat_model_picker(crate::editor::ChatModelPickerSection::Effort);
            return Ok(None);
        }
        if editor
            .render_cache
            .ai_chat_interactions
            .comprehension_toggle
            .is_some_and(|area| area.contains(col, row))
        {
            editor.toggle_ai_chat_comprehension_policy();
            return Ok(None);
        }
        if editor.ai_chat_image_modal_path().is_some() {
            editor.close_ai_chat_image_modal();
            return Ok(None);
        }
        if editor
            .render_cache
            .ai_chat_separator_area
            .is_some_and(|area| area.contains(col, row))
        {
            editor.render_cache.ai_chat_separator_dragging = true;
            editor.render_cache.ai_chat_text_selection = None;
            editor.render_cache.ai_chat_text_selecting = false;
            editor.clear_ai_chat_text_selection_autoscroll();
            return Ok(None);
        }
        if let Some(index) = editor
            .render_cache
            .ai_chat_interactions
            .slash_completions
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, index)| *index)
        {
            editor.accept_ai_chat_slash_completion(Some(index));
            return Ok(None);
        }
        if editor
            .render_cache
            .ai_chat_interactions
            .yolo_toggle
            .is_some_and(|area| area.contains(col, row))
        {
            editor.toggle_ai_chat_yolo_mode();
            return Ok(None);
        }
        if let Some(path) = editor
            .render_cache
            .ai_chat_image_thumbnails
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, path)| path.clone())
        {
            editor.open_ai_chat_image_modal(path);
            return Ok(None);
        }
        if let Some(target) = editor
            .render_cache
            .ai_chat_interactions
            .branches
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, target)| *target)
        {
            editor.render_cache.ai_chat_text_selection = None;
            editor.render_cache.ai_chat_text_selecting = false;
            editor.clear_ai_chat_text_selection_autoscroll();
            editor.switch_ai_chat_runtime_branch(target);
            return Ok(None);
        }
        if let Some(tool_call_id) = editor
            .render_cache
            .ai_chat_interactions
            .walkthrough_replays
            .iter()
            .find(|(area, _)| area.contains(col, row))
            .map(|(_, tool_call_id)| tool_call_id.clone())
        {
            editor.render_cache.ai_chat_text_selection = None;
            editor.render_cache.ai_chat_text_selecting = false;
            editor.clear_ai_chat_text_selection_autoscroll();
            editor.replay_code_explanation(&tool_call_id);
            return Ok(None);
        }
        if let Some((history_row, history_column)) = ai_chat_screen_position(editor, col, row) {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.focus = crate::ai::chat_types::ChatFocus::MessageHistory;
            }
            editor.begin_ai_chat_text_selection(history_row, history_column);
            return Ok(None);
        }
        if handle_ai_chat_input_click(editor, col, row) {
            return Ok(None);
        }
        if editor
            .render_cache
            .last_chat_area
            .is_some_and(|area| area.contains(col, row))
        {
            editor.render_cache.ai_chat_text_selection = None;
            editor.render_cache.ai_chat_text_selecting = false;
            editor.clear_ai_chat_text_selection_autoscroll();
            return Ok(None);
        }
    }

    // Handle AI prompt panel clicks (model picker + prompt cursor placement).
    if mode == Mode::AiPrompt && handle_ai_prompt_click(editor, col, row)? {
        return Ok(None);
    }

    // Handle picker mode clicks
    if mode == Mode::Picker {
        handle_picker_click(editor, col, row)?;
        return Ok(None);
    }

    // Dismiss transient overlays on click
    if matches!(
        mode,
        Mode::Command | Mode::Search | Mode::HoverPreview | Mode::HoverNavigate
    ) {
        editor.set_mode(Mode::Normal);
        // Fall through to also move cursor
    } else if should_ignore_click(mode) {
        return Ok(None);
    }

    // Exit visual mode if active
    if matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock) {
        editor.set_mode(Mode::Normal);
    }

    // Check concealed markdown link click → open URL
    if let Some(url) = check_concealed_link_click(editor, col, row) {
        return Ok(Some(url));
    }

    // Check blame column click → show blame popup
    if let Some(line) = is_blame_click(editor, col, row) {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol::ZERO);
        editor.show_blame_info();
        return Ok(None);
    }

    // Check sign column click → toggle breakpoint
    if let Some(line) = is_sign_column_click(editor, col, row) {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol::ZERO);
        editor.toggle_breakpoint();
        return Ok(None);
    }

    // Check gutter click → select line (Visual Line mode)
    if let Some(line) = is_gutter_click(editor, col, row) {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol::ZERO);
        editor.set_visual_start(line, 0);
        editor.set_mode(Mode::VisualLine);
        editor.render_cache.mouse_state.is_dragging = true;
        editor.render_cache.mouse_state.drag_origin = Some((line, 0));
        return Ok(None);
    }

    // Buffer click → move cursor
    if let Some((line, char_col)) = screen_to_buffer(editor, col, row) {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(char_col));
        editor.render_cache.mouse_state.is_dragging = true;
        editor.render_cache.mouse_state.drag_origin = Some((line, char_col));
    }

    Ok(None)
}

fn handle_ai_chat_input_click(editor: &mut Editor, col: u16, row: u16) -> bool {
    let Some(input_area) = editor.render_cache.ai_chat_input_area else {
        return false;
    };
    if !input_area.contains(col, row) {
        return false;
    }

    let hit = editor
        .render_cache
        .ai_chat_input_rows
        .iter()
        .find(|(rect, _, _, _)| rect.contains(col, row))
        .copied();
    let cursor = if let Some((rect, start, visible_start, end)) = hit {
        let input = editor.ai_chat_input();
        chat_input_byte_for_display_column(
            input,
            ChatInputRow {
                start,
                visible_start,
                end,
            },
            col.saturating_sub(rect.x) as usize,
            editor.options.tab_width,
        )
    } else {
        editor.ai_chat_input().len()
    };

    if let Some(chat) = editor.ai_state.chat.as_mut() {
        chat.focus = crate::ai::chat_types::ChatFocus::TextInput;
        chat.input_cursor = cursor;
    }
    editor.render_cache.ai_chat_text_selection = None;
    editor.render_cache.ai_chat_text_selecting = false;
    editor.clear_ai_chat_text_selection_autoscroll();
    true
}

fn handle_left_drag(editor: &mut Editor, col: u16, row: u16) -> Result<()> {
    if editor.render_cache.ai_chat_separator_dragging {
        if let Some(split_area) = editor.render_cache.ai_chat_split_area {
            editor.resize_ai_chat_panel(col, split_area);
        }
        return Ok(());
    }

    if editor.render_cache.ai_chat_text_selecting {
        if let Some(area) = editor.render_cache.ai_chat_interactions.history {
            let column = col.saturating_sub(area.x).min(area.width.saturating_sub(1)) as usize;
            if row < area.y {
                editor.set_ai_chat_text_selection_autoscroll(
                    crate::editor::render_cache::ChatTextAutoscrollDirection::Older,
                    column,
                );
                return Ok(());
            }
            if row >= area.y.saturating_add(area.height) {
                editor.set_ai_chat_text_selection_autoscroll(
                    crate::editor::render_cache::ChatTextAutoscrollDirection::Newer,
                    column,
                );
                return Ok(());
            }
        }
        editor.clear_ai_chat_text_selection_autoscroll();
        if let Some((history_row, history_column)) = ai_chat_screen_position(editor, col, row) {
            editor.update_ai_chat_text_selection(history_row, history_column);
        }
        return Ok(());
    }

    if !editor.render_cache.mouse_state.is_dragging {
        return Ok(());
    }

    let mode = editor.mode();
    if should_ignore_click(mode) && mode != Mode::Picker {
        return Ok(());
    }

    if let Some((line, char_col)) = screen_to_buffer(editor, col, row) {
        // Enter visual mode on first drag movement (if not already visual)
        if !matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock) {
            if let Some((origin_line, origin_col)) = editor.render_cache.mouse_state.drag_origin {
                editor.set_visual_start(origin_line, origin_col);
            }
            // If we started from a gutter click, stay in VisualLine
            if editor.mode() != Mode::VisualLine {
                editor.set_mode(Mode::Visual);
            }
        }

        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(char_col));
    }

    Ok(())
}

fn handle_left_release(editor: &mut Editor) -> Result<()> {
    if editor.render_cache.ai_chat_separator_dragging {
        editor.render_cache.ai_chat_separator_dragging = false;
        return Ok(());
    }
    if editor.render_cache.ai_chat_text_selecting {
        editor.finish_ai_chat_text_selection();
        return Ok(());
    }
    editor.render_cache.mouse_state.is_dragging = false;
    Ok(())
}

fn ai_chat_screen_position(editor: &Editor, col: u16, row: u16) -> Option<(usize, usize)> {
    let area = editor.render_cache.ai_chat_interactions.history?;
    if !area.contains(col, row) {
        return None;
    }
    let history_row = editor
        .render_cache
        .ai_chat_last_visible_start_row
        .saturating_add((row - area.y) as usize);
    if history_row >= editor.render_cache.ai_chat_last_visible_end_row
        || history_row >= editor.render_cache.ai_chat_rendered_text_rows.len()
    {
        return None;
    }
    Some((history_row, (col - area.x) as usize))
}

fn handle_scroll(editor: &mut Editor, up: bool, col: u16, row: u16) -> Result<()> {
    const SCROLL_LINES: usize = 3;

    // In picker mode, scroll the picker results
    if editor.mode() == Mode::Picker {
        if let Some(picker) = editor.picker_mut() {
            for _ in 0..SCROLL_LINES {
                if up {
                    picker.move_up();
                } else {
                    picker.move_down();
                }
            }
            editor.mark_picker_selection_changed();
        }
        return Ok(());
    }

    // In AiChat mode, scroll the chat message history if the mouse is over the chat area
    if editor.mode() == Mode::AiChat {
        if let Some(chat_rect) = editor.render_cache.last_chat_area {
            if chat_rect.contains(col, row) {
                if up {
                    editor.ai_chat_scroll_viewport_up(SCROLL_LINES);
                } else {
                    editor.ai_chat_scroll_viewport_down(SCROLL_LINES);
                }
                return Ok(());
            }
        }
    }

    // In wrap mode, scroll by visual rows instead of logical lines.
    // We compute the target logical line from visual row arithmetic and
    // set the scroll offset directly via scroll_viewport_up/down with
    // the appropriate logical line delta.
    if editor.options.wrap {
        if let Some(wrap_map) = editor.wrap_map() {
            let scroll_offset = editor.scroll_offset();
            let current_visual = wrap_map.logical_to_visual(scroll_offset);
            let target_visual = if up {
                current_visual.saturating_sub(SCROLL_LINES)
            } else {
                (current_visual + SCROLL_LINES).min(wrap_map.total_visual_lines().saturating_sub(1))
            };
            let (target_line, _) = wrap_map.visual_to_logical(target_visual);
            if target_line < scroll_offset {
                editor.scroll_viewport_up(scroll_offset - target_line);
            } else if target_line > scroll_offset {
                editor.scroll_viewport_down(target_line - scroll_offset);
            }
            return Ok(());
        }
    }

    if up {
        editor.scroll_viewport_up(SCROLL_LINES);
    } else {
        editor.scroll_viewport_down(SCROLL_LINES);
    }

    Ok(())
}

fn handle_middle_click(editor: &mut Editor, col: u16, row: u16) -> Result<()> {
    let mode = editor.mode();
    if should_ignore_click(mode) {
        return Ok(());
    }

    // Move cursor to click position first
    if let Some((line, char_col)) = screen_to_buffer(editor, col, row) {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(char_col));
    }

    // Paste from system clipboard register (+)
    let text = editor.registers().get(Some('+'));
    if !text.is_empty() {
        editor.handle_paste_event(&text)?;
    }

    Ok(())
}

/// Handles a left click inside the picker overlay.
fn handle_picker_click(editor: &mut Editor, col: u16, row: u16) -> Result<()> {
    use crate::editor::PickerField;

    let layout = match &editor.picker_state.last_layout {
        Some(l) => l.clone(),
        None => return Ok(()),
    };

    let point = (col, row);

    // Hit-test: query field
    if rect_contains(&layout.query_field, point) {
        if let Some(picker) = editor.picker_mut() {
            picker.set_active_field(PickerField::Query);
        }
        return Ok(());
    }

    // Hit-test: filter field (LiveGrep only)
    if let Some(ref filter_rect) = layout.filter_field {
        if rect_contains(filter_rect, point) {
            if let Some(picker) = editor.picker_mut() {
                picker.set_active_field(PickerField::FileFilter);
            }
            return Ok(());
        }
    }

    // Hit-test: results area
    if rect_contains(&layout.results_area, point) {
        let row_in_results = (row - layout.results_area.y) as usize;
        let clicked_index = layout.results_scroll_offset + row_in_results;

        if let Some(picker) = editor.picker_mut() {
            picker.set_selected_index(clicked_index);
        }
        editor.mark_picker_selection_changed();
        return Ok(());
    }

    Ok(())
}

/// Handles a left click inside the AI prompt panel.
/// Returns true when the click was consumed.
fn handle_ai_prompt_click(editor: &mut Editor, col: u16, row: u16) -> Result<bool> {
    if let Some(trigger) = editor.render_cache.ai_prompt_model_trigger_hitbox {
        if trigger.contains(col, row) {
            editor.ai_toggle_model_picker();
            return Ok(true);
        }
    }

    for (rect, profile_name) in editor.render_cache.ai_prompt_model_hitboxes.clone() {
        if rect.contains(col, row) {
            editor.ai_set_profile(&profile_name);
            editor.ai_close_model_picker();
            return Ok(true);
        }
    }

    if editor.ai_state.prompt.model_picker_open {
        editor.ai_close_model_picker();
    }

    let prompt = editor.ai_prompt_input().to_string();
    let prompt_len = prompt.len();
    for (input_rect, start_byte, end_byte) in &editor.render_cache.ai_prompt_input_rows {
        if input_rect.contains(col, row) {
            let rel_display_col = (col.saturating_sub(input_rect.x)) as usize;
            let row_start = (*start_byte).min(prompt_len);
            let row_end = (*end_byte).min(prompt_len).max(row_start);
            let new_cursor = prompt_cursor_from_display_col_in_range(
                &prompt,
                row_start,
                row_end,
                rel_display_col,
            );
            editor.ai_state.prompt.cursor = new_cursor;
            return Ok(true);
        }
    }

    if let Some(input_rect) = editor.render_cache.ai_prompt_input_area {
        if input_rect.contains(col, row) {
            editor.ai_state.prompt.cursor = prompt_len;
            return Ok(true);
        }
    }

    Ok(false)
}

fn prompt_cursor_from_display_col_in_range(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    display_col: usize,
) -> usize {
    let mut width = 0;
    for (rel_idx, ch) in text[start_byte..end_byte].char_indices() {
        let byte_idx = start_byte + rel_idx;
        let ch_width = char_display_width(ch);
        if width + ch_width > display_col {
            return byte_idx;
        }
        width += ch_width;
    }
    end_byte
}

/// Returns true if the screen point (col, row) is inside the rect.
fn rect_contains(rect: &crate::Rect, point: (u16, u16)) -> bool {
    rect.contains(point.0, point.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::editor::ai_chat_state::AiChatState;
    use crate::editor::render_cache::{
        ChatTextAutoscrollDirection, ChatTextPoint, ChatTextSelection,
    };

    fn editor_with_docked_chat() -> Editor {
        let mut editor = Editor::default();
        let buffer_id = editor.buffer().id();
        editor.ai_state.chat = Some(AiChatState::new(
            ChatOpts::default(),
            buffer_id,
            Mode::Normal,
        ));
        editor.set_mode(Mode::AiChat);
        editor.render_cache.last_chat_area = Some(crate::Rect {
            x: 40,
            y: 0,
            width: 40,
            height: 20,
        });
        editor.render_cache.ai_chat_last_total_rows = 100;
        editor
    }

    #[test]
    fn dragging_chat_separator_resizes_panel_and_owns_gesture() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_split_area = Some(crate::Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 20,
        });
        editor.render_cache.ai_chat_separator_area = Some(crate::Rect {
            x: 60,
            y: 0,
            width: 1,
            height: 20,
        });

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 60,
                row: 5,
            },
        )
        .unwrap();
        assert!(editor.render_cache.ai_chat_separator_dragging);

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 45,
                row: 5,
            },
        )
        .unwrap();
        assert_eq!(editor.ai_chat_panel_width_percent(), Some(55));
        assert!(!editor.ai_chat_has_text_selection());

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 45,
                row: 5,
            },
        )
        .unwrap();
        assert!(!editor.render_cache.ai_chat_separator_dragging);
    }

    #[test]
    fn clicking_slash_completion_accepts_that_command() {
        let mut editor = editor_with_docked_chat();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "/".into();
        chat.input_cursor = 1;
        editor.render_cache.ai_chat_interactions.slash_completions = vec![(
            crate::Rect {
                x: 42,
                y: 12,
                width: 30,
                height: 1,
            },
            2,
        )];

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 50,
                row: 12,
            },
        )
        .unwrap();

        assert_eq!(editor.ai_chat_input(), "/model");
    }

    #[test]
    fn clicking_chat_picker_trigger_and_effort_option_updates_chat() {
        let mut editor = editor_with_docked_chat();
        editor
            .render_cache
            .ai_chat_interactions
            .effort_picker_trigger = Some(crate::Rect {
            x: 42,
            y: 0,
            width: 10,
            height: 1,
        });

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 0,
            },
        )
        .unwrap();
        assert_eq!(
            editor.ai_chat_model_picker_section(),
            crate::editor::ChatModelPickerSection::Effort
        );
        assert_eq!(
            editor.ai_chat_focus(),
            crate::ai::chat_types::ChatFocus::ModelSelector
        );

        editor
            .render_cache
            .ai_chat_interactions
            .effort_picker_options = vec![(
            crate::Rect {
                x: 42,
                y: 2,
                width: 20,
                height: 1,
            },
            "high".into(),
        )];
        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 2,
            },
        )
        .unwrap();

        assert_eq!(editor.ai_chat_reasoning_effort(), "high");
    }

    #[test]
    fn dragging_chat_text_highlights_and_copies_the_selection() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.history = Some(crate::Rect {
            x: 40,
            y: 1,
            width: 40,
            height: 4,
        });
        editor.render_cache.ai_chat_last_visible_start_row = 0;
        editor.render_cache.ai_chat_last_visible_end_row = 2;
        editor.render_cache.ai_chat_rendered_text_rows = vec![
            "\u{258d} alpha   ".to_string(),
            "\u{258d} beta    ".to_string(),
        ];

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 42,
                row: 1,
            },
        )
        .unwrap();
        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 45,
                row: 2,
            },
        )
        .unwrap();
        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 45,
                row: 2,
            },
        )
        .unwrap();

        assert_eq!(editor.registers.get_clipboard(), "alpha\nbeta");
        assert!(editor.ai_chat_has_text_selection());
    }

    #[test]
    fn dragging_above_chat_history_keeps_scrolling_and_extending_selection() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.history = Some(crate::Rect {
            x: 40,
            y: 5,
            width: 40,
            height: 4,
        });
        editor.render_cache.ai_chat_last_visible_start_row = 40;
        editor.render_cache.ai_chat_last_visible_end_row = 44;
        editor.render_cache.ai_chat_rendered_text_rows =
            (0..100).map(|row| format!("row {row}")).collect();

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 7,
            },
        )
        .unwrap();
        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 45,
                row: 0,
            },
        )
        .unwrap();

        assert_eq!(
            editor
                .render_cache
                .ai_chat_text_autoscroll
                .expect("autoscroll")
                .direction,
            ChatTextAutoscrollDirection::Older
        );
        assert!(editor.tick_ai_chat_text_selection_autoscroll());
        assert_eq!(
            editor
                .ai_state
                .chat
                .as_ref()
                .expect("chat")
                .viewport
                .row_scroll_from_bottom,
            1
        );
        assert_eq!(
            editor
                .render_cache
                .ai_chat_text_selection
                .expect("selection")
                .head,
            ChatTextPoint { row: 39, column: 5 }
        );
    }

    #[test]
    fn dragging_below_chat_history_scrolls_toward_latest_until_reentering() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.history = Some(crate::Rect {
            x: 40,
            y: 5,
            width: 40,
            height: 4,
        });
        editor.render_cache.ai_chat_last_visible_start_row = 40;
        editor.render_cache.ai_chat_last_visible_end_row = 44;
        editor.render_cache.ai_chat_rendered_text_rows =
            (0..100).map(|row| format!("row {row}")).collect();
        {
            let viewport = &mut editor.ai_state.chat.as_mut().expect("chat").viewport;
            viewport.follow_latest = false;
            viewport.row_scroll_from_bottom = 10;
            viewport.pinned_base_total_rows = Some(100);
        }

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 6,
            },
        )
        .unwrap();
        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 50,
                row: 12,
            },
        )
        .unwrap();

        assert_eq!(
            editor
                .render_cache
                .ai_chat_text_autoscroll
                .expect("autoscroll")
                .direction,
            ChatTextAutoscrollDirection::Newer
        );
        assert!(editor.tick_ai_chat_text_selection_autoscroll());
        assert_eq!(
            editor
                .render_cache
                .ai_chat_text_selection
                .expect("selection")
                .head,
            ChatTextPoint {
                row: 44,
                column: 10,
            }
        );

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 47,
                row: 7,
            },
        )
        .unwrap();
        assert!(editor.render_cache.ai_chat_text_autoscroll.is_none());
        assert_eq!(
            editor
                .render_cache
                .ai_chat_text_selection
                .expect("selection")
                .head,
            ChatTextPoint { row: 42, column: 7 }
        );
    }

    #[test]
    fn clicking_a_wrapped_chat_input_row_places_the_cursor() {
        let mut editor = editor_with_docked_chat();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "first second".to_string();
        chat.input_cursor = chat.input.len();
        editor.render_cache.ai_chat_input_area = Some(crate::Rect {
            x: 40,
            y: 15,
            width: 40,
            height: 3,
        });
        editor.render_cache.ai_chat_input_rows = vec![(
            crate::Rect {
                x: 45,
                y: 16,
                width: 20,
                height: 1,
            },
            6,
            6,
            12,
        )];

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 47,
                row: 16,
            },
        )
        .unwrap();

        assert_eq!(editor.ai_chat_input_cursor(), 8);
        assert_eq!(
            editor.ai_chat_focus(),
            crate::ai::chat_types::ChatFocus::TextInput
        );
    }

    #[test]
    fn clicking_chat_thumbnail_opens_and_closes_the_image_modal() {
        let mut editor = editor_with_docked_chat();
        let path = std::path::PathBuf::from("/tmp/example.png");
        editor.render_cache.ai_chat_image_thumbnails = vec![(
            crate::Rect {
                x: 42,
                y: 12,
                width: 10,
                height: 4,
            },
            path.clone(),
        )];

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 13,
            },
        )
        .unwrap();

        assert_eq!(editor.ai_chat_image_modal_path(), Some(path.as_path()));

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
            },
        )
        .unwrap();

        assert!(editor.ai_chat_image_modal_path().is_none());
    }

    #[test]
    fn clicking_chat_yolo_toggle_changes_per_chat_policy() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.yolo_toggle = Some(crate::Rect {
            x: 69,
            y: 0,
            width: 11,
            height: 1,
        });

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 74,
                row: 0,
            },
        )
        .unwrap();
        assert!(editor.ai_chat_yolo_mode());

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 74,
                row: 0,
            },
        )
        .unwrap();
        assert!(!editor.ai_chat_yolo_mode());
    }

    #[test]
    fn clicking_branch_control_takes_priority_over_text_selection() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.branches = vec![(
            crate::Rect {
                x: 70,
                y: 4,
                width: 8,
                height: 1,
            },
            42,
        )];
        editor.render_cache.ai_chat_text_selection = Some(ChatTextSelection {
            anchor: ChatTextPoint { row: 0, column: 0 },
            head: ChatTextPoint { row: 0, column: 1 },
            moved: true,
        });
        editor.render_cache.ai_chat_text_selecting = true;

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 72,
                row: 4,
            },
        )
        .unwrap();

        assert!(editor.render_cache.ai_chat_text_selection.is_none());
        assert!(!editor.render_cache.ai_chat_text_selecting);
    }

    #[test]
    fn clicking_walkthrough_replay_takes_priority_over_text_selection() {
        let mut editor = editor_with_docked_chat();
        editor.render_cache.ai_chat_interactions.walkthrough_replays = vec![(
            crate::Rect {
                x: 68,
                y: 4,
                width: 10,
                height: 1,
            },
            "walkthrough-call".into(),
        )];
        editor.render_cache.ai_chat_text_selection = Some(ChatTextSelection {
            anchor: ChatTextPoint { row: 0, column: 0 },
            head: ChatTextPoint { row: 0, column: 1 },
            moved: true,
        });
        editor.render_cache.ai_chat_text_selecting = true;

        handle_mouse_event(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 72,
                row: 4,
            },
        )
        .unwrap();

        assert!(editor.render_cache.ai_chat_text_selection.is_none());
        assert!(!editor.render_cache.ai_chat_text_selecting);
    }

    #[test]
    fn scrolling_editor_side_does_not_scroll_docked_chat() {
        let mut editor = editor_with_docked_chat();

        handle_scroll(&mut editor, true, 10, 5).expect("scroll editor");

        assert_eq!(
            editor
                .ai_chat_state()
                .expect("chat")
                .viewport
                .row_scroll_from_bottom,
            0
        );
    }

    #[test]
    fn scrolling_chat_side_scrolls_docked_chat() {
        let mut editor = editor_with_docked_chat();

        handle_scroll(&mut editor, true, 50, 5).expect("scroll chat");

        assert_eq!(
            editor
                .ai_chat_state()
                .expect("chat")
                .viewport
                .row_scroll_from_bottom,
            3
        );
    }
}
