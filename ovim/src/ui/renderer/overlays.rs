use crate::editor::Editor;
use crate::syntax::{Theme, UiGroup};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::ai_chat::TEXT_DIM;
use super::helpers::{grapheme_col_to_display_col, truncate_to_width};
use super::layout::OverlayContext;

fn hover_content_width(rendered_lines: &[Line<'_>], hover_text: &str, is_preview: bool) -> usize {
    if is_preview {
        rendered_lines.iter().map(Line::width).max().unwrap_or(30)
    } else {
        hover_text
            .lines()
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(30)
    }
}

fn render_hover_preview_lines(
    hover_text: &str,
    max_window_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use super::markdown::{parse_markdown, render_markdown};

    let content_width = max_window_width.saturating_sub(4).max(1);
    render_markdown(&parse_markdown(hover_text), content_width, Some(theme))
        .iter()
        .flat_map(|line| super::ai_chat::styled_word_wrap_line(line, content_width))
        .map(Line::from)
        .collect()
}

fn hover_raw_lines(hover_text: &str) -> Vec<&str> {
    hover_text.split('\n').collect()
}

/// Renders hover information as a floating window positioned near the cursor
///
/// Both modes are scrollable (j/k vertical, h/l horizontal):
/// - Preview mode: styled markdown rendering
/// - Navigate mode: raw text view (K from preview to switch)
#[allow(clippy::too_many_arguments)]
pub fn render_hover_window(
    frame: &mut Frame,
    editor: &Editor,
    hover_text: &str,
    scroll_offset: usize,
    ctx: &OverlayContext,
    hover_position: Option<(usize, usize)>,
    is_preview: bool,
    theme: &Theme,
    content_type: crate::editor::HoverContentType,
) {
    let layout = ctx.layout;
    let viewport_start = ctx.viewport_start;
    let buffer_area = layout.buffer_area;
    use super::markdown::colors;

    let h_scroll = editor.hover_h_scroll();

    const MIN_WIDTH: u16 = 30;
    const MIN_HEIGHT: u16 = 3;

    // Adaptive max dimensions: use up to 80% of available space, but cap at sane limits.
    let max_width = (buffer_area.width * 4 / 5).clamp(MIN_WIDTH, 120);
    let max_height = (buffer_area.height * 4 / 5).clamp(MIN_HEIGHT, 40);

    let rendered_lines = render_hover_preview_lines(hover_text, max_width as usize, theme);
    let total_lines = if is_preview {
        rendered_lines.len()
    } else {
        hover_raw_lines(hover_text).len()
    };

    // Calculate content dimensions
    let content_width = hover_content_width(&rendered_lines, hover_text, is_preview);

    let window_width = (content_width as u16 + 4)
        .clamp(MIN_WIDTH, max_width)
        .min(buffer_area.width.saturating_sub(4));

    let window_height = (total_lines as u16 + 2)
        .clamp(MIN_HEIGHT, max_height)
        .min(buffer_area.height.saturating_sub(2));

    // Calculate cursor screen position
    let (cursor_line, cursor_col) = hover_position.unwrap_or_else(|| {
        let cursor = editor.buffer().cursor();
        (cursor.line(), cursor.col().0)
    });

    let gutter_width = layout.gutter_width;

    // Convert cursor to screen coordinates
    let rope = editor.buffer().rope();
    let line_text = ovim_core::display::line_content(rope, cursor_line);
    let tab_width = editor.indent_options().tab_width;
    let char_col = ovim_core::unicode::grapheme_to_char_col(
        &line_text,
        ovim_core::unicode::GraphemeCol(cursor_col),
    )
    .0;
    let base_display_col = grapheme_col_to_display_col(&line_text, cursor_col, tab_width);
    let edit_log = editor.buffer().edit_log();
    let inline_offset =
        editor
            .decorations
            .inline_width_before_projected(cursor_line, char_col, rope, edit_log);
    let display_col = base_display_col + inline_offset;
    let text_width = layout.text_width;
    let inline_widths =
        editor
            .decorations
            .inline_decorations_for_line_projected(cursor_line, rope, edit_log);

    let (screen_line, visual_col) = if editor.options.wrap && text_width > 0 {
        if let Some(wrap_map) = editor.wrap_map() {
            let (abs_row, vcol) = wrap_map.cursor_to_visual_with_decorations(
                cursor_line,
                display_col,
                &line_text,
                &inline_widths,
            );
            let viewport_row =
                wrap_map.viewport_top_visual_row(viewport_start, editor.scroll_subrow());
            (abs_row.saturating_sub(viewport_row), vcol)
        } else {
            (cursor_line.saturating_sub(viewport_start), display_col)
        }
    } else {
        let h_offset = editor.horizontal_offset();
        (
            cursor_line.saturating_sub(viewport_start),
            display_col.saturating_sub(h_offset),
        )
    };

    let cursor_screen_x = buffer_area.x + gutter_width as u16 + visual_col as u16;
    let cursor_screen_y = buffer_area.y + screen_line as u16;

    // Determine vertical position (prefer below, fallback to above)
    let space_below = buffer_area.bottom().saturating_sub(cursor_screen_y + 1);
    let space_above = cursor_screen_y.saturating_sub(buffer_area.y);

    let window_y = if space_below >= window_height || space_below >= space_above {
        // Position below cursor
        (cursor_screen_y + 1).min(buffer_area.bottom().saturating_sub(window_height))
    } else {
        // Position above cursor
        cursor_screen_y.saturating_sub(window_height)
    };

    // Determine horizontal position (start at cursor, shift left if needed)
    let window_x = cursor_screen_x
        .min(buffer_area.right().saturating_sub(window_width))
        .max(buffer_area.x);

    let window_area = Rect {
        x: window_x,
        y: window_y,
        width: window_width,
        height: window_height,
    };

    // Calculate visible content height
    let content_height = window_height.saturating_sub(2) as usize;

    // Clamp scroll offset
    let max_scroll = total_lines.saturating_sub(content_height);
    let clamped_scroll = scroll_offset.min(max_scroll);

    let scrollable = total_lines > content_height;

    // Create title based on content type and scrollability
    let title = match (is_preview, content_type) {
        (true, crate::editor::HoverContentType::Diagnostic) if scrollable => {
            format!(" Diagnostic {}/{} ", clamped_scroll + 1, total_lines)
        }
        (true, crate::editor::HoverContentType::Diagnostic) => " Diagnostic ".to_string(),
        (true, crate::editor::HoverContentType::BlameInfo) => " Blame ".to_string(),
        (true, crate::editor::HoverContentType::AiReasoning) if scrollable => {
            format!(" AI reasoning {}/{} ", clamped_scroll + 1, total_lines)
        }
        (true, crate::editor::HoverContentType::AiReasoning) => " AI reasoning ".to_string(),
        (true, _) if scrollable => {
            format!(
                " {}/{} j/k:scroll K:raw q:close ",
                clamped_scroll + 1,
                total_lines
            )
        }
        (true, _) => " q:close K:raw ".to_string(),
        (false, _) if scrollable => {
            format!(
                " {}/{} j/k:scroll q:close ",
                clamped_scroll + 1,
                total_lines
            )
        }
        _ => " q to close ".to_string(),
    };

    // Render content based on mode
    if is_preview {
        // Render styled markdown with scroll support
        let visible_lines: Vec<ratatui::text::Line> = rendered_lines
            .into_iter()
            .skip(clamped_scroll)
            .take(content_height)
            .collect();

        let paragraph = Paragraph::new(visible_lines)
            .style(Style::default().bg(colors::BG))
            .scroll((0, h_scroll as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(colors::BORDER))
                    .title(title)
                    .title_style(
                        Style::default()
                            .fg(colors::BORDER)
                            .add_modifier(Modifier::BOLD),
                    ),
            );

        frame.render_widget(ratatui::widgets::Clear, window_area);
        frame.render_widget(paragraph, window_area);
    } else {
        // Render raw text (navigate mode) — no wrapping, uses h/l for horizontal scroll
        let all_lines = hover_raw_lines(hover_text);
        let visible_lines: Vec<String> = all_lines
            .iter()
            .skip(clamped_scroll)
            .take(content_height)
            .map(|line| format!(" {} ", line))
            .collect();

        let text = visible_lines.join("\n");

        let paragraph = Paragraph::new(text)
            .style(
                Style::default()
                    .bg(Color::Rgb(30, 30, 40))
                    .fg(Color::Rgb(230, 230, 230)),
            )
            .scroll((0, h_scroll as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(137, 180, 250)))
                    .title(title)
                    .title_style(
                        Style::default()
                            .fg(Color::Rgb(137, 180, 250))
                            .add_modifier(Modifier::BOLD),
                    ),
            );

        frame.render_widget(ratatui::widgets::Clear, window_area);
        frame.render_widget(paragraph, window_area);
    }
}

/// Renders the completion menu popup
pub fn render_completion_menu(frame: &mut Frame, editor: &Editor, ctx: &OverlayContext) {
    let layout = ctx.layout;
    let viewport_start = ctx.viewport_start;
    let buffer_area = layout.buffer_area;
    let completion_menu = editor.completion_menu();
    if !completion_menu.is_visible() {
        return;
    }

    let items = completion_menu.items();
    if items.is_empty() {
        return;
    }

    // Get cursor position on screen
    let cursor = editor.buffer().cursor();
    let cursor_line = cursor.line();
    let cursor_col = cursor.col().0;

    // Get the line text and convert character column to display column
    let rope = editor.buffer().rope();
    let line_text = ovim_core::display::line_content(rope, cursor_line);

    // Convert character column to display column (accounting for tabs, emojis, and inline decorations)
    let tab_width = editor.indent_options().tab_width;
    let char_col = ovim_core::unicode::grapheme_to_char_col(
        &line_text,
        ovim_core::unicode::GraphemeCol(cursor_col),
    )
    .0;
    let base_display_col = grapheme_col_to_display_col(&line_text, cursor_col, tab_width);
    let edit_log = editor.buffer().edit_log();
    let inline_offset =
        editor
            .decorations
            .inline_width_before_projected(cursor_line, char_col, rope, edit_log);
    let display_col = base_display_col + inline_offset;
    let text_width = layout.text_width;
    let inline_widths =
        editor
            .decorations
            .inline_decorations_for_line_projected(cursor_line, rope, edit_log);

    let (screen_line, visual_col) = if editor.options.wrap && text_width > 0 {
        if let Some(wrap_map) = editor.wrap_map() {
            let (abs_row, vcol) = wrap_map.cursor_to_visual_with_decorations(
                cursor_line,
                display_col,
                &line_text,
                &inline_widths,
            );
            let viewport_row =
                wrap_map.viewport_top_visual_row(viewport_start, editor.scroll_subrow());
            (abs_row.saturating_sub(viewport_row), vcol)
        } else {
            (cursor_line.saturating_sub(viewport_start), display_col)
        }
    } else {
        let h_offset = editor.horizontal_offset();
        (
            cursor_line.saturating_sub(viewport_start),
            display_col.saturating_sub(h_offset),
        )
    };

    let gutter_width = layout.gutter_width;

    // Position menu below cursor
    let menu_x = buffer_area.x + gutter_width as u16 + visual_col as u16;
    let menu_y = buffer_area.y + screen_line as u16 + 1; // Below current line

    // Determine menu dimensions
    let max_items_to_show = 10;
    let num_items = items.len().min(max_items_to_show);
    let menu_height = num_items as u16 + 2; // +2 for borders

    // Calculate width based on longest label
    // Use UnicodeWidthStr::width() instead of len() because CJK characters
    // are 2 columns wide while ASCII characters are 1 column wide
    let max_label_len = items
        .iter()
        .take(max_items_to_show)
        .map(|item| item.label.width())
        .max()
        .unwrap_or(20);
    let menu_width = (max_label_len + 4).min(60) as u16; // +4 for padding and borders

    // Adjust position if menu would go off screen
    let menu_x = menu_x.min(buffer_area.width.saturating_sub(menu_width));
    let menu_y = if menu_y + menu_height > buffer_area.y + buffer_area.height {
        // Show above cursor if not enough space below
        (buffer_area.y + screen_line as u16)
            .saturating_sub(menu_height)
            .max(buffer_area.y)
    } else {
        menu_y
    };

    let menu_area = Rect::new(
        menu_x,
        menu_y,
        menu_width,
        menu_height.min(buffer_area.height),
    );

    // Build menu lines
    let selected_index = completion_menu.selected_index();
    let mut lines = Vec::new();

    for (idx, item) in items.iter().take(max_items_to_show).enumerate() {
        let is_selected = idx == selected_index;
        let style = if is_selected {
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Rgb(40, 44, 52)).fg(Color::White)
        };

        // Format: "label"  or "  label" with selection indicator
        let prefix = if is_selected { "> " } else { "  " };
        let text = format!("{}{}", prefix, item.label);

        lines.push(Line::from(Span::styled(text, style)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Rgb(40, 44, 52)));

    let paragraph = Paragraph::new(lines).block(block);

    // Clear background and render menu
    frame.render_widget(ratatui::widgets::Clear, menu_area);
    frame.render_widget(paragraph, menu_area);
}

/// Compact floating help card for AI chat review mode.
///
/// Uses a rounded border and transparent panel background while clearing
/// underlying content for readability.
pub fn render_ai_review_shortcuts(frame: &mut Frame, theme: &Theme, buffer_area: Rect) {
    if buffer_area.width < 34 || buffer_area.height < 8 {
        return;
    }

    let shortcuts = vec![
        ("\u{2190}/\u{2192}", "navigate edits"),
        ("Enter", "accept"),
        ("Ctrl-r", "back to chat"),
        ("Esc", "close"),
    ];

    let title = " Review Keys ";
    let content_width = shortcuts
        .iter()
        .map(|(k, v)| 1 + k.width() + 3 + v.width())
        .max()
        .unwrap_or(20)
        .max(title.width())
        .max(20);

    let max_panel_width = buffer_area.width.saturating_sub(2) as usize;
    if max_panel_width < 12 {
        return;
    }
    let panel_width = (content_width + 2).min(max_panel_width).max(24) as u16;
    let max_panel_height = buffer_area.height.saturating_sub(2);
    if max_panel_height < 4 {
        return;
    }
    let visible_rows = shortcuts
        .len()
        .min(max_panel_height.saturating_sub(2) as usize);
    let panel_height = (visible_rows + 2) as u16;

    let x = buffer_area
        .x
        .saturating_add(buffer_area.width.saturating_sub(panel_width + 1));
    let y = buffer_area.y.saturating_add(1);
    let panel_area = Rect {
        x,
        y,
        width: panel_width,
        height: panel_height,
    };

    let border_color = crate::key_convert::convert_core_color(theme.get_ui_color(UiGroup::Info));
    let key_color =
        crate::key_convert::convert_core_color(theme.get_ui_color(UiGroup::TabActiveFg));
    let text_color =
        crate::key_convert::convert_core_color(theme.get_ui_color(UiGroup::StatusLineForeground));
    let dash_color = crate::key_convert::convert_core_color(theme.get_ui_color(UiGroup::Border));

    let mut lines = Vec::with_capacity(visible_rows);
    for (key, desc) in shortcuts.into_iter().take(visible_rows) {
        let key_text = format!("{key:<7}");
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(Color::Reset)),
            Span::styled(
                key_text,
                Style::default()
                    .fg(key_color)
                    .bg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " \u{2022} ",
                Style::default().fg(dash_color).bg(Color::Reset),
            ),
            Span::styled(
                desc.to_string(),
                Style::default().fg(text_color).bg(Color::Reset),
            ),
        ]));
    }

    let card = Paragraph::new(lines)
        .style(Style::default().bg(Color::Reset))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(border_color).bg(Color::Reset))
                .title(title)
                .title_style(
                    Style::default()
                        .fg(border_color)
                        .bg(Color::Reset)
                        .add_modifier(Modifier::BOLD),
                ),
        );

    frame.render_widget(ratatui::widgets::Clear, panel_area);
    frame.render_widget(card, panel_area);
}

fn centered_area(full: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(full.width).max(1);
    let height = height.min(full.height).max(1);
    let x = full.x + full.width.saturating_sub(width) / 2;
    let y = full.y + full.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// Colors for modal dialogs. Explicit RGB values so the dialog is always
/// legible regardless of terminal background or active theme.
struct ModalColors {
    bg: Color,
    border: Color,
    title: Color,
    text: Color,
    secondary: Color,
    action: Color,
}

const MODAL_COLORS: ModalColors = ModalColors {
    bg: Color::Rgb(30, 34, 42),
    border: Color::Rgb(240, 180, 50),
    title: Color::Rgb(240, 180, 50),
    text: Color::Rgb(220, 225, 235),
    secondary: Color::Rgb(148, 158, 175),
    action: Color::Rgb(130, 210, 150),
};

/// Renders a centered modal dialog with the given title and content lines.
///
/// Each line is a `(text, role)` pair where role selects the color:
/// - `'t'` = primary text, `'s'` = secondary/hint, `'a'` = action/keybindings
fn render_modal_dialog(frame: &mut Frame, title: &str, lines: &[(&str, char)]) {
    let full = frame.area();
    if full.width < 24 || full.height < 5 {
        return;
    }
    let width = ((full.width * 70) / 100)
        .clamp(48, 100)
        .min(full.width.saturating_sub(2));

    let c = &MODAL_COLORS;
    let content: Vec<Line> = lines
        .iter()
        .map(|(text, role)| {
            let (fg, bold) = match role {
                'a' => (c.action, true),
                's' => (c.secondary, false),
                _ => (c.text, false),
            };
            let mut style = Style::default().fg(fg).bg(c.bg);
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(*text, style))
        })
        .collect();
    let content_width = width.saturating_sub(2).max(1) as usize;
    let content: Vec<Line<'static>> = content
        .iter()
        .flat_map(|line| super::ai_chat::styled_word_wrap_line(line, content_width))
        .map(Line::from)
        .collect();
    let requested_height = content.len().saturating_add(2).min(u16::MAX as usize) as u16;

    let dialog = Paragraph::new(content).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(c.border).bg(c.bg))
            .title(title)
            .title_style(
                Style::default()
                    .fg(c.title)
                    .bg(c.bg)
                    .add_modifier(Modifier::BOLD),
            ),
    );

    // The content is pre-wrapped with the same word-aware routine used by the
    // AI UI, so this height is exactly what will be rendered. In particular,
    // deliberate spacer lines cannot push the primary action below the border.
    // Compute the max first and cap the min by it: for short terminals the
    // preferred minimum (7) can exceed the available space, and
    // `Ord::clamp` panics when min > max.
    let max_height = full.height.saturating_sub(2).max(1);
    let height = requested_height.clamp(7.min(max_height), max_height);
    let area = centered_area(full, width, height);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(dialog, area);
}

/// Contextual ChatGPT sign-in for Ovim-owned direct Codex inference.
pub fn render_codex_auth_dialog(frame: &mut Frame, editor: &Editor) {
    use ovim_core::editor::CodexAuthDialogPhase;

    let Some(summary) = editor.codex_auth_dialog_summary() else {
        return;
    };
    let detail = summary.detail.unwrap_or_default();
    match summary.phase {
        CodexAuthDialogPhase::Offer => render_modal_dialog(
            frame,
            " Sign in to Codex ",
            &[
                ("Use your ChatGPT plan for Codex inference inside Ovim.", 't'),
                (
                    "Ovim keeps its own secure login and does not share refresh tokens with the Codex CLI.",
                    's',
                ),
                (detail.as_str(), 's'),
                (" ", 's'),
                (
                    "Enter: sign in   D: device code (SSH)   B: browser   Esc: not now",
                    'a',
                ),
            ],
        ),
        CodexAuthDialogPhase::Refreshing => render_modal_dialog(
            frame,
            " Refreshing Codex Sign-in ",
            &[
                ("Refreshing Ovim's Codex credentials…", 't'),
                ("Your draft and selection are preserved.", 's'),
                (" ", 's'),
                ("Esc: cancel", 'a'),
            ],
        ),
        CodexAuthDialogPhase::PreparingDeviceCode => render_modal_dialog(
            frame,
            " Preparing Device Sign-in ",
            &[
                ("Requesting a one-time sign-in code…", 't'),
                ("This works over SSH and on headless machines.", 's'),
                (" ", 's'),
                ("Esc: cancel", 'a'),
            ],
        ),
        CodexAuthDialogPhase::WaitingForDeviceCode => {
            let url = summary.authorize_url.unwrap_or_default();
            let code = summary.user_code.unwrap_or_default();
            render_modal_dialog(
                frame,
                " Finish Codex Device Sign-in ",
                &[
                    ("1. Open this URL in any browser:", 't'),
                    (url.as_str(), 's'),
                    ("2. Enter this one-time code (expires in 15 minutes):", 't'),
                    (code.as_str(), 'a'),
                    (
                        "Continue only if you started this login in Ovim. If someone else gave you this code, cancel.",
                        's',
                    ),
                    (" ", 's'),
                    ("Waiting for approval…   Esc: cancel", 'a'),
                ],
            );
        }
        CodexAuthDialogPhase::WaitingForBrowser => render_modal_dialog(
            frame,
            " Finish Sign-in in Your Browser ",
            &[
                ("Complete the OpenAI sign-in in the browser window.", 't'),
                ("Ovim will continue automatically when it succeeds.", 's'),
                (" ", 's'),
                ("O: reopen browser   Esc: cancel", 'a'),
            ],
        ),
        CodexAuthDialogPhase::Error => render_modal_dialog(
            frame,
            " Codex Sign-in Needs Attention ",
            &[
                (detail.as_str(), 't'),
                ("Your draft and selection are still here.", 's'),
                (" ", 's'),
                (
                    "Enter: try again   D: device code   B: browser   Esc: cancel",
                    'a',
                ),
            ],
        ),
    }
}

/// Centered consent dialog for LSP auto-install requests.
pub fn render_lsp_install_dialog(frame: &mut Frame, editor: &Editor, _theme: &Theme) {
    let Some((language, server, method)) = editor.pending_lsp_install_summary() else {
        return;
    };

    let summary = format!("Install {} for {} support?", server, language);
    let method_line = format!("Method: {}", method);
    render_modal_dialog(
        frame,
        " Install Language Server ",
        &[
            (&summary, 't'),
            (" ", 's'),
            (&method_line, 's'),
            (" ", 's'),
            ("Enter: install   A: always auto-install   Esc: skip", 'a'),
        ],
    );
}

/// Centered permission dialog for AI chat approval requests.
///
/// This is used for high-attention, blocking prompts (tool approval and
/// no-repo folder approval) instead of relying on low-visibility status bars.
pub fn render_ai_chat_permission_dialog(frame: &mut Frame, editor: &Editor, _theme: &Theme) {
    let pending_no_repo = editor.ai_chat_has_pending_no_repo_folder_approval();
    let pending_tool = editor.ai_chat_has_pending_tool_approval();
    let agent_snapshot = editor.ai_agent_current_snapshot().ok().flatten();
    let pending_agent = agent_snapshot
        .as_ref()
        .and_then(super::agent_tree::project_agent_approval_prompt);
    if !pending_no_repo && !pending_tool && pending_agent.is_none() {
        return;
    }

    let (title, summary, blocking, hints) = if pending_no_repo {
        (
            " Folder Access Permission ",
            editor
                .ai_chat_pending_no_repo_folder_approval_summary()
                .unwrap_or_else(|| "Allow folder access for this chat session?".to_string()),
            "This request blocks agent progress until resolved.",
            "Enter/Ctrl-Y allow   Esc/Ctrl-N deny",
        )
    } else if pending_tool {
        (
            " Tool Permission ",
            editor
                .ai_chat_pending_tool_approval_summary()
                .unwrap_or_else(|| "Allow requested tool action?".to_string()),
            "This request blocks agent progress until resolved.",
            "Enter/Ctrl-Y allow once   Ctrl-A allow for chat   Esc/Ctrl-N deny",
        )
    } else {
        (
            " Child Agent Permission ",
            pending_agent
                .map(|approval| approval.summary)
                .unwrap_or_else(|| "Allow requested child action?".to_string()),
            // A child pausing for approval never freezes the editor; only the
            // child waits. Keys reflect that non-blocking model.
            "This child agent is paused until you allow or deny; the editor stays interactive.",
            "Ctrl-Y allow   Ctrl-N deny   ·   a/d on the selected child in the agent tree (Ctrl-T)",
        )
    };

    render_modal_dialog(
        frame,
        title,
        &[
            (&summary, 't'),
            (" ", 's'),
            (blocking, 's'),
            (" ", 's'),
            (hints, 'a'),
        ],
    );
}

/// Compact code-page card or large centered concept-page panel.
pub fn render_ai_code_explanation(frame: &mut Frame, editor: &mut Editor) {
    editor.render_cache.code_explanation_answer_max_scroll = 0;
    let Some(view) = editor.ai_code_explanation_view() else {
        return;
    };
    let Some(cached) = editor.render_cache.last_buffer_area else {
        return;
    };
    let buffer = Rect::new(cached.x, cached.y, cached.width, cached.height);
    let (layout_width, layout_height, inner_width, title, teaching_text, concept_page) =
        match &view.page {
            ovim_core::editor::CodeExplanationPageView::Concept { title, body } => {
                let Some(layout) = ovim_core::editor::ConceptExplanationCardLayout::resolve(
                    buffer.width,
                    buffer.height,
                    body,
                    editor.indent_options().tab_width,
                ) else {
                    return;
                };
                let title_budget = layout.width.saturating_sub(28) as usize;
                (
                    layout.width,
                    layout.height,
                    layout.body_width,
                    format!(
                        " Concept {}/{} · {} ",
                        view.current,
                        view.total,
                        truncate_to_width(title, title_budget)
                    ),
                    body.clone(),
                    true,
                )
            }
            ovim_core::editor::CodeExplanationPageView::Code {
                path,
                start_line,
                end_line,
                comment,
            } => {
                let Some(layout) = ovim_core::editor::CodeExplanationCardLayout::resolve(
                    buffer.width,
                    buffer.height,
                    comment,
                    editor.indent_options().tab_width,
                ) else {
                    return;
                };
                let range = if start_line == end_line {
                    format!("{path}:{start_line}")
                } else {
                    format!("{path}:{start_line}-{end_line}")
                };
                (
                    layout.width,
                    layout.height,
                    layout.comment_width,
                    format!(
                        " Code walkthrough {}/{} · {range} ",
                        view.current, view.total
                    ),
                    comment.clone(),
                    false,
                )
            }
        };
    let height_limit = buffer
        .height
        .saturating_sub(2)
        .max(layout_height)
        .min(buffer.height);
    let discussion_row_limit = height_limit.saturating_sub(layout_height) as usize;
    let answer_row_limit = discussion_row_limit.saturating_sub(1).max(1);
    let discussion = walkthrough_discussion(
        &view.discussion,
        inner_width,
        answer_row_limit,
        view.answer_scroll,
    );
    editor.render_cache.code_explanation_answer_max_scroll = discussion.answer_max_scroll;
    let discussion_rows = discussion.lines.len() as u16;
    let height = layout_height
        .saturating_add(discussion_rows)
        .min(height_limit);
    let y = if concept_page {
        buffer.y + buffer.height.saturating_sub(height) / 2
    } else {
        let selection_rows = editor.ai_state.active_selection.as_ref().map(|selection| {
            walkthrough_selection_screen_rows(editor, selection.start_line, selection.end_line)
        });
        walkthrough_code_card_y(buffer, height, selection_rows)
    };
    let area = Rect::new(
        buffer.x + buffer.width.saturating_sub(layout_width) / 2,
        y,
        layout_width,
        height,
    );
    let mut content = vec![Line::from(Span::styled(
        teaching_text,
        Style::default().fg(MODAL_COLORS.text).bg(MODAL_COLORS.bg),
    ))];
    content.extend(discussion.lines);
    content.extend([
        Line::from(""),
        Line::from(Span::styled(
            discussion.hints,
            Style::default()
                .fg(MODAL_COLORS.action)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD),
        )),
    ]);
    let card = Paragraph::new(content)
        .style(Style::default().fg(MODAL_COLORS.text).bg(MODAL_COLORS.bg))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .style(Style::default().bg(MODAL_COLORS.bg))
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(MODAL_COLORS.border).bg(MODAL_COLORS.bg))
                .title(title)
                .title_style(
                    Style::default()
                        .fg(MODAL_COLORS.title)
                        .bg(MODAL_COLORS.bg)
                        .add_modifier(Modifier::BOLD),
                ),
        );
    frame.render_widget(Clear, area);
    frame.render_widget(card, area);
}

fn walkthrough_selection_screen_rows(
    editor: &Editor,
    selection_start: usize,
    selection_end: usize,
) -> (isize, isize) {
    let start_line = selection_start.min(selection_end);
    let end_line = selection_start.max(selection_end);
    let viewport_start = editor.scroll_offset();

    if editor.options.wrap {
        if let Some(wrap_map) = editor.wrap_map() {
            let viewport_row =
                wrap_map.viewport_top_visual_row(viewport_start, editor.scroll_subrow());
            let start_row = wrap_map.logical_to_visual(start_line);
            let end_row = wrap_map
                .logical_to_visual(end_line)
                .saturating_add(wrap_map.visual_lines_for(end_line) as usize)
                .saturating_sub(1);
            return (
                start_row as isize - viewport_row as isize,
                end_row as isize - viewport_row as isize,
            );
        }
    }

    (
        start_line as isize - viewport_start as isize,
        end_line as isize - viewport_start as isize,
    )
}

fn walkthrough_code_card_y(
    buffer: Rect,
    card_height: u16,
    selection_rows: Option<(isize, isize)>,
) -> u16 {
    let top_y = buffer.y;
    let bottom_y = buffer.bottom().saturating_sub(card_height);
    let Some((selection_start, selection_end)) = selection_rows else {
        return bottom_y;
    };
    let selection_start = buffer.y as isize + selection_start;
    let selection_end = buffer.y as isize + selection_end + 1;
    let overlaps = |card_y: u16| {
        let card_start = card_y as isize;
        let card_end = card_start + card_height as isize;
        selection_start < card_end && selection_end > card_start
    };

    if overlaps(bottom_y) && !overlaps(top_y) {
        top_y
    } else {
        bottom_y
    }
}

struct WalkthroughDiscussion {
    lines: Vec<Line<'static>>,
    hints: String,
    answer_max_scroll: usize,
}

struct WalkthroughExchange {
    lines: Vec<Line<'static>>,
    visible_start: usize,
    visible_end: usize,
    total_rows: usize,
    max_scroll: usize,
}

fn walkthrough_discussion(
    discussion: &ovim_core::editor::CodeExplanationDiscussionView,
    width: usize,
    answer_row_limit: usize,
    answer_scroll: usize,
) -> WalkthroughDiscussion {
    match discussion {
        ovim_core::editor::CodeExplanationDiscussionView::Navigating {
            question_count,
            latest_question: Some(question),
            latest_answer: Some(answer),
            latest_failed,
        } => {
            let exchange = walkthrough_exchange_lines(
                *question_count,
                question,
                answer,
                *latest_failed,
                width,
                answer_row_limit,
                answer_scroll,
            );
            let hints = if exchange.max_scroll > 0 {
                format!(
                    "↑/↓ reply {}–{}/{}   ←/→ steps   Space ask   Enter next/done   Esc dismiss",
                    exchange.visible_start + 1,
                    exchange.visible_end,
                    exchange.total_rows,
                )
            } else {
                "←/→ previous/next   Space ask   Enter next/done   Esc dismiss".into()
            };
            WalkthroughDiscussion {
                lines: exchange.lines,
                hints,
                answer_max_scroll: exchange.max_scroll,
            }
        }
        ovim_core::editor::CodeExplanationDiscussionView::Navigating { .. } => {
            WalkthroughDiscussion {
                lines: Vec::new(),
                hints: "←/→ previous/next   Space ask   Enter next/done   Esc dismiss".into(),
                answer_max_scroll: 0,
            }
        }
        ovim_core::editor::CodeExplanationDiscussionView::Composing {
            input,
            cursor,
            question_count,
        } => {
            let mut input_with_cursor = input.clone();
            input_with_cursor.insert((*cursor).min(input_with_cursor.len()), '▏');
            WalkthroughDiscussion {
                lines: vec![Line::from(Span::styled(
                    compact_walkthrough_line(
                        &format!("Ask {}: ", question_count + 1),
                        &input_with_cursor,
                        width,
                    ),
                    Style::default().fg(MODAL_COLORS.title).bg(MODAL_COLORS.bg),
                ))],
                hints: "Enter send   Shift-Enter newline   Esc cancel".into(),
                answer_max_scroll: 0,
            }
        }
        ovim_core::editor::CodeExplanationDiscussionView::Answering {
            question,
            answer,
            question_count,
        } => {
            let exchange = walkthrough_exchange_lines(
                *question_count,
                question,
                answer,
                false,
                width,
                answer_row_limit,
                answer_scroll,
            );
            let hints = if exchange.max_scroll > 0 {
                format!(
                    "Answering…   ↑/↓ reply {}–{}/{}   ←/→ steps   Esc dismiss",
                    exchange.visible_start + 1,
                    exchange.visible_end,
                    exchange.total_rows,
                )
            } else {
                "Answering…   ←/→ browse steps   Esc dismiss".into()
            };
            WalkthroughDiscussion {
                lines: exchange.lines,
                hints,
                answer_max_scroll: exchange.max_scroll,
            }
        }
    }
}

fn walkthrough_exchange_lines(
    question_count: usize,
    question: &str,
    answer: &str,
    failed: bool,
    width: usize,
    answer_row_limit: usize,
    answer_scroll: usize,
) -> WalkthroughExchange {
    let question_label = format!("Q{question_count}  ");
    let question_budget = width.saturating_sub(UnicodeWidthStr::width(question_label.as_str()));
    let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines = vec![Line::from(vec![
        Span::styled(
            question_label,
            Style::default()
                .fg(MODAL_COLORS.title)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_to_width(&question, question_budget),
            Style::default()
                .fg(MODAL_COLORS.secondary)
                .bg(MODAL_COLORS.bg),
        ),
    ])];

    let label = if failed { "Error  " } else { "AI  " };
    let label_width = UnicodeWidthStr::width(label);
    let answer_width = width.saturating_sub(label_width).max(1);
    let answer_rows = if answer.is_empty() {
        vec![vec![Span::styled(
            "Thinking…",
            Style::default()
                .fg(MODAL_COLORS.secondary)
                .bg(MODAL_COLORS.bg),
        )]]
    } else {
        let elements = super::markdown::parse_markdown(answer);
        super::markdown::render_markdown(&elements, answer_width, None)
            .iter()
            .flat_map(|line| super::ai_chat::styled_word_wrap_line(line, answer_width))
            .collect::<Vec<_>>()
    };
    let answer_row_limit = answer_row_limit.max(1);
    let total_rows = answer_rows.len();
    let max_scroll = total_rows.saturating_sub(answer_row_limit);
    let visible_start = answer_scroll.min(max_scroll);
    let visible_end = visible_start
        .saturating_add(answer_row_limit)
        .min(total_rows);
    for (index, row) in answer_rows
        .into_iter()
        .skip(visible_start)
        .take(answer_row_limit)
        .enumerate()
    {
        let prefix = if index == 0 {
            label.to_string()
        } else {
            " ".repeat(label_width)
        };
        let prefix_style = if failed {
            Style::default()
                .fg(Color::Red)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(MODAL_COLORS.title)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD)
        };
        let mut spans = Vec::with_capacity(row.len() + 1);
        spans.push(Span::styled(prefix, prefix_style));
        spans.extend(row);
        lines.push(Line::from(spans));
    }
    WalkthroughExchange {
        lines,
        visible_start,
        visible_end,
        total_rows,
        max_scroll,
    }
}

fn compact_walkthrough_line(label: &str, text: &str, width: usize) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let available = width.saturating_sub(UnicodeWidthStr::width(label));
    let (truncated, ellipsis) =
        if UnicodeWidthStr::width(single_line.as_str()) > available && available > 0 {
            (truncate_to_width(&single_line, available - 1), "…")
        } else {
            (truncate_to_width(&single_line, available), "")
        };
    format!("{label}{truncated}{ellipsis}")
}

/// Live and retained output for one agent-owned shell process.
pub fn render_ai_shell_process_inspector(frame: &mut Frame, editor: &Editor) {
    let Some(view) = editor.ai_shell_inspector_view() else {
        return;
    };
    let screen = frame.area();
    if screen.width < 24 || screen.height < 10 {
        return;
    }
    let width = (screen.width * 9 / 10).clamp(24, 120).min(screen.width);
    let height = (screen.height * 4 / 5).clamp(10, 40).min(screen.height);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let phase_color = match view.phase {
        ovim_core::editor::ShellProcessPhase::Succeeded => Color::Green,
        ovim_core::editor::ShellProcessPhase::Failed
        | ovim_core::editor::ShellProcessPhase::OutcomeUnknown => Color::Red,
        ovim_core::editor::ShellProcessPhase::Interrupted
        | ovim_core::editor::ShellProcessPhase::InterruptRequested => Color::Yellow,
        _ => MODAL_COLORS.title,
    };
    let title = format!(" Process Inspector · {} ", view.phase.label());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(phase_color).bg(MODAL_COLORS.bg))
        .title(title)
        .title_style(
            Style::default()
                .fg(phase_color)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);
    let content_width = chunks[0].width.saturating_sub(1) as usize;
    let command = truncate_to_width(&format!("$ {}", view.command), content_width);
    let cwd = truncate_to_width(
        &format!("cwd {}", view.workdir.display()),
        content_width.saturating_sub(1),
    );
    let pid = view
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "—".into());
    let last_output = view
        .last_output_age
        .map(|age| format!("{} ago", format_process_duration(age)))
        .unwrap_or_else(|| "none yet".into());
    let metrics = truncate_to_width(
        &format!(
            "pid {pid} · elapsed {} · last output {last_output}",
            format_process_duration(view.elapsed)
        ),
        content_width,
    );
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            command,
            Style::default()
                .fg(MODAL_COLORS.text)
                .bg(MODAL_COLORS.bg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            cwd,
            Style::default().fg(TEXT_DIM).bg(MODAL_COLORS.bg),
        )),
        Line::from(Span::styled(
            metrics,
            Style::default().fg(TEXT_DIM).bg(MODAL_COLORS.bg),
        )),
    ])
    .style(Style::default().bg(MODAL_COLORS.bg));
    frame.render_widget(header, chunks[0]);

    let mut output_lines = if view.expired {
        vec!["Output expired from the bounded process history.".to_string()]
    } else if view.output.is_empty() {
        let message = if view.phase.is_running() {
            "Waiting for process output…"
        } else {
            "This process produced no output."
        };
        vec![message.to_string()]
    } else {
        view.output.lines().map(str::to_owned).collect::<Vec<_>>()
    };
    let has_discarded_banner = view.dropped_bytes > 0 && !view.expired;
    if has_discarded_banner {
        output_lines.insert(
            0,
            format!(
                "… {} of older output discarded …",
                format_byte_count(view.dropped_bytes)
            ),
        );
    }
    let visible_rows = chunks[1].height as usize;
    let max_scroll = output_lines.len().saturating_sub(visible_rows);
    let scroll = if view.follow_latest {
        0
    } else {
        view.row_scroll_from_bottom.min(max_scroll)
    };
    let end = output_lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(visible_rows);
    let query = view.search_query.as_deref();
    let selected_match = view
        .search_match_line
        .map(|line| line + usize::from(has_discarded_banner));
    let lines = output_lines[start..end]
        .iter()
        .enumerate()
        .map(|(visible_index, line)| {
            let absolute_index = start + visible_index;
            let is_match = query.is_some_and(|query| line.contains(query));
            let style = if selected_match == Some(absolute_index) {
                Style::default()
                    .fg(Color::Black)
                    .bg(MODAL_COLORS.title)
                    .add_modifier(Modifier::BOLD)
            } else if is_match {
                Style::default().fg(Color::Yellow).bg(MODAL_COLORS.bg)
            } else {
                Style::default().fg(MODAL_COLORS.text).bg(MODAL_COLORS.bg)
            };
            Line::from(Span::styled(
                truncate_to_width(line, chunks[1].width as usize),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(MODAL_COLORS.bg)),
        chunks[1],
    );

    let position = if view.follow_latest {
        "FOLLOW".to_string()
    } else {
        format!("SCROLL +{scroll}")
    };
    let footer = if let Some(input) = view.search_input.as_deref() {
        vec![
            Line::from(Span::styled(
                format!("/{input}▏"),
                Style::default().fg(MODAL_COLORS.title).bg(MODAL_COLORS.bg),
            )),
            Line::from(Span::styled(
                "Enter find   Esc cancel search",
                Style::default().fg(MODAL_COLORS.action).bg(MODAL_COLORS.bg),
            )),
        ]
    } else if view.phase.is_running() {
        vec![
            Line::from(Span::styled(
                position,
                Style::default()
                    .fg(phase_color)
                    .bg(MODAL_COLORS.bg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "↑/↓ scroll   G follow   / search   n next   Ctrl-C interrupt   Ctrl-K force   Esc close",
                Style::default().fg(MODAL_COLORS.action).bg(MODAL_COLORS.bg),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                position,
                Style::default()
                    .fg(phase_color)
                    .bg(MODAL_COLORS.bg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "↑/↓ scroll   G bottom   / search   n next   Esc close",
                Style::default().fg(MODAL_COLORS.action).bg(MODAL_COLORS.bg),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().bg(MODAL_COLORS.bg)),
        chunks[2],
    );
}

fn format_process_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn format_byte_count(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// First-run and credential-recovery dialog for Exa-backed web search.
pub fn render_ai_chat_exa_setup_dialog(frame: &mut Frame, editor: &mut Editor) {
    editor.render_cache.ai_chat_exa_dashboard_hitbox = None;
    editor.render_cache.ai_chat_exa_input_cursor_pos = None;
    let Some((input, cursor, error, environment_override)) = editor.ai_chat_exa_setup_summary()
    else {
        return;
    };
    let full = frame.area();
    if full.width < 48 || full.height < 12 {
        return;
    }
    let width = ((full.width * 72) / 100)
        .clamp(56, 100)
        .min(full.width.saturating_sub(2));
    let height = if error.is_some() { 17 } else { 15 }.min(full.height.saturating_sub(2));
    let area = centered_area(full, width, height);
    let inner = Rect::new(
        area.x + 2,
        area.y + 2,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    let c = &MODAL_COLORS;

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(c.border).bg(c.bg))
            .title(" Enable Web Search ")
            .title_style(
                Style::default()
                    .fg(c.title)
                    .bg(c.bg)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
    );

    let intro = if environment_override {
        "Ovim found EXA_API_KEY in the environment. Replace it there if it is expired or revoked."
    } else {
        "Ovim uses Exa for live web search and readable page/PDF extraction. Your key is stored locally in Ovim's private configuration directory."
    };
    frame.render_widget(
        Paragraph::new(intro)
            .style(Style::default().fg(c.text).bg(c.bg))
            .wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y, inner.width, 3),
    );

    let link_y = inner.y + 4;
    let link_text = editor.ai_chat_exa_dashboard_url();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Get or manage a key: ",
                Style::default().fg(c.secondary).bg(c.bg),
            ),
            Span::styled(
                link_text,
                Style::default()
                    .fg(Color::Rgb(90, 170, 255))
                    .bg(c.bg)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ])),
        Rect::new(inner.x, link_y, inner.width, 1),
    );
    let prefix_width = UnicodeWidthStr::width("Get or manage a key: ") as u16;
    editor.render_cache.ai_chat_exa_dashboard_hitbox = Some(ovim_core::Rect {
        x: inner.x + prefix_width,
        y: link_y,
        width: (UnicodeWidthStr::width(link_text) as u16)
            .min(inner.width.saturating_sub(prefix_width)),
        height: 1,
    });

    let field_y = inner.y + 6;
    let field_width = inner.width.saturating_sub(2).max(1);
    // One bullet per grapheme, not per char: a multi-scalar grapheme in the
    // key (combining mark, emoji) must mask as one perceived character so
    // the bullet count and cursor column match what the user typed.
    use unicode_segmentation::UnicodeSegmentation;
    let masked = "•".repeat(input.graphemes(true).count());
    let visible_capacity = field_width.saturating_sub(1) as usize;
    let cursor_chars = input[..cursor.min(input.len())].graphemes(true).count();
    let scroll = cursor_chars.saturating_sub(visible_capacity);
    let visible = masked
        .chars()
        .skip(scroll)
        .take(visible_capacity)
        .collect::<String>();
    frame.render_widget(
        Paragraph::new(visible)
            .style(Style::default().fg(c.text).bg(Color::Rgb(22, 27, 35)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(c.secondary)),
            ),
        Rect::new(inner.x, field_y, inner.width, 3),
    );
    editor.render_cache.ai_chat_exa_input_cursor_pos = Some((
        inner.x + 1 + cursor_chars.saturating_sub(scroll).min(visible_capacity) as u16,
        field_y + 1,
    ));

    let mut hint_y = field_y + 4;
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error)
                .style(Style::default().fg(Color::Rgb(255, 105, 105)).bg(c.bg))
                .wrap(Wrap { trim: false }),
            Rect::new(inner.x, hint_y, inner.width, 2),
        );
        hint_y += 2;
    }
    frame.render_widget(
        Paragraph::new("Enter: save and enable   Esc: not now   /exa: reopen later").style(
            Style::default()
                .fg(c.action)
                .bg(c.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
}

pub fn render_ai_chat_image_modal_frame(frame: &mut Frame, editor: &Editor) {
    let Some(path) = editor.ai_chat_image_modal_path() else {
        return;
    };
    let full = frame.area();
    if full.width < 20 || full.height < 10 {
        return;
    }
    let area = centered_area(full, full.width * 4 / 5, full.height * 4 / 5);
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Image");
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(MODAL_COLORS.border).bg(MODAL_COLORS.bg))
            .title(format!(" {title} · Esc/click to close ")),
        area,
    );
    // The terminal-graphics pass runs after Ratatui and fills the bordered area.
}

#[cfg(test)]
mod tests {
    use ratatui::{
        backend::TestBackend,
        layout::Rect,
        style::Modifier,
        text::{Line, Span},
        Terminal,
    };

    use crate::editor::Editor;
    use ovim_core::ai::chat_types::{ChatOpts, ToolCallInfo};

    #[test]
    fn hover_width_uses_terminal_cells_in_preview_and_raw_modes() {
        let rendered = vec![Line::from(vec![Span::raw("型"), Span::raw(" information")])];

        assert_eq!(super::hover_content_width(&rendered, "ignored", true), 14);
        assert_eq!(super::hover_content_width(&[], "型 information", false), 14);
    }

    #[test]
    fn hover_preview_wraps_markdown_while_raw_mode_stays_lossless() {
        let markdown = concat!(
            "The `span` element doesn't mean anything on its own, but can be useful with global attributes.\n\n",
            "![Baseline icon](data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iMTgi)\n\n",
            "[MDN Reference](https://developer.mozilla.org/docs/Web/HTML/Reference/Elements/span)\n"
        );
        let preview =
            super::render_hover_preview_lines(markdown, 40, &crate::syntax::Theme::default());
        let preview_text = preview
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(preview.iter().all(|line| line.width() <= 36));
        assert!(preview_text.contains("Image: Baseline icon"));
        assert!(preview_text.contains("MDN Reference ↗"));
        assert!(!preview_text.contains("data:image"));
        assert_eq!(super::hover_raw_lines(markdown).join("\n"), markdown);
    }

    /// Regression test: heights 7 and 8 used to panic in `render_modal_dialog`
    /// because the clamp minimum (7) exceeded the available maximum
    /// (`height - 2`). Height 6 exercises the too-small early return.
    #[test]
    fn modal_dialog_renders_without_panicking_on_short_terminals() {
        for height in [6u16, 7, 8, 9, 24] {
            let backend = TestBackend::new(80, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    super::render_modal_dialog(
                        frame,
                        " Test ",
                        &[("line one", 't'), ("line two", 's'), ("[y]es [n]o", 'a')],
                    )
                })
                .unwrap();
        }
    }

    #[test]
    fn modal_dialog_keeps_primary_action_visible_when_body_wraps() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                super::render_modal_dialog(
                    frame,
                    " Sign in to Codex ",
                    &[
                        ("Use your ChatGPT plan for Codex inference inside Ovim.", 't'),
                        (
                            "Ovim keeps its own secure login and does not share refresh tokens with the Codex CLI.",
                            's',
                        ),
                        (
                            "Ovim's legacy Codex credentials cannot be reused safely; sign in to Ovim once",
                            's',
                        ),
                        (" ", 's'),
                        (
                            "Enter: sign in   D: device code (SSH)   B: browser   Esc: not now",
                            'a',
                        ),
                    ],
                )
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Enter: sign in"), "{rendered}");
    }

    #[test]
    fn walkthrough_reply_keeps_markdown_and_geometry_when_answering_completes() {
        let answer =
            "**Choosing `k` is safe here.** It is handled only after history receives focus.";
        let answering = ovim_core::editor::CodeExplanationDiscussionView::Answering {
            question: "Why is k used here?".into(),
            answer: answer.into(),
            question_count: 1,
        };
        let completed = ovim_core::editor::CodeExplanationDiscussionView::Navigating {
            question_count: 1,
            latest_question: Some("Why is k used here?".into()),
            latest_answer: Some(answer.into()),
            latest_failed: false,
        };

        let streaming = super::walkthrough_discussion(&answering, 72, 8, 0);
        let finished = super::walkthrough_discussion(&completed, 72, 8, 0);
        assert_eq!(streaming.lines, finished.lines);
        assert!(streaming.lines.len() >= 2);

        let rendered = streaming
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!rendered.contains("**"), "{rendered}");
        assert!(!rendered.contains('`'), "{rendered}");
        assert!(streaming
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| {
                span.content.contains("Choosing")
                    && span.style.add_modifier.contains(Modifier::BOLD)
            }));
    }

    #[test]
    fn walkthrough_reply_uses_available_rows_and_scrolls_in_place() {
        let discussion = ovim_core::editor::CodeExplanationDiscussionView::Navigating {
            question_count: 2,
            latest_question: Some("Can you give me the complete reasoning?".into()),
            latest_answer: Some(
                (1..=80)
                    .map(|word| format!("word{word}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            latest_failed: false,
        };
        let first = super::walkthrough_discussion(&discussion, 24, 10, 0);
        assert_eq!(first.lines.len(), 11);
        assert!(first.answer_max_scroll > 0);
        assert!(first.hints.contains("↑/↓ reply 1–10/"), "{}", first.hints);
        let first_rendered = first
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!first_rendered.contains("full reply"), "{first_rendered}");

        let later = super::walkthrough_discussion(&discussion, 24, 10, 1);
        let later_rendered = later
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_ne!(first_rendered, later_rendered);
        assert!(later.hints.contains("↑/↓ reply 2–11/"), "{}", later.hints);

        let following = super::walkthrough_discussion(&discussion, 24, 10, usize::MAX);
        assert!(
            following
                .hints
                .contains(&format!("–{0}/{0}", following.answer_max_scroll + 10)),
            "{}",
            following.hints
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn walkthrough_space_opens_visible_step_question_composer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let file = dir.path().join("demo.rs");
        std::fs::write(&file, "fn demo() {}\n").unwrap();
        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let profile = editor.ai_state.active_profile.clone();
        editor
            .ai_state
            .config
            .profiles
            .get_mut(&profile)
            .unwrap()
            .scope
            .files = ovim_core::ai::FileScope::Project;
        editor.set_last_layout(
            ovim_core::Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 22,
            },
            0,
            100,
            0,
        );
        let call = ToolCallInfo {
            id: "walkthrough".into(),
            name: "explain_with_codebase".into(),
            arguments: serde_json::json!({
                "steps": [{
                    "path": "demo.rs",
                    "start_line": 1,
                    "comment": "This function is the entry point."
                }]
            }),
        };
        let key = {
            let chat = editor.ai_state.chat.as_ref().unwrap();
            (chat.origin_buffer_id, chat.opts.name.clone())
        };
        let conversation = editor.ai_state.conversations.get_mut(&key).unwrap();
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![call.clone()],
        );
        conversation.append_tool_result(
            call.id.clone(),
            "User completed the code walkthrough (1 steps).".into(),
        );
        assert!(editor.replay_code_explanation(&call.id));
        ovim_core::editor::InputHandler::handle_key_event(
            &mut editor,
            ovim_core::KeyEvent::new(ovim_core::KeyCode::Char(' '), ovim_core::Modifiers::NONE),
        )
        .unwrap();
        for character in "Why?".chars() {
            ovim_core::editor::InputHandler::handle_key_event(
                &mut editor,
                ovim_core::KeyEvent::new(
                    ovim_core::KeyCode::Char(character),
                    ovim_core::Modifiers::NONE,
                ),
            )
            .unwrap();
        }

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_ai_code_explanation(frame, &mut editor))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Ask 1: Why?▏"), "{rendered}");
        assert!(rendered.contains("Enter send"), "{rendered}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concept_page_uses_a_large_centered_panel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let file = dir.path().join("demo.rs");
        std::fs::write(&file, "fn demo() {}\n").unwrap();
        let mut editor = Editor::default();
        editor.open_file(&file).unwrap();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor.set_last_layout(
            ovim_core::Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 22,
            },
            0,
            100,
            0,
        );
        let call = ToolCallInfo {
            id: "concept-walkthrough".into(),
            name: "explain_with_codebase".into(),
            arguments: serde_json::json!({
                "steps": [{
                    "type": "concept",
                    "title": "Two layers of history",
                    "body": "Input recall and conversation navigation are separate concerns."
                }]
            }),
        };
        let key = {
            let chat = editor.ai_state.chat.as_ref().unwrap();
            (chat.origin_buffer_id, chat.opts.name.clone())
        };
        let conversation = editor.ai_state.conversations.get_mut(&key).unwrap();
        conversation.append_assistant_message_with_tools(
            String::new(),
            "test".into(),
            vec![call.clone()],
        );
        conversation.append_tool_result(
            call.id.clone(),
            "User completed the walkthrough (1 page).".into(),
        );
        assert!(editor.replay_code_explanation(&call.id));

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_ai_code_explanation(frame, &mut editor))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let title_row = rows
            .iter()
            .position(|row| row.contains("Concept 1/1 · Two layers of history"))
            .expect("concept title");
        let body_row = rows
            .iter()
            .position(|row| row.contains("Input recall and conversation navigation"))
            .expect("concept body");

        assert!((4..=6).contains(&title_row), "title row: {title_row}");
        assert!(body_row > title_row);
        assert!(rows.iter().any(|row| row.contains("Space ask")));
    }

    #[test]
    fn code_walkthrough_card_moves_above_a_selection_at_viewport_bottom() {
        let buffer = Rect::new(0, 2, 100, 20);

        assert_eq!(
            super::walkthrough_code_card_y(buffer, 6, Some((16, 19))),
            buffer.y
        );
    }

    #[test]
    fn code_walkthrough_card_stays_below_a_selection_at_viewport_top() {
        let buffer = Rect::new(0, 2, 100, 20);

        assert_eq!(
            super::walkthrough_code_card_y(buffer, 6, Some((1, 3))),
            buffer.bottom() - 6
        );
    }
}
