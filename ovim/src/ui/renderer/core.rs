use crate::editor::{Editor, SplitDirection, WindowViewNode};
use crate::syntax::Theme;
use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::terminal::SetTitle;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal as RatatuiTerminal,
};
use std::io;

use super::buffer::{render_buffer, WindowRenderContext};
use super::dashboard::render_dashboard;
use super::file_tree_widget::render_file_tree;
use super::layout::{BufferLayout, OverlayContext};
use super::line_cache::LineRenderCache;
use super::overlays::{
    render_ai_chat_exa_setup_dialog, render_ai_chat_permission_dialog, render_ai_review_shortcuts,
    render_completion_menu, render_hover_window, render_lsp_install_dialog,
};
use super::picker_widget::{render_picker, Fill};
use super::status_widgets::{
    ai_prompt_panel_height, render_ai_prompt_line, render_command_line, render_margin_widgets,
    render_message_line, render_path_completion, render_progress_line, render_rename_input,
    render_search_line, render_status_line, render_tab_bar, render_top_right_toasts,
};

// ---------------------------------------------------------------------------
// Frame layout types
// ---------------------------------------------------------------------------

/// Areas computed from the frame layout (tab bar, file tree, buffer, status, command, progress).
struct FrameAreas {
    tab_area: Option<Rect>,
    file_tree_area: Option<Rect>,
    buffer_chunk: Rect,
    status_chunk: Rect,
    command_chunk: Rect,
    progress_chunk: Option<Rect>,
    chat_area: Option<Rect>,
    debug_side_area: Option<Rect>,
    debug_output_area: Option<Rect>,
}

// ---------------------------------------------------------------------------
// Extracted render phases (free functions)
// ---------------------------------------------------------------------------

/// Phase 1: Initialize the window manager for the current terminal size.
///
/// Ratatui's double-buffer diff handles clearing stale cells automatically —
/// we don't need to paint a full blank background every frame. The previous
/// implementation allocated `" ".repeat(width) × height` strings and rendered
/// a full-screen paragraph on every frame, which was pure overhead.
fn init_frame(frame: &Frame, editor: &mut Editor) {
    let area = frame.area();
    editor.init_window_manager(area.width, area.height);
}

/// Phase 2: Compute the frame layout (tab bar, file tree, buffer, status splits).
///
/// Returns `None` if the editor is in dashboard mode (caller should render
/// the dashboard and return early).
fn compute_frame_layout(frame: &Frame, editor: &Editor) -> Option<FrameAreas> {
    if editor.should_show_dashboard() {
        return None;
    }

    let main_area = frame.area();

    // Tab bar (if multiple tabs) + rest
    let (tab_area, remaining_area) = if editor.tab_count() > 1 {
        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)].as_ref())
            .split(main_area);
        (Some(vertical_chunks[0]), vertical_chunks[1])
    } else {
        (None, main_area)
    };

    // File tree (if visible) + rest
    let (file_tree_area, content_area) = if editor.file_tree().is_visible() {
        let explorer_width = editor.file_tree().preferred_width(remaining_area.width);
        let horizontal_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(explorer_width), Constraint::Min(1)].as_ref())
            .split(remaining_area);
        (Some(horizontal_chunks[0]), horizontal_chunks[1])
    } else {
        (None, remaining_area)
    };

    // Debug panels (if visible and session active)
    let debug_panels_visible =
        editor.debug_state().panels_visible && editor.debug_state().session_active;

    // Debug side panel (right) — split from content area
    let (content_area, debug_side_area) = if debug_panels_visible {
        let width = (content_area.width / 3).clamp(25, 50);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(width)])
            .split(content_area);
        (chunks[0], Some(chunks[1]))
    } else {
        (content_area, None)
    };

    // Debug output panel (bottom) — split from content area
    let (content_area, debug_output_area) =
        if debug_panels_visible && !editor.debug_state().output_lines.is_empty() {
            let height = 6u16.min(content_area.height / 4);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(height)])
                .split(content_area);
            (chunks[0], Some(chunks[1]))
        } else {
            (content_area, None)
        };

    // Buffer + optional progress line + status line + command/prompt area
    let has_progress = editor.lsp_progress_message().is_some();
    let is_ai_chat = editor.mode() == crate::mode::Mode::AiChat;
    let command_height = if editor.mode() == crate::mode::Mode::AiPrompt {
        let max_height = content_area
            .height
            .saturating_sub(if has_progress { 2 } else { 1 })
            .max(1);
        ai_prompt_panel_height(editor, content_area.width, max_height)
    } else {
        1
    };

    // Interactive walkthroughs temporarily dedicate the shared content width
    // to code. The chat remains alive and resumes as soon as the walkthrough
    // completes or is dismissed.
    let review_mode = editor.ai_chat_review_mode();
    let walkthrough_open = editor.ai_chat_has_pending_code_explanation();
    let (effective_content, chat_area) =
        if should_dock_ai_chat(is_ai_chat, review_mode, walkthrough_open) {
            let allow_edits = editor.ai_chat_allow_edits();
            let (buffer_rect, chat_rect) = super::ai_chat::compute_chat_split(
                content_area,
                allow_edits,
                editor.ai_chat_panel_width_percent(),
            );
            (buffer_rect, Some(chat_rect))
        } else {
            (content_area, None)
        };

    let chunks = if has_progress {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Min(1),
                    Constraint::Length(1),              // progress line
                    Constraint::Length(1),              // status line
                    Constraint::Length(command_height), // command/message line
                ]
                .as_ref(),
            )
            .split(effective_content)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Min(1),
                    Constraint::Length(1),              // status line
                    Constraint::Length(command_height), // command/message line
                ]
                .as_ref(),
            )
            .split(effective_content)
    };

    let (status_chunk, command_chunk, progress_chunk) = if has_progress {
        (chunks[2], chunks[3], Some(chunks[1]))
    } else {
        (chunks[1], chunks[2], None)
    };

    Some(FrameAreas {
        tab_area,
        file_tree_area,
        buffer_chunk: chunks[0],
        status_chunk,
        command_chunk,
        progress_chunk,
        chat_area,
        debug_side_area,
        debug_output_area,
    })
}

fn should_dock_ai_chat(is_ai_chat: bool, review_mode: bool, walkthrough_open: bool) -> bool {
    is_ai_chat && !review_mode && !walkthrough_open
}

/// Phase 3: Render the buffer area (split or single window), returning
/// the viewport start line and the focused window's layout.
fn render_buffer_area(
    frame: &mut Frame,
    editor: &mut Editor,
    theme: &Theme,
    areas: &FrameAreas,
    line_cache: &mut LineRenderCache,
) -> (usize, BufferLayout) {
    let has_splits = editor
        .window_manager()
        .map(|wm| wm.root().count_windows() > 1)
        .unwrap_or(false);

    if has_splits {
        // Each split pane builds its *own* wrap map at its *own* content width
        // inside `render_window_tree`'s leaf handler (roadmap 19 / OV-00209), so
        // there's no global pre-pass here: a `ensure_wrap_map(estimated_width)`
        // call would only be overwritten the moment the focused leaf rebuilds at
        // its real pane width.
        //
        // Render split windows recursively
        if let Some(wm) = editor.window_manager() {
            let focused_index = wm.focused_window_index();
            // Structure-only snapshot: drops the `&WindowManager` borrow so the
            // walk can take `&mut editor` (to (re)build each pane's wrap map),
            // without `clone()`-ing every pane's wrap map vectors. (OV-00015)
            let root = wm.root().view_tree();
            let mut current_index = 0;
            let mut tree_ctx = RenderTreeContext {
                frame,
                editor,
                theme,
                focused_index,
                current_index: &mut current_index,
                line_cache,
            };
            if let Some((vs, ly)) = render_window_tree(&mut tree_ctx, &root, areas.buffer_chunk) {
                (vs, ly)
            } else {
                let fallback_layout = BufferLayout::compute(editor, areas.buffer_chunk);
                let viewport_start =
                    render_buffer(frame, editor, theme, &fallback_layout, line_cache, None);
                (viewport_start, fallback_layout)
            }
        } else {
            let fallback_layout = BufferLayout::compute(editor, areas.buffer_chunk);
            let viewport_start =
                render_buffer(frame, editor, theme, &fallback_layout, line_cache, None);
            (viewport_start, fallback_layout)
        }
    } else {
        // Single window — apply textwidth centering if set
        let buffer_area = if let Some(textwidth) = editor.options.textwidth {
            let max_width = textwidth as u16;
            if areas.buffer_chunk.width > max_width {
                let margin = (areas.buffer_chunk.width - max_width) / 2;

                // Render margin shading if configured
                if let crate::editor::MarginColor::Solid(r, g, b) = editor.options.margin_color {
                    let padding = editor.options.margin_padding as u16;
                    let shaded_margin = margin.saturating_sub(padding);
                    if shaded_margin > 0 {
                        let color = Color::Rgb(r, g, b);
                        let chunk = areas.buffer_chunk;
                        // Left margin shading
                        let left = Rect {
                            x: chunk.x,
                            y: chunk.y,
                            width: shaded_margin,
                            height: chunk.height,
                        };
                        frame.render_widget(Fill::bg(color), left);
                        // Right margin shading
                        let right_x = chunk.x + margin + max_width + padding;
                        let right_width = (chunk.x + chunk.width).saturating_sub(right_x);
                        if right_width > 0 {
                            let right = Rect {
                                x: right_x,
                                y: chunk.y,
                                width: right_width,
                                height: chunk.height,
                            };
                            frame.render_widget(Fill::bg(color), right);
                        }
                    }
                }

                Rect {
                    x: areas.buffer_chunk.x + margin,
                    y: areas.buffer_chunk.y,
                    width: max_width,
                    height: areas.buffer_chunk.height,
                }
            } else {
                areas.buffer_chunk
            }
        } else {
            areas.buffer_chunk
        };

        let full_area = areas.buffer_chunk;
        let centered = buffer_area.width < full_area.width;
        // In centered mode, lines render into the full pane (so EOL
        // diagnostics can extend into the right margin); the code-box
        // (text_width / wrap target / cursor coords) stays anchored to
        // buffer_area.
        let single_layout = if centered {
            BufferLayout::compute_with_render_area(editor, buffer_area, full_area)
        } else {
            BufferLayout::compute(editor, buffer_area)
        };

        if editor.options.wrap {
            editor.ensure_wrap_map(single_layout.text_width);
        }

        let viewport_start = render_buffer(frame, editor, theme, &single_layout, line_cache, None);
        if centered {
            render_margin_widgets(frame, editor, theme, full_area, buffer_area);
        }
        (viewport_start, single_layout)
    }
}

/// Phase 4: Render the status area (progress line + status line + command/message line).
fn render_status_area(frame: &mut Frame, editor: &mut Editor, theme: &Theme, areas: &FrameAreas) {
    if let Some(progress_chunk) = areas.progress_chunk {
        if let Some(progress_msg) = editor.lsp_progress_message() {
            render_progress_line(frame, &progress_msg, progress_chunk);
        }
    }

    // Status line is always visible (mode, filename, position, diagnostics, LSP)
    render_status_line(frame, editor, theme, areas.status_chunk);

    // Command/message line below the status line
    editor.render_cache.ai_prompt_input_area = None;
    editor.render_cache.ai_prompt_input_rows.clear();
    editor.render_cache.ai_prompt_model_hitboxes.clear();
    editor.render_cache.ai_prompt_model_trigger_hitbox = None;
    if editor.mode() == crate::mode::Mode::Command {
        render_command_line(frame, editor, areas.command_chunk);
    } else if editor.mode() == crate::mode::Mode::Search {
        render_search_line(frame, editor, areas.command_chunk);
    } else if editor.mode() == crate::mode::Mode::RenameInput {
        render_rename_input(frame, editor, areas.command_chunk);
    } else if editor.mode() == crate::mode::Mode::AiPrompt {
        let layout = render_ai_prompt_line(frame, editor, areas.command_chunk);
        editor.render_cache.ai_prompt_input_area = layout.input_area;
        editor.render_cache.ai_prompt_input_rows = layout.input_rows;
        editor.render_cache.ai_prompt_model_hitboxes = layout.model_hitboxes;
        editor.render_cache.ai_prompt_model_trigger_hitbox = layout.model_trigger_hitbox;
    } else {
        render_message_line(frame, editor, areas.command_chunk);
    }
}

/// Phase 5: Render overlay widgets (picker, hover, completion, path completion).
fn render_overlays(
    frame: &mut Frame,
    editor: &mut Editor,
    theme: &Theme,
    ctx: &OverlayContext,
    command_chunk: Rect,
) {
    if editor.mode() == crate::mode::Mode::AiChat && editor.ai_chat_review_mode() {
        render_ai_review_shortcuts(frame, theme, ctx.layout.buffer_area);
    }

    // Top-right toast overlays (diagnostics + transient notifications) — hidden during full-screen overlays
    let mode = editor.mode();
    let blocking_modal_active = has_blocking_modal(editor);
    let hide_toasts = matches!(
        mode,
        crate::mode::Mode::Picker
            | crate::mode::Mode::LspManager
            | crate::mode::Mode::HoverPreview
            | crate::mode::Mode::HoverNavigate
    ) || (mode == crate::mode::Mode::AiChat && editor.ai_chat_review_mode())
        || blocking_modal_active;
    if !hide_toasts {
        render_top_right_toasts(frame, editor, theme, ctx.layout.buffer_area);
    }

    // LSP Manager overlay
    if editor.mode() == crate::mode::Mode::LspManager {
        if let Some(panel) = editor.lsp_manager_panel() {
            super::lsp_manager::render_lsp_manager(frame, panel);
        }
    }

    // Picker overlay
    if editor.mode() == crate::mode::Mode::Picker {
        render_picker(frame, editor);
    }

    // Hover window
    if editor.mode().is_hover() {
        if let Some(hover_text) = editor.hover_info() {
            let is_preview = editor.mode() == crate::mode::Mode::HoverPreview;
            let hover_pos = editor.hover_position();
            let content_type = editor.hover_content_type();
            render_hover_window(
                frame,
                editor,
                hover_text,
                editor.hover_scroll(),
                ctx,
                hover_pos,
                is_preview,
                theme,
                content_type,
            );
        }
    }

    // Completion menu (LSP)
    if editor.completion_menu().is_visible() {
        render_completion_menu(frame, editor, ctx);
    }

    // Path completion popup (command mode)
    if editor.path_completion().is_visible() {
        render_path_completion(frame, editor, command_chunk);
    }
}

fn has_blocking_modal(editor: &Editor) -> bool {
    editor.has_pending_lsp_install()
        || editor.has_codex_auth_dialog()
        || editor.ai_chat_has_exa_setup_dialog()
        || editor.ai_chat_image_modal_path().is_some()
        || editor.ai_shell_inspector_is_open()
        || (editor.mode() == crate::mode::Mode::AiChat
            && (editor.ai_chat_has_pending_tool_approval()
                || editor.ai_chat_has_pending_no_repo_folder_approval()))
}

/// Render centered, blocking overlays after all other popup classes.
///
/// This tier is reserved for workflows that block agent/user progress until
/// explicitly resolved. Keep these dialogs highly visible and singular.
fn render_blocking_modals(frame: &mut Frame, editor: &mut Editor, theme: &Theme) {
    if editor.has_pending_lsp_install() {
        render_lsp_install_dialog(frame, editor, theme);
    } else if editor.has_codex_auth_dialog() {
        super::overlays::render_codex_auth_dialog(frame, editor);
    } else if editor.ai_chat_has_exa_setup_dialog() {
        render_ai_chat_exa_setup_dialog(frame, editor);
    } else if editor.ai_chat_image_modal_path().is_some() {
        super::overlays::render_ai_chat_image_modal_frame(frame, editor);
    } else if editor.ai_shell_inspector_is_open() {
        super::overlays::render_ai_shell_process_inspector(frame, editor);
    } else if has_blocking_modal(editor) {
        render_ai_chat_permission_dialog(frame, editor, theme);
    }
}

/// Sets the hardware cursor position based on the current mode.
fn set_cursor_position(
    frame: &mut Frame,
    editor: &mut Editor,
    ctx: &OverlayContext,
    command_chunk: Rect,
    chat_area: Option<Rect>,
    file_tree_area: Option<Rect>,
) {
    if editor.has_codex_auth_dialog()
        || editor.ai_chat_has_pending_code_explanation()
        || editor.ai_shell_inspector_is_open()
    {
        return;
    }
    let layout = ctx.layout;
    let viewport_start = ctx.viewport_start;
    let cursor_pos = editor.buffer().cursor();
    let cursor_line = cursor_pos.line();
    let cursor_col = cursor_pos.col();

    if editor.mode() == crate::mode::Mode::FileTree {
        if let Some(area) = file_tree_area {
            use ovim_core::editor::FileTreeAction;
            use unicode_width::UnicodeWidthStr;

            let action = editor.file_tree().pending_action();
            let prompt = match action {
                FileTreeAction::Add { input } => Some(("new: ", input)),
                FileTreeAction::Rename { input, .. } => Some(("rename: ", input)),
                FileTreeAction::Filter { input } => Some(("filter: ", input)),
                FileTreeAction::None | FileTreeAction::DeleteConfirm { .. } => None,
            };
            if let Some((prefix, input)) = prompt {
                let input_width = UnicodeWidthStr::width(&input.text()[..input.cursor()]);
                let x = area
                    .x
                    .saturating_add((prefix.len() + input_width) as u16)
                    .min(area.right().saturating_sub(1));
                frame.set_cursor_position((x, area.bottom().saturating_sub(1)));
            } else {
                let tree = editor.file_tree();
                let row = tree.selected_index().saturating_sub(tree.scroll_offset());
                let footer = if tree.help_visible() { 6 } else { 1 };
                let last_tree_row = area.bottom().saturating_sub(footer).saturating_sub(1);
                let y = area
                    .y
                    .saturating_add(1)
                    .saturating_add(row as u16)
                    .min(last_tree_row);
                let x = tree
                    .selected_node()
                    .map(|node| area.x.saturating_add((node.depth() * 2) as u16))
                    .unwrap_or(area.x)
                    .min(area.right().saturating_sub(1));
                frame.set_cursor_position((x, y));
            }
        }
        return;
    }

    if editor.mode() == crate::mode::Mode::LspManager {
        if let Some(panel) = editor.lsp_manager_panel() {
            if panel.filter_focused {
                use unicode_width::UnicodeWidthStr;

                let mgr_area = super::lsp_manager::get_lsp_manager_area(frame.area());
                let inner_x = mgr_area.x + 1;
                let inner_y = mgr_area.y + 1;
                let cursor = panel.filter_cursor();
                let input_width = UnicodeWidthStr::width(&panel.filter_query()[..cursor]);
                let max_x = mgr_area.right().saturating_sub(2);
                let cursor_x = (inner_x + 2 + input_width as u16).min(max_x);
                frame.set_cursor_position((cursor_x, inner_y));
            }
        }
        return;
    }

    if editor.mode() == crate::mode::Mode::Picker {
        if let Some(picker) = editor.picker() {
            use unicode_width::UnicodeWidthStr;

            let picker_area = super::picker_widget::get_picker_area(frame.area());
            // Inner area is picker_area inset by 1 on each side (border)
            let inner_x = picker_area.x + 1;
            let inner_width = picker_area.width.saturating_sub(2) as usize;
            let cursor_y = picker_area.y + 1;

            let cursor_x = if picker.has_file_filter() {
                use crate::editor::PickerField;
                let search_width = (inner_width * 70 / 100).max(10);
                match picker.active_field() {
                    PickerField::Query => {
                        // icon(1) + space(1) + cursor_pos
                        let cursor = picker.query_cursor();
                        let width = UnicodeWidthStr::width(&picker.query()[..cursor]);
                        (inner_x + 2 + width as u16).min(inner_x + search_width as u16 - 1)
                    }
                    PickerField::FileFilter => {
                        // search_width + sep(1) + icon(1) + space(1) + cursor_pos
                        let cursor = picker.file_filter_cursor();
                        let width = UnicodeWidthStr::width(&picker.file_filter()[..cursor]);
                        let filter_start = inner_x + search_width as u16 + 1; // after separator
                        (filter_start + 2 + width as u16).min(inner_x + inner_width as u16 - 1)
                    }
                }
            } else {
                let cursor = picker.query_cursor();
                let width = UnicodeWidthStr::width(&picker.query()[..cursor]);
                (inner_x + 2 + width as u16).min(inner_x + inner_width as u16 - 1)
            };

            frame.set_cursor_position((cursor_x, cursor_y));
        }
    } else if editor.mode() == crate::mode::Mode::Command {
        use unicode_width::UnicodeWidthStr;

        let cursor = editor.command_cursor();
        let input_width = UnicodeWidthStr::width(&editor.command_line()[..cursor]);
        let cmd_cursor_x = (input_width + 1).min(command_chunk.width.saturating_sub(1) as usize);
        frame.set_cursor_position((command_chunk.x + cmd_cursor_x as u16, command_chunk.y));
    } else if editor.mode() == crate::mode::Mode::Search {
        use unicode_width::UnicodeWidthStr;

        let cursor = editor.search_cursor();
        let input_width = UnicodeWidthStr::width(&editor.search_buffer()[..cursor]);
        let search_cursor_x = (input_width + 1).min(command_chunk.width.saturating_sub(1) as usize);
        frame.set_cursor_position((command_chunk.x + search_cursor_x as u16, command_chunk.y));
    } else if editor.mode() == crate::mode::Mode::RenameInput {
        use unicode_width::UnicodeWidthStr;

        let cursor = editor.rename_cursor();
        let input_width = UnicodeWidthStr::width(&editor.rename_buffer()[..cursor]);
        let rename_cursor_x = (input_width + 8).min(command_chunk.width.saturating_sub(1) as usize);
        frame.set_cursor_position((command_chunk.x + rename_cursor_x as u16, command_chunk.y));
    } else if editor.mode() == crate::mode::Mode::AiChat && chat_area.is_some() {
        if let Some(position) = editor.render_cache.ai_chat_exa_input_cursor_pos {
            frame.set_cursor_position(position);
            return;
        }
        if let Some(chat_rect) = chat_area {
            if let Some((cx, cy)) = super::ai_chat::chat_cursor_info(editor, chat_rect) {
                frame.set_cursor_position((cx, cy));
            }
        }
    } else if editor.mode() == crate::mode::Mode::AiPrompt {
        if !editor.render_cache.ai_prompt_input_rows.is_empty() {
            let cursor_byte = editor
                .ai_prompt_cursor()
                .min(editor.ai_prompt_input().len());
            let mut row = *editor.render_cache.ai_prompt_input_rows.last().unwrap();
            for candidate in &editor.render_cache.ai_prompt_input_rows {
                if cursor_byte < candidate.2 {
                    row = *candidate;
                    break;
                }
            }
            let row_start = row.1.min(editor.ai_prompt_input().len());
            let row_end = row.2.min(editor.ai_prompt_input().len()).max(row_start);
            let row_cursor = cursor_byte.clamp(row_start, row_end);
            let cursor_display = editor.ai_prompt_input()[row_start..row_cursor]
                .chars()
                .map(crate::display::char_display_width)
                .sum::<usize>();
            let clamped_x = row
                .0
                .x
                .saturating_add(cursor_display.min(row.0.width.saturating_sub(1) as usize) as u16);
            frame.set_cursor_position((clamped_x, row.0.y));
        } else if let Some(input_area) = editor.render_cache.ai_prompt_input_area {
            frame.set_cursor_position((input_area.x, input_area.y));
        } else {
            frame.set_cursor_position((command_chunk.x, command_chunk.y));
        }
    } else {
        let rope = editor.buffer().rope();
        let line_text = ovim_core::display::line_content(rope, cursor_line);

        let tab_width = editor.options.tab_width;

        // Compute the cursor's flat display column: expand tabs, then add
        // inline decoration widths before the cursor's char position.
        let exp = super::helpers::expand_tabs_with_mapping(&line_text, tab_width);
        let char_col = ovim_core::unicode::grapheme_to_char_col(&line_text, cursor_col).0;
        let expanded_col = if char_col < exp.char_mapping.len() {
            exp.char_mapping[char_col]
        } else if !exp.char_mapping.is_empty() {
            *exp.char_mapping.last().unwrap()
        } else {
            char_col
        };
        let inline_offset = editor.decorations.inline_width_before_projected(
            cursor_line,
            char_col,
            editor.buffer().rope(),
            editor.buffer().edit_log(),
        );
        let display_col = expanded_col + inline_offset;

        let buffer_area = layout.buffer_area;
        let gutter_width = layout.gutter_width;
        let text_width = layout.text_width;

        let (cursor_y, cursor_x) = if editor.options.wrap && text_width > 0 {
            // Use WrapMap's cursor_to_visual_with_decorations which correctly
            // handles wide chars pushed to next row, variable-width tabs at
            // row boundaries, and decorations spanning multiple visual rows.
            let rope = editor.buffer().rope();
            let inline_widths = editor.decorations.inline_decorations_for_line_projected(
                cursor_line,
                rope,
                editor.buffer().edit_log(),
            );

            let (abs_visual_row, visual_col) = if let Some(wrap_map) = editor.wrap_map() {
                wrap_map.cursor_to_visual_with_decorations(
                    cursor_line,
                    display_col,
                    &line_text,
                    &inline_widths,
                )
            } else {
                // Fallback: simple division when no wrap map
                let sub_row = display_col / text_width;
                let col = display_col % text_width;
                (cursor_line + sub_row, col)
            };

            let viewport_visual_row = if let Some(wrap_map) = editor.wrap_map() {
                wrap_map.viewport_top_visual_row(viewport_start, editor.scroll_subrow())
            } else {
                viewport_start
            };
            let screen_row = abs_visual_row.saturating_sub(viewport_visual_row);
            (
                screen_row.min(buffer_area.height.saturating_sub(1) as usize),
                visual_col.min(text_width.saturating_sub(1)),
            )
        } else {
            let screen_line = cursor_line.saturating_sub(viewport_start);
            let h_offset = editor.horizontal_offset();
            let adjusted_col = display_col.saturating_sub(h_offset);
            (
                screen_line.min(buffer_area.height.saturating_sub(1) as usize),
                adjusted_col.min(text_width.saturating_sub(1)),
            )
        };

        frame.set_cursor_position((
            buffer_area.x + gutter_width as u16 + cursor_x as u16,
            buffer_area.y + cursor_y as u16,
        ));
    }
}

// ---------------------------------------------------------------------------
// Split window rendering (unchanged)
// ---------------------------------------------------------------------------

/// Invariant context for recursive window tree rendering.
struct RenderTreeContext<'a, 'b> {
    frame: &'a mut Frame<'b>,
    /// Mutable so each leaf can `ensure_wrap_map_for_window` before it renders
    /// (roadmap 19); `render_buffer` reborrows it shared.
    editor: &'a mut Editor,
    theme: &'a Theme,
    focused_index: usize,
    current_index: &'a mut usize,
    line_cache: &'a mut LineRenderCache,
}

/// Recursively renders windows in a split layout
/// Returns (viewport_start, layout) for the focused window (for cursor positioning)
fn render_window_tree(
    ctx: &mut RenderTreeContext,
    node: &WindowViewNode,
    area: Rect,
) -> Option<(usize, BufferLayout)> {
    match node {
        WindowViewNode::Leaf(view) => {
            let window_idx = *ctx.current_index;
            let is_focused = window_idx == ctx.focused_index;
            *ctx.current_index += 1;

            let layout = BufferLayout::compute(&*ctx.editor, area);

            // Build this pane's wrap map at *its own* content width.
            // `editor.wrap_map()` resolves to the focused window's map, so the
            // cursor overlay agrees with the focused pane's content. (roadmap 19
            // / OV-00209)
            if ctx.editor.options.wrap {
                ctx.editor
                    .ensure_wrap_map_for_window(window_idx, layout.text_width);
            }

            // For non-focused windows, override cursor / scroll / wrap-map with
            // the window's own state; the focused window *is* the editor.
            let window_context = if !is_focused {
                Some(WindowRenderContext {
                    cursor: Some(view.cursor),
                    scroll_offset: Some(view.scroll_offset),
                    scroll_subrow: Some(view.scroll_subrow),
                    horizontal_offset: Some(view.horizontal_offset),
                    wrap_map_window_index: Some(window_idx),
                })
            } else {
                None
            };

            let viewport_start = render_buffer(
                ctx.frame,
                &*ctx.editor,
                ctx.theme,
                &layout,
                ctx.line_cache,
                window_context.as_ref(),
            );

            if is_focused {
                Some((viewport_start, layout))
            } else {
                None
            }
        }
        WindowViewNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (first_area, sep_area, second_area) = match direction {
                SplitDirection::Horizontal => {
                    let first_height = (area.height as f32 * *ratio) as u16;
                    let sep_height = 1u16;
                    let second_height = area.height.saturating_sub(first_height + sep_height);

                    let first_rect = Rect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: first_height,
                    };
                    let sep_rect = Rect {
                        x: area.x,
                        y: area.y + first_height,
                        width: area.width,
                        height: sep_height,
                    };
                    let second_rect = Rect {
                        x: area.x,
                        y: area.y + first_height + sep_height,
                        width: area.width,
                        height: second_height,
                    };
                    (first_rect, sep_rect, second_rect)
                }
                SplitDirection::Vertical => {
                    let first_width = (area.width as f32 * *ratio) as u16;
                    let sep_width = 1u16;
                    let second_width = area.width.saturating_sub(first_width + sep_width);

                    let first_rect = Rect {
                        x: area.x,
                        y: area.y,
                        width: first_width,
                        height: area.height,
                    };
                    let sep_rect = Rect {
                        x: area.x + first_width,
                        y: area.y,
                        width: sep_width,
                        height: area.height,
                    };
                    let second_rect = Rect {
                        x: area.x + first_width + sep_width,
                        y: area.y,
                        width: second_width,
                        height: area.height,
                    };
                    (first_rect, sep_rect, second_rect)
                }
            };

            render_separator(ctx.frame, sep_area, *direction);

            let first_result = render_window_tree(ctx, first, first_area);
            let second_result = render_window_tree(ctx, second, second_area);

            first_result.or(second_result)
        }
    }
}

/// Renders a separator line between split windows
fn render_separator(frame: &mut Frame, area: Rect, direction: SplitDirection) {
    let sep_char = match direction {
        SplitDirection::Horizontal => '─',
        SplitDirection::Vertical => '│',
    };

    let sep_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    match direction {
        SplitDirection::Horizontal => {
            let line_text = sep_char.to_string().repeat(area.width as usize);
            let line = Line::from(Span::styled(line_text, sep_style));
            let paragraph = Paragraph::new(vec![line]);
            frame.render_widget(paragraph, area);
        }
        SplitDirection::Vertical => {
            let lines: Vec<Line> = (0..area.height)
                .map(|_| Line::from(Span::styled(sep_char.to_string(), sep_style)))
                .collect();
            let paragraph = Paragraph::new(lines);
            frame.render_widget(paragraph, area);
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer struct
// ---------------------------------------------------------------------------

/// Handles rendering the editor state to the terminal
pub struct Renderer {
    terminal: RatatuiTerminal<CrosstermBackend<io::Stdout>>,
    /// Per-line render cache to avoid recomputing unchanged lines
    line_cache: LineRenderCache,
    /// Discriminant of the last emitted cursor style (0=block, 1=bar) to
    /// avoid redundant crossterm writes every frame.
    last_cursor_style: Option<u8>,
    /// Cached terminal title to avoid redundant crossterm writes every frame
    last_title: String,
    image_renderer: super::terminal_images::TerminalImageRenderer,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    /// Creates a new renderer
    pub fn new() -> Self {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = RatatuiTerminal::new(backend).expect("Failed to create terminal");
        Self {
            terminal,
            line_cache: LineRenderCache::new(),
            last_cursor_style: None,
            last_title: String::new(),
            image_renderer: super::terminal_images::TerminalImageRenderer::detect(),
        }
    }

    /// Renders editor to a frame (used by both TUI and headless rendering)
    pub fn render_to_frame(
        frame: &mut Frame,
        editor: &mut Editor,
        line_cache: &mut LineRenderCache,
    ) {
        // Terminal graphics can outlive ordinary cell contents. Rebuild the
        // visible thumbnail placements on every frame so hiding the chat or
        // scrolling an image message away cannot replay stale image commands.
        editor.render_cache.ai_chat_image_thumbnails.clear();
        editor.render_cache.ai_chat_interactions.yolo_toggle = None;
        editor
            .render_cache
            .ai_chat_interactions
            .comprehension_toggle = None;
        init_frame(frame, editor);

        let areas = match compute_frame_layout(frame, editor) {
            Some(areas) => areas,
            None => {
                let area = frame.area();
                render_dashboard(frame, editor, area);
                return;
            }
        };

        let scheme = editor
            .get_color_scheme()
            .cloned()
            .unwrap_or_else(crate::syntax::ColorScheme::tokyonight);
        let theme = Theme::from_scheme(scheme);

        // Render chrome
        if let Some(tab_area) = areas.tab_area {
            render_tab_bar(frame, editor, &theme, tab_area);
        }
        if let Some(tree_area) = areas.file_tree_area {
            render_file_tree(frame, editor, tree_area);
        }

        // Render buffer content
        let (viewport_start, layout) =
            render_buffer_area(frame, editor, &theme, &areas, line_cache);

        // Update viewport dimensions and cache layout for mouse coordinate conversion
        editor.set_viewport_height(layout.buffer_area.height as usize);
        editor.set_last_layout(
            crate::key_convert::convert_ratatui_rect(layout.buffer_area),
            layout.gutter_width,
            layout.text_width,
            layout.blame_width,
        );
        if let Some(wm) = editor.window_manager_mut() {
            wm.update_dimensions(layout.buffer_area.width, layout.buffer_area.height);
        }

        // Render chat panel (if in AiChat mode)
        if let Some(chat_area) = areas.chat_area {
            super::ai_chat::render_chat_panel_cached(frame, editor, chat_area, &theme, line_cache);
            let chat_area = crate::key_convert::convert_ratatui_rect(chat_area);
            editor.render_cache.last_chat_area = Some(chat_area);
            editor.render_cache.ai_chat_separator_area = Some(ovim_core::Rect {
                x: chat_area.x,
                y: chat_area.y,
                width: 1,
                height: chat_area.height,
            });
            editor.render_cache.ai_chat_split_area = Some(ovim_core::Rect {
                x: areas.buffer_chunk.x,
                y: chat_area.y,
                width: areas.buffer_chunk.width.saturating_add(chat_area.width),
                height: chat_area.height,
            });
        } else {
            editor.render_cache.last_chat_area = None;
            editor.render_cache.ai_chat_separator_area = None;
            editor.render_cache.ai_chat_split_area = None;
            editor.render_cache.ai_chat_separator_dragging = false;
        }

        // Render debug panels (if visible)
        if let Some(debug_side) = areas.debug_side_area {
            super::debug_panels::render_debug_side_panel(frame, editor, debug_side);
        }
        if let Some(debug_output) = areas.debug_output_area {
            super::debug_panels::render_debug_output(frame, editor, debug_output);
        }

        // Render status + overlays + cursor
        render_status_area(frame, editor, &theme, &areas);
        let ctx = OverlayContext {
            layout: &layout,
            viewport_start,
        };
        render_overlays(frame, editor, &theme, &ctx, areas.command_chunk);
        super::overlays::render_ai_code_explanation(frame, editor);
        render_blocking_modals(frame, editor, &theme);
        set_cursor_position(
            frame,
            editor,
            &ctx,
            areas.command_chunk,
            areas.chat_area,
            areas.file_tree_area,
        );
    }

    /// Renders the editor state to the terminal
    pub fn render(&mut self, editor: &mut Editor) -> Result<()> {
        let cursor_style = match editor.mode() {
            crate::mode::Mode::Insert
            | crate::mode::Mode::Picker
            | crate::mode::Mode::Command
            | crate::mode::Mode::Search
            | crate::mode::Mode::RenameInput
            | crate::mode::Mode::FileTree
            | crate::mode::Mode::AiPrompt
            | crate::mode::Mode::AiChat => SetCursorStyle::BlinkingBar,
            _ => SetCursorStyle::SteadyBlock,
        };
        let title = editor
            .buffer()
            .file_path()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p)
            })
            .unwrap_or("ovim");

        // Only emit crossterm commands when the values actually change.
        // These run on every frame otherwise — unnecessary terminal I/O.
        let style_key = match cursor_style {
            SetCursorStyle::BlinkingBar => 1u8,
            _ => 0u8,
        };
        let style_changed = self.last_cursor_style != Some(style_key);
        let title_changed = self.last_title != title;
        if style_changed && title_changed {
            crossterm::execute!(io::stdout(), cursor_style, SetTitle(title))?;
        } else if style_changed {
            crossterm::execute!(io::stdout(), cursor_style)?;
        } else if title_changed {
            crossterm::execute!(io::stdout(), SetTitle(title))?;
        }
        if style_changed {
            self.last_cursor_style = Some(style_key);
        }
        if title_changed {
            self.last_title = title.to_string();
        }

        self.terminal.autoresize()?;
        if take_terminal_image_refresh(
            &mut editor.render_cache,
            self.image_renderer.is_enabled(),
            self.image_renderer.rendered_last_frame(),
        ) {
            // Inline terminal images are owned by the terminal rather than by
            // Ratatui's cell buffer. A tab/focus transition can invalidate or
            // restore them independently, so reset both the physical surface
            // and Ratatui's back buffer before re-emitting visible images.
            self.terminal.clear()?;
        }
        editor.render_cache.terminal_image_support = self.image_renderer.is_enabled();

        // Take the line cache out to avoid borrow conflict with terminal.draw()
        let mut line_cache = std::mem::take(&mut self.line_cache);
        let image_renderer = &mut self.image_renderer;
        self.terminal.draw(|frame| {
            Self::render_to_frame(frame, editor, &mut line_cache);
            image_renderer.render(frame, editor);
        })?;
        self.line_cache = line_cache;

        use std::io::Write;
        io::stdout().flush()?;

        Ok(())
    }

    /// Clears the terminal
    pub fn clear(&mut self) -> Result<()> {
        self.terminal.clear()?;
        Ok(())
    }
}

fn take_terminal_image_refresh(
    cache: &mut ovim_core::editor::RenderCache,
    protocol_enabled: bool,
    image_was_visible: bool,
) -> bool {
    std::mem::take(&mut cache.terminal_image_refresh_requested)
        && protocol_enabled
        && image_was_visible
}

#[cfg(test)]
mod cursor_screen_position_tests {
    //! Regression coverage for the hardware-cursor screen row under soft wrap.
    //!
    //! The viewport's visual-row origin is `logical_to_visual(scroll_offset) +
    //! scroll_subrow`. When the top logical line is wrapped and scrolled into
    //! (`scroll_subrow > 0`), the buffer renderer skips those sub-rows — so the
    //! cursor must subtract them too. Omitting the sub-row term drew the cursor
    //! `scroll_subrow` rows below its real input point (regression of OV-00019,
    //! visible when many wrapped rows precede the cursor).

    use super::{should_dock_ai_chat, take_terminal_image_refresh, Renderer};
    use crate::editor::{Editor, Picker};
    use crate::ui::renderer::line_cache::LineRenderCache;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn code_walkthrough_temporarily_reclaims_the_docked_chat_width() {
        assert!(should_dock_ai_chat(true, false, false));
        assert!(!should_dock_ai_chat(true, false, true));
        assert!(!should_dock_ai_chat(true, true, false));
    }

    #[test]
    fn focus_refresh_only_clears_when_terminal_image_was_visible() {
        let mut cache = ovim_core::editor::RenderCache {
            terminal_image_refresh_requested: true,
            ..Default::default()
        };
        assert!(!take_terminal_image_refresh(&mut cache, true, false));
        assert!(!cache.terminal_image_refresh_requested);

        cache.terminal_image_refresh_requested = true;
        assert!(!take_terminal_image_refresh(&mut cache, false, true));
        assert!(!cache.terminal_image_refresh_requested);

        cache.terminal_image_refresh_requested = true;
        assert!(take_terminal_image_refresh(&mut cache, true, true));
        assert!(!cache.terminal_image_refresh_requested);
    }

    #[test]
    fn frame_without_chat_clears_stale_terminal_image_placements() {
        let mut editor = Editor::default();
        editor.render_cache.ai_chat_image_thumbnails.push((
            ovim_core::Rect {
                x: 1,
                y: 1,
                width: 8,
                height: 4,
            },
            std::path::PathBuf::from("/tmp/stale.png"),
        ));
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut line_cache = LineRenderCache::new();

        terminal
            .draw(|frame| Renderer::render_to_frame(frame, &mut editor, &mut line_cache))
            .unwrap();

        assert!(editor.render_cache.ai_chat_image_thumbnails.is_empty());
    }

    /// Renders `editor` to a test terminal and returns the hardware cursor.
    fn render_and_cursor_position(editor: &mut Editor, width: u16, height: u16) -> (u16, u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut line_cache = LineRenderCache::new();
        terminal
            .draw(|f| Renderer::render_to_frame(f, editor, &mut line_cache))
            .unwrap();
        let position = terminal.get_cursor_position().unwrap();
        (position.x, position.y)
    }

    fn render_and_cursor_y(editor: &mut Editor, width: u16, height: u16) -> u16 {
        render_and_cursor_position(editor, width, height).1
    }

    #[test]
    fn rename_cursor_uses_display_width_for_unicode_input() {
        let mut editor = Editor::default();
        editor.set_rename_buffer("éx".to_owned());
        editor.set_mode(crate::mode::Mode::RenameInput);

        let (cursor_x, _) = render_and_cursor_position(&mut editor, 40, 12);

        assert_eq!(cursor_x, 10);
    }

    #[test]
    fn command_cursor_uses_display_width_for_unicode_input() {
        let mut editor = Editor::default();
        editor.set_command_line("éx");
        editor.set_mode(crate::mode::Mode::Command);

        let (cursor_x, _) = render_and_cursor_position(&mut editor, 40, 12);

        assert_eq!(cursor_x, 3);
    }

    #[test]
    fn picker_cursor_uses_display_width_for_wide_unicode_input() {
        let mut editor = Editor::default();
        let mut picker = Picker::new_file_finder(".".into(), ".".into());
        picker.insert_text("界x");
        editor.set_picker(picker);
        editor.set_mode(crate::mode::Mode::Picker);

        let (cursor_x, _) = render_and_cursor_position(&mut editor, 40, 12);
        let picker_area =
            super::super::picker_widget::get_picker_area(ratatui::layout::Rect::new(0, 0, 40, 12));

        assert_eq!(cursor_x, picker_area.x + 6);
    }

    #[test]
    fn cursor_row_accounts_for_scroll_subrow_in_wrapped_line() {
        const WIDTH: u16 = 24;
        const HEIGHT: u16 = 12;

        // One very long logical line so it wraps into many visual rows
        // regardless of the exact gutter width the layout chooses.
        let content = "a".repeat(400);
        let mut editor = Editor::with_content(&content);
        editor.init_window_manager(WIDTH, HEIGHT);
        editor.set_viewport_height(HEIGHT as usize);
        editor.options.wrap = true;
        editor.options.scrolloff = 0;

        // First render establishes the real layout (text_width after gutter)
        // and builds the wrap map at that width.
        render_and_cursor_y(&mut editor, WIDTH, HEIGHT);
        let text_width = editor.render_cache.last_text_width;
        let buffer_top = editor.render_cache.last_buffer_area.unwrap().y;
        assert!(
            text_width > 0,
            "wrap mode must produce a positive text width"
        );

        // Park the cursor on the 6th visual row of line 0, then scroll 3 wrapped
        // rows into the line. The cursor then sits at screen row 6 - 3 = 3:
        // comfortably mid-viewport, so the `min(height - 1)` clamp can't mask a
        // wrong (too-low) value.
        const CURSOR_VISUAL_ROW: usize = 6;
        const SUBROW: usize = 3;
        let cursor_col = CURSOR_VISUAL_ROW * text_width + 2;
        editor
            .buffer_mut()
            .set_cursor_char_col(0, ovim_core::unicode::CharCol(cursor_col));
        if let Some(wm) = editor.window_manager_mut() {
            if let Some(window) = wm.focused_window_mut() {
                window.set_scroll_position(0, SUBROW);
            }
        }

        let cursor_y = render_and_cursor_y(&mut editor, WIDTH, HEIGHT);

        // Ground truth from the wrap map: absolute visual row of the cursor
        // minus the viewport's visual-row origin, offset by the buffer's top.
        let map = editor
            .window_manager()
            .unwrap()
            .focused_window()
            .unwrap()
            .wrap_map()
            .unwrap();
        let line_text = editor.buffer().line_text(0).unwrap_or_default();
        let (abs_row, _) = map.cursor_to_visual(0, cursor_col, &line_text);
        let top = map.viewport_top_visual_row(0, SUBROW);
        let expected_y = buffer_top + (abs_row - top) as u16;

        assert_eq!(
            cursor_y, expected_y,
            "cursor drawn at terminal row {cursor_y}, expected {expected_y} \
             (abs visual row {abs_row}, viewport top visual row {top}); a value \
             {SUBROW} rows too low means scroll_subrow was ignored"
        );
    }
}
