use crate::editor::{Editor, Toast, ToastLevel};
use crate::syntax::{Theme, UiGroup};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Convert a core color to a ratatui color (convenience wrapper)
fn ui_color(theme: &Theme, group: UiGroup) -> Color {
    crate::key_convert::convert_core_color(theme.get_ui_color(group))
}

/// Renders the LSP progress line (just above status line)
pub fn render_progress_line(frame: &mut Frame, progress_msg: &str, area: Rect) {
    // Right-align the progress message
    let padding_len = progress_padding(area.width, progress_msg);
    let progress_line = Line::from(vec![
        Span::raw(" ".repeat(padding_len as usize)),
        Span::styled(
            format!(" {} ", progress_msg),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);

    let paragraph = Paragraph::new(progress_line).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

fn progress_padding(area_width: u16, message: &str) -> u16 {
    let message_width = message.width().min(u16::MAX as usize) as u16;
    area_width.saturating_sub(message_width.saturating_add(2))
}

/// Renders the tab bar with overflow handling
pub fn render_tab_bar(frame: &mut Frame, editor: &Editor, theme: &Theme, area: Rect) {
    let tabs = editor.tab_page_manager().tabs();
    let current_index = editor.current_tab_index();

    let tab_fill = ui_color(theme, UiGroup::TabFill);
    let tab_active_bg = ui_color(theme, UiGroup::TabActiveBg);
    let tab_active_fg = ui_color(theme, UiGroup::TabActiveFg);
    let tab_inactive_bg = ui_color(theme, UiGroup::TabInactiveBg);
    let tab_inactive_fg = ui_color(theme, UiGroup::TabInactiveFg);

    if tabs.is_empty() {
        let tab_line = Line::from(Span::styled(
            " ".repeat(area.width as usize),
            Style::default().bg(tab_fill),
        ));
        let paragraph = Paragraph::new(tab_line).style(Style::default().bg(tab_fill));
        frame.render_widget(paragraph, area);
        return;
    }

    let mut spans = Vec::new();
    let available_width = area.width as usize;

    const MIN_TAB_WIDTH: usize = 10;
    const SEPARATOR_WIDTH: usize = 1;
    const OVERFLOW_INDICATOR_WIDTH: usize = 12;

    let mut tab_widths: Vec<usize> = Vec::new();
    let mut total_width = 0;

    for (i, _tab) in tabs.iter().enumerate() {
        let title = editor.get_tab_title(i);
        let tab_text = format!(" {} {} ", i + 1, title);
        let tab_width = tab_text.width();
        tab_widths.push(tab_width);
        total_width += tab_width;
        if i < tabs.len() - 1 {
            total_width += SEPARATOR_WIDTH;
        }
    }

    let active_style = Style::default()
        .fg(tab_active_fg)
        .bg(tab_active_bg)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(tab_inactive_fg).bg(tab_inactive_bg);
    let separator_style = Style::default().bg(tab_fill);
    let overflow_style = Style::default()
        .fg(ui_color(theme, UiGroup::Warning))
        .bg(tab_inactive_bg)
        .add_modifier(Modifier::ITALIC);

    if total_width > available_width {
        let mut visible_tabs = Vec::new();

        let current_tab_width = tab_widths[current_index].max(MIN_TAB_WIDTH);
        visible_tabs.push(current_index);
        let mut used_width = current_tab_width + OVERFLOW_INDICATOR_WIDTH;

        let mut before_idx = current_index.saturating_sub(1);
        let mut after_idx = current_index + 1;
        let mut add_before = current_index > 0;
        let mut add_after = after_idx < tabs.len();

        while (add_before || add_after) && used_width < available_width {
            if add_before {
                let tab_width = tab_widths[before_idx].max(MIN_TAB_WIDTH) + SEPARATOR_WIDTH;
                if used_width + tab_width
                    <= available_width.saturating_sub(OVERFLOW_INDICATOR_WIDTH)
                {
                    visible_tabs.insert(0, before_idx);
                    used_width += tab_width;
                    if before_idx > 0 {
                        before_idx -= 1;
                    } else {
                        add_before = false;
                    }
                } else {
                    add_before = false;
                }
            }

            if add_after && used_width < available_width {
                let tab_width = tab_widths[after_idx].max(MIN_TAB_WIDTH) + SEPARATOR_WIDTH;
                if used_width + tab_width
                    <= available_width.saturating_sub(OVERFLOW_INDICATOR_WIDTH)
                {
                    visible_tabs.push(after_idx);
                    used_width += tab_width;
                    after_idx += 1;
                    if after_idx >= tabs.len() {
                        add_after = false;
                    }
                } else {
                    add_after = false;
                }
            }
        }

        let hidden_before = visible_tabs.first().copied().unwrap_or(0);
        if hidden_before > 0 {
            let overflow_text = format!(" +{} ", hidden_before);
            spans.push(Span::styled(overflow_text, overflow_style));
            spans.push(Span::styled(" ", separator_style));
        }

        for (idx, &tab_idx) in visible_tabs.iter().enumerate() {
            let is_current = tab_idx == current_index;
            let title = editor.get_tab_title(tab_idx);
            let tab_text = format!(" {} {} ", tab_idx + 1, title);

            let style = if is_current {
                active_style
            } else {
                inactive_style
            };

            spans.push(Span::styled(tab_text, style));

            if idx < visible_tabs.len() - 1 {
                spans.push(Span::styled(" ", separator_style));
            }
        }

        let hidden_after = tabs
            .len()
            .saturating_sub(visible_tabs.last().copied().unwrap_or(0) + 1);
        if hidden_after > 0 {
            spans.push(Span::styled(" ", separator_style));
            let overflow_text = format!(" +{} ", hidden_after);
            spans.push(Span::styled(overflow_text, overflow_style));
        }
    } else {
        for (i, _tab) in tabs.iter().enumerate() {
            let is_current = i == current_index;
            let title = editor.get_tab_title(i);
            let tab_text = format!(" {} {} ", i + 1, title);

            let style = if is_current {
                active_style
            } else {
                inactive_style
            };

            spans.push(Span::styled(tab_text, style));

            if i < tabs.len() - 1 {
                spans.push(Span::styled(" ", separator_style));
            }
        }
    }

    let content_width: usize = spans.iter().map(|s| s.content.width()).sum();
    let remaining = (area.width as usize).saturating_sub(content_width);
    if remaining > 0 {
        spans.push(Span::styled(
            " ".repeat(remaining),
            Style::default().bg(tab_fill),
        ));
    }

    let tab_line = Line::from(spans);
    let paragraph = Paragraph::new(tab_line).style(Style::default().bg(tab_fill));
    frame.render_widget(paragraph, area);
}

/// Renders the status line
pub fn render_status_line(frame: &mut Frame, editor: &Editor, theme: &Theme, area: Rect) {
    // Review mode: compact single-line indicator
    if editor.ai_chat_review_mode() {
        render_review_mode_status(frame, editor, theme, area);
        return;
    }

    let mode = editor.mode();
    let buffer = editor.buffer();
    let cursor = buffer.cursor();

    // Build status line content
    let mode_indicator = format!(" {} ", mode.display_name());
    let recording_indicator = if editor.is_recording_macro() {
        if let Some(reg) = editor.recording_register() {
            format!(" recording @{} ", reg)
        } else {
            " recording ".to_string()
        }
    } else {
        String::new()
    };
    let position = format!(" {}:{} ", cursor.line() + 1, cursor.col().0 + 1);
    let modified = if editor.is_modified() { " [+] " } else { " " };
    let file = buffer
        .file_path()
        .or_else(|| buffer.display_name())
        .unwrap_or("[No Name]");
    let branch_display = editor
        .git_branch()
        .map(|b| format!(" {}", b))
        .unwrap_or_default();

    let status_bg = ui_color(theme, UiGroup::StatusLineBackground);
    let status_fg = ui_color(theme, UiGroup::StatusLineForeground);
    let accent_bg = ui_color(theme, UiGroup::TabActiveBg);
    let accent_fg = ui_color(theme, UiGroup::TabActiveFg);
    let error_color = ui_color(theme, UiGroup::Error);

    let mut spans = vec![Span::styled(
        &mode_indicator,
        Style::default()
            .fg(accent_fg)
            .bg(accent_bg)
            .add_modifier(Modifier::BOLD),
    )];

    // Add recording indicator if recording
    if !recording_indicator.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            &recording_indicator,
            Style::default()
                .fg(Color::White)
                .bg(error_color)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw(" "));
    }

    spans.push(Span::styled(file, Style::default().fg(status_fg)));
    spans.push(Span::styled(modified, Style::default().fg(status_fg)));
    if !branch_display.is_empty() {
        spans.push(Span::styled(
            &branch_display,
            Style::default().fg(status_fg).add_modifier(Modifier::DIM),
        ));
    }

    // Right-side widgets differ for AI chat mode
    let is_ai_chat = mode == crate::mode::Mode::AiChat;
    let mut right_spans: Vec<Span> = Vec::new();

    if is_ai_chat {
        // AI chat right-side: profile:model, tool iterations, streaming status, position
        if let Some(followed) = editor.ai_agent_follow_status() {
            let followed: String = followed.chars().take(48).collect();
            right_spans.push(Span::styled(
                format!(" ↳ {followed} "),
                Style::default()
                    .fg(Color::Rgb(130, 205, 235))
                    .bg(status_bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let active_profile = editor.ai_chat_effective_profile();
        let model_display = editor
            .ai_state
            .config
            .resolve_profile(&active_profile)
            .map(|p| {
                let short: String = p.model.chars().take(16).collect();
                format!(" {}:{} ", active_profile, short)
            })
            .unwrap_or_else(|| format!(" {} ", active_profile));
        right_spans.push(Span::styled(
            model_display,
            Style::default()
                .fg(Color::Rgb(180, 188, 202))
                .bg(Color::Rgb(46, 52, 64)),
        ));

        if let Some(chat) = editor.ai_state.chat.as_ref() {
            if chat.tool_call_count > 0 {
                let iter_text = match editor.ai_chat_tool_call_limit() {
                    Some(max_calls) => {
                        format!(" \u{26A1}{}/{} ", chat.tool_call_count, max_calls)
                    }
                    None => format!(" \u{26A1}{} ", chat.tool_call_count),
                };
                right_spans.push(Span::styled(
                    iter_text,
                    Style::default().fg(Color::Yellow).bg(status_bg),
                ));
            }

            if chat.waiting {
                let status_text = if chat.streaming_content.is_some() {
                    " streaming... "
                } else if chat.streaming_thinking.is_some() {
                    " thinking... "
                } else {
                    " waiting... "
                };
                right_spans.push(Span::styled(
                    status_text,
                    Style::default()
                        .fg(Color::Rgb(120, 180, 255))
                        .bg(status_bg)
                        .add_modifier(Modifier::ITALIC),
                ));
            }

            if let (Some(policy), Some(mode)) = (
                editor.ai_chat_save_policy_label(),
                editor.ai_chat_save_mode_label(),
            ) {
                let save_text = format!(" save:{mode} ");
                right_spans.push(Span::styled(
                    save_text,
                    Style::default().fg(Color::Rgb(150, 165, 190)).bg(status_bg),
                ));
                if policy != "only_if_clean_at_start" {
                    right_spans.push(Span::styled(
                        format!(" ({policy}) "),
                        Style::default()
                            .fg(Color::Rgb(126, 140, 165))
                            .bg(status_bg)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
        }

        right_spans.push(Span::styled(
            &position,
            Style::default()
                .fg(accent_fg)
                .bg(accent_bg)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        // Normal right-side: diagnostics, latest status, position
        let (errors, warnings, _info, _hints) = editor.cached_diagnostic_count();
        let diagnostics = if errors > 0 || warnings > 0 {
            format!(" E:{} W:{} ", errors, warnings)
        } else {
            String::new()
        };

        let status_message = if !editor.status_message().is_empty() {
            format!(" {} ", editor.status_message())
        } else if editor.current_lsp_server_name().is_some() {
            " LSP ".to_string()
        } else {
            String::new()
        };

        if !diagnostics.is_empty() {
            right_spans.push(Span::styled(
                diagnostics,
                Style::default().fg(Color::Black).bg(if errors > 0 {
                    error_color
                } else {
                    ui_color(theme, UiGroup::Warning)
                }),
            ));
        }

        if !status_message.is_empty() {
            let status_color = if editor.status_message().contains("Failed")
                || editor.status_message().contains("Error")
            {
                error_color
            } else if editor.status_message().contains("ready") {
                Color::Green
            } else {
                ui_color(theme, UiGroup::Info)
            };
            right_spans.push(Span::styled(
                status_message,
                Style::default().fg(Color::Black).bg(status_color),
            ));
        }

        right_spans.push(Span::styled(
            &position,
            Style::default()
                .fg(accent_fg)
                .bg(accent_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Calculate padding (display columns, not bytes: unicode filenames,
    // branches, and status messages must not shift the right-side spans)
    let recording_len = if !recording_indicator.is_empty() {
        UnicodeWidthStr::width(recording_indicator.as_str()) + 1
    } else {
        1
    };
    let left_used = UnicodeWidthStr::width(mode_indicator.as_str())
        + recording_len
        + UnicodeWidthStr::width(file)
        + UnicodeWidthStr::width(modified)
        + UnicodeWidthStr::width(branch_display.as_str());
    let right_used: usize = right_spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let padding_len = (area.width as usize).saturating_sub(left_used + right_used);

    spans.push(Span::raw(" ".repeat(padding_len)));
    spans.extend(right_spans);

    let status_line = Line::from(spans);

    let paragraph = Paragraph::new(status_line).style(Style::default().bg(status_bg).fg(status_fg));
    frame.render_widget(paragraph, area);
}

/// Renders the command line
pub fn render_command_line(frame: &mut Frame, editor: &Editor, area: Rect) {
    let command_text = format!(":{}", editor.command_line());

    let command_line = Line::from(vec![Span::styled(
        command_text,
        Style::default().fg(Color::White).bg(Color::Black),
    )]);

    let paragraph = Paragraph::new(command_line).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

/// Renders the path completion popup above the command line.
pub fn render_path_completion(frame: &mut Frame, editor: &Editor, status_area: Rect) {
    let state = editor.path_completion();
    if !state.is_visible() {
        return;
    }

    let entries = state.entries();
    let selected = state.selected_index();

    let max_visible = 10usize;
    let num_items = entries.len().min(max_visible);
    if num_items == 0 {
        return;
    }

    // Scroll window so selected item is always visible.
    let scroll_offset = if selected >= max_visible {
        selected - max_visible + 1
    } else {
        0
    };

    let menu_height = num_items as u16 + 2; // +2 for borders
    let max_name_len = entries
        .iter()
        .skip(scroll_offset)
        .take(max_visible)
        .map(|e| {
            let display_len = e.name.width();
            if e.is_dir {
                display_len + 1
            } else {
                display_len
            }
        })
        .max()
        .unwrap_or(20);
    let menu_width = (max_name_len + 4).clamp(20, 60) as u16;

    // Position above the status line, left-aligned.
    let menu_y = status_area.y.saturating_sub(menu_height);
    let menu_x = status_area.x;
    let menu_area = Rect::new(
        menu_x,
        menu_y,
        menu_width.min(status_area.width),
        menu_height,
    );

    // Build list items.
    let items: Vec<ListItem> = entries
        .iter()
        .skip(scroll_offset)
        .take(max_visible)
        .enumerate()
        .map(|(i, entry)| {
            let display = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            let is_selected = (i + scroll_offset) == selected;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(display, style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::Black)),
    );

    frame.render_widget(ratatui::widgets::Clear, menu_area);
    frame.render_widget(list, menu_area);
}

/// Renders the message line (command line area when not in command/search mode).
/// Shows editor status messages, command feedback, or blank.
pub fn render_message_line(frame: &mut Frame, editor: &Editor, area: Rect) {
    let message = editor.status_message();
    let line = if !message.is_empty() {
        Line::from(vec![Span::styled(
            message.to_string(),
            Style::default().fg(Color::White).bg(Color::Black),
        )])
    } else {
        diagnostic_echo_line(editor, area.width as usize).unwrap_or_default()
    };

    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

/// When the message line is idle, echo the diagnostic under the cursor —
/// severity-tagged and colored, truncated to the row. Complements the
/// width-capped inline virtual text: the echo has the whole row to spend,
/// so the message is readable without opening the `<Space>e` float.
fn diagnostic_echo_line(editor: &Editor, width: usize) -> Option<Line<'static>> {
    let diagnostic = editor.diagnostic_at_cursor()?;
    let (label, color) = match diagnostic.severity {
        Some(lsp_types::DiagnosticSeverity::WARNING) => ("W", Color::Yellow),
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => ("I", Color::Cyan),
        Some(lsp_types::DiagnosticSeverity::HINT) => ("H", Color::Gray),
        // ERROR, missing severity, or any unknown value.
        _ => ("E", Color::Red),
    };

    let mut text = diagnostic.message.lines().next().unwrap_or("").to_string();
    if let Some(source) = diagnostic.source.as_deref() {
        let code = match &diagnostic.code {
            Some(lsp_types::NumberOrString::Number(n)) => format!(" {n}"),
            Some(lsp_types::NumberOrString::String(s)) => format!(" {s}"),
            None => String::new(),
        };
        text.push_str(&format!(" [{source}{code}]"));
    }

    let prefix = format!("{label}: ");
    let body_width = width.saturating_sub(prefix.width());
    if body_width == 0 {
        return None;
    }
    Some(Line::from(vec![
        Span::styled(
            prefix,
            Style::default()
                .fg(color)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_with_ellipsis(&text, body_width),
            Style::default().fg(Color::White).bg(Color::Black),
        ),
    ]))
}

/// Renders the search line
pub fn render_search_line(frame: &mut Frame, editor: &Editor, area: Rect) {
    let search_prefix = if editor.search.search_forward {
        "/"
    } else {
        "?"
    };
    let search_text = format!("{}{}", search_prefix, editor.search_buffer());

    let search_line = Line::from(vec![Span::styled(
        search_text,
        Style::default().fg(Color::White).bg(Color::Black),
    )]);

    let paragraph = Paragraph::new(search_line).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

/// Renders the rename input line
pub fn render_rename_input(frame: &mut Frame, editor: &Editor, area: Rect) {
    let text = format!("rename: {}", editor.rename_buffer());

    let line = Line::from(vec![Span::styled(
        text,
        Style::default().fg(Color::White).bg(Color::Black),
    )]);

    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

fn truncate_with_ellipsis(input: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if input.width() <= max_width {
        return input.to_string();
    }

    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;

    for grapheme in input.graphemes(true) {
        let w = grapheme.width();
        if used + w > max_width - 1 {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

fn toast_accent(theme: &Theme, level: ToastLevel) -> Color {
    match level {
        ToastLevel::Error => ui_color(theme, UiGroup::Error),
        ToastLevel::Warning => ui_color(theme, UiGroup::Warning),
        ToastLevel::Success => ui_color(theme, UiGroup::Success),
        ToastLevel::Info => ui_color(theme, UiGroup::Info),
    }
}

fn toast_glyph(level: ToastLevel) -> &'static str {
    match level {
        ToastLevel::Error => "×",
        ToastLevel::Warning => "!",
        ToastLevel::Success => "✓",
        ToastLevel::Info => "•",
    }
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_toast_message(text: &str) -> String {
    text.trim()
        .split('\n')
        .map(single_line)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToastRow {
    level: ToastLevel,
    label: String,
    message: String,
    repeat: u32,
}

impl ToastRow {
    fn from_toast(toast: Toast) -> Self {
        let label = toast
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| toast.source.label())
            .to_string();
        Self {
            level: toast.level,
            label,
            message: normalize_toast_message(&toast.message),
            repeat: toast.repeat,
        }
    }

    fn status(level: ToastLevel, label: &str, message: impl AsRef<str>) -> Self {
        Self {
            level,
            label: label.to_string(),
            message: normalize_toast_message(message.as_ref()),
            repeat: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToastCardLayout {
    width: usize,
    title: String,
    body: Vec<String>,
}

impl ToastCardLayout {
    fn height(&self) -> usize {
        self.body.len() + 2
    }
}

fn toast_title(row: &ToastRow) -> String {
    if row.repeat > 1 {
        format!("{} {} · {}×", toast_glyph(row.level), row.label, row.repeat)
    } else {
        format!("{} {}", toast_glyph(row.level), row.label)
    }
}

fn wrap_toast_message(message: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for logical_line in message.split('\n') {
        if logical_line.is_empty() {
            rows.push(String::new());
            continue;
        }
        let line = Line::from(logical_line.to_string());
        rows.extend(
            super::ai_chat::styled_word_wrap_line(&line, width)
                .into_iter()
                .map(|spans| {
                    spans
                        .into_iter()
                        .map(|span| span.content.into_owned())
                        .collect()
                }),
        );
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn line_with_ellipsis(line: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in line.graphemes(true) {
        let width = grapheme.width();
        if used + width > max_width - 1 {
            break;
        }
        out.push_str(grapheme);
        used += width;
    }
    out.push('…');
    out
}

fn layout_toast_card(
    row: &ToastRow,
    max_width: usize,
    max_body_rows: usize,
) -> Option<ToastCardLayout> {
    const MIN_WIDTH: usize = 24;
    const MAX_WIDTH: usize = 56;

    if max_width < 12 || max_body_rows == 0 {
        return None;
    }

    let title = toast_title(row);
    let natural_body_width = row
        .message
        .split('\n')
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or_default();
    let desired_width = natural_body_width
        .saturating_add(4)
        .max(title.width().saturating_add(4));
    let upper = max_width.min(MAX_WIDTH);
    let lower = MIN_WIDTH.min(upper);
    let width = desired_width.clamp(lower, upper);
    let content_width = width.saturating_sub(4).max(1);
    let mut body = wrap_toast_message(&row.message, content_width);

    if body.len() > max_body_rows {
        body.truncate(max_body_rows);
        if let Some(last) = body.last_mut() {
            *last = line_with_ellipsis(last, content_width);
        }
    }

    Some(ToastCardLayout {
        width,
        title: truncate_with_ellipsis(&title, width.saturating_sub(4)),
        body,
    })
}

/// `"1 error"` / `"3 errors"` — count with a correctly pluralized noun.
fn count_label(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Renders a top-right toast stack over the buffer area.
///
/// Persistent AI activity and diagnostics are followed by transient toasts from
/// the editor's toast center.
pub fn render_top_right_toasts(
    frame: &mut Frame,
    editor: &Editor,
    theme: &Theme,
    buffer_area: Rect,
) {
    let mut rows: Vec<ToastRow> = Vec::new();

    if let Some((message, level)) = hidden_ai_chat_status(editor) {
        let message = message.trim();
        rows.push(ToastRow::status(
            level,
            "AI",
            message.strip_prefix("AI ").unwrap_or(message),
        ));
    }

    if !editor.diagnostic_badge_dismissed() {
        let (errors, warnings, _, _) = editor.cached_diagnostic_count();
        if errors > 0 || warnings > 0 {
            let counts = if errors > 0 && warnings > 0 {
                format!(
                    "{} · {}",
                    count_label(errors, "error"),
                    count_label(warnings, "warning")
                )
            } else if errors > 0 {
                count_label(errors, "error")
            } else {
                count_label(warnings, "warning")
            };
            let message = format!("{counts}\n<Space>e inspects at cursor");
            rows.push(ToastRow::status(
                if errors > 0 {
                    ToastLevel::Error
                } else {
                    ToastLevel::Warning
                },
                "Diagnostics",
                message,
            ));
        }
    }

    rows.extend(
        editor
            .visible_toasts_newest_first(4)
            .into_iter()
            .map(ToastRow::from_toast),
    );

    if rows.is_empty() {
        return;
    }

    if buffer_area.height < 3 {
        return;
    }

    let available_width = buffer_area.width.saturating_sub(2) as usize;
    let stack_height = ((buffer_area.height as usize * 2) / 3)
        .max(3)
        .min(buffer_area.height as usize)
        .min(24);
    let mut y = buffer_area.y;
    let mut used_height = 0usize;

    for row in rows.into_iter().take(5) {
        let remaining_height = stack_height.saturating_sub(used_height);
        if remaining_height < 3 {
            break;
        }
        let max_body_rows = remaining_height.saturating_sub(2).min(6);
        let Some(card) = layout_toast_card(&row, available_width, max_body_rows) else {
            continue;
        };
        let card_height = card.height();
        if card_height > remaining_height {
            break;
        }

        let width = card.width as u16;
        let height = card_height as u16;
        let x = buffer_area.right().saturating_sub(width + 1);
        let area = Rect {
            x,
            y,
            width,
            height,
        };

        let background = ui_color(theme, UiGroup::MenuBackground);
        let foreground = ui_color(theme, UiGroup::Foreground);
        let accent = toast_accent(theme, row.level);
        let base = Style::default().fg(foreground).bg(background);
        let content = card
            .body
            .into_iter()
            .map(|line| Line::from(Span::styled(format!(" {line}"), base)))
            .collect::<Vec<_>>();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent).bg(background))
            .style(base)
            .title(Span::styled(
                format!(" {} ", card.title),
                Style::default()
                    .fg(accent)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
            ));

        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(content).style(base).block(block), area);

        let occupied = card_height + 1;
        used_height = used_height.saturating_add(occupied);
        y = y.saturating_add(occupied as u16);
    }
}

fn hidden_ai_chat_status(editor: &Editor) -> Option<(String, ToastLevel)> {
    hidden_ai_chat_status_for(
        editor.mode() == crate::mode::Mode::AiChat,
        editor.ai_chat_activity(),
    )
}

fn hidden_ai_chat_status_for(
    chat_open: bool,
    activity: ovim_core::editor::AiChatActivity,
) -> Option<(String, ToastLevel)> {
    if chat_open {
        return None;
    }
    match activity {
        ovim_core::editor::AiChatActivity::Idle => None,
        ovim_core::editor::AiChatActivity::WaitingToolApproval => {
            Some((" AI approval needed ".to_string(), ToastLevel::Warning))
        }
        ovim_core::editor::AiChatActivity::WaitingFolderApproval => {
            Some((" AI folder approval ".to_string(), ToastLevel::Warning))
        }
        ovim_core::editor::AiChatActivity::WaitingCodeExplanation => {
            Some((" AI walkthrough ready ".to_string(), ToastLevel::Warning))
        }
        _ => Some((" AI working… ".to_string(), ToastLevel::Info)),
    }
}

/// Renders contextual widgets in the left and right margins when textwidth centering
/// creates extra space. Left margin shows git info, right margin shows diagnostics/LSP.
pub fn render_margin_widgets(
    frame: &mut Frame,
    editor: &Editor,
    theme: &Theme,
    full_area: Rect,
    buffer_area: Rect,
) {
    let dim_style = Style::default()
        .fg(ui_color(theme, UiGroup::StatusLineForeground))
        .add_modifier(Modifier::DIM);

    // ── Left margin: git branch + change summary ──
    let left_margin_width = buffer_area.x.saturating_sub(full_area.x) as usize;
    if left_margin_width >= 12 {
        let mut parts: Vec<Span> = Vec::new();

        if let Some(branch) = editor.git_branch() {
            // Truncate branch name to fit margin (leave room for change stats)
            let max_branch = left_margin_width.saturating_sub(3); // 1 gap + some padding
                                                                  // Truncate by display columns at grapheme boundaries: byte
                                                                  // slicing panics mid-char on non-ascii branch names.
            let display = if unicode_width::UnicodeWidthStr::width(branch) > max_branch {
                format!(
                    "{}~",
                    crate::ui::renderer::helpers::truncate_to_width(
                        branch,
                        max_branch.saturating_sub(1)
                    )
                )
            } else {
                branch.to_string()
            };
            parts.push(Span::styled(format!(" {}", display), dim_style));
        }

        // Change summary from git status
        let (added, modified, removed) = editor.buffer().git_status().change_counts();
        if added > 0 || modified > 0 || removed > 0 {
            let mut summary = String::new();
            if added > 0 {
                summary.push_str(&format!(" +{}", added));
            }
            if modified > 0 {
                summary.push_str(&format!(" ~{}", modified));
            }
            if removed > 0 {
                summary.push_str(&format!(" -{}", removed));
            }
            parts.push(Span::styled(summary, dim_style));
        }

        if !parts.is_empty() {
            // Right-align within the left margin, 1 col gap before buffer
            let content_width: usize = parts.iter().map(|s| s.width()).sum();
            let padding = left_margin_width.saturating_sub(content_width + 1);

            let mut spans = vec![Span::raw(" ".repeat(padding))];
            spans.extend(parts);

            let line = Line::from(spans);
            let area = Rect {
                x: full_area.x,
                y: full_area.y,
                width: left_margin_width as u16,
                height: 1,
            };
            frame.render_widget(Paragraph::new(line), area);
        }
    }

    // ── Right margin: diagnostic counts + LSP status ──
    //
    // Note: this widget renders on row 0 of the right margin. Buffer EOL
    // diagnostics are free to extend into the right margin too, so on a
    // file whose first visible line carries a long diagnostic the two
    // would overlap. To minimize that collision we only render this
    // widget when there's something useful to show (errors, warnings, or
    // an active LSP) — otherwise the leading 1-col gap would clobber the
    // last char of any EOL diagnostic that ends at the box edge.
    let right_margin_start = buffer_area.x + buffer_area.width;
    let right_margin_width =
        (full_area.x + full_area.width).saturating_sub(right_margin_start) as usize;
    let (errors, warnings, _, _) = editor.cached_diagnostic_count();
    let has_lsp = editor.current_lsp_server_name().is_some();
    if right_margin_width >= 12 && (errors > 0 || warnings > 0 || has_lsp) {
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::raw(" ")); // 1 col gap after buffer

        if errors > 0 {
            spans.push(Span::styled(
                format!("E:{}", errors),
                Style::default().fg(ui_color(theme, UiGroup::Error)),
            ));
            spans.push(Span::raw(" "));
        }
        if warnings > 0 {
            spans.push(Span::styled(
                format!("W:{}", warnings),
                Style::default().fg(ui_color(theme, UiGroup::Warning)),
            ));
            spans.push(Span::raw(" "));
        }

        // LSP availability badge
        if has_lsp {
            let status_text = "LSP ready".to_string();
            let lsp_color = if status_text.contains("Failed") || status_text.contains("Error") {
                ui_color(theme, UiGroup::Error)
            } else if status_text.contains("ready") {
                Color::Green
            } else {
                ui_color(theme, UiGroup::Info)
            };
            // Truncate if too long for margin
            let max_len = right_margin_width
                .saturating_sub(spans.iter().map(|s| s.width()).sum::<usize>() + 1);
            let display = if unicode_width::UnicodeWidthStr::width(status_text.as_str()) > max_len {
                format!(
                    "{}~",
                    crate::ui::renderer::helpers::truncate_to_width(
                        &status_text,
                        max_len.saturating_sub(1)
                    )
                )
            } else {
                status_text
            };
            spans.push(Span::styled(display, Style::default().fg(lsp_color)));
        }

        let line = Line::from(spans);
        let area = Rect {
            x: right_margin_start,
            y: full_area.y,
            width: right_margin_width as u16,
            height: 1,
        };
        frame.render_widget(Paragraph::new(line), area);
    }
}
fn render_review_mode_status(frame: &mut Frame, editor: &Editor, theme: &Theme, area: Rect) {
    let accent_bg = ui_color(theme, UiGroup::TabActiveBg);
    let accent_fg = ui_color(theme, UiGroup::TabActiveFg);
    let status_bg = ui_color(theme, UiGroup::StatusLineBackground);
    let status_fg = ui_color(theme, UiGroup::StatusLineForeground);

    let edit_count = editor
        .ai_chat_state()
        .map(|c| c.agent_edits.total_edit_count())
        .unwrap_or(0);
    let file_count = editor
        .ai_chat_state()
        .map(|c| c.agent_edits.edited_buffer_count())
        .unwrap_or(0);
    let active_target = review_target_path_hint(editor, 34);
    let pending_state = if editor.ai_chat_has_pending_tool_approval() {
        "approval pending"
    } else if editor.ai_chat_has_pending_no_repo_folder_approval() {
        "folder approval pending"
    } else if editor.ai_chat_waiting() {
        "agent running"
    } else {
        "ready"
    };
    let save_mode = editor.ai_chat_save_mode_label().unwrap_or("unknown");

    let mode_span = Span::styled(
        " REVIEW ",
        Style::default()
            .fg(accent_fg)
            .bg(accent_bg)
            .add_modifier(Modifier::BOLD),
    );

    let info = format!(
        " {} edit{} in {} file{} \u{00b7} {} \u{00b7} {} \u{00b7} save:{} ",
        edit_count,
        if edit_count == 1 { "" } else { "s" },
        file_count,
        if file_count == 1 { "" } else { "s" },
        active_target,
        pending_state,
        save_mode,
    );
    let hints = if editor.ai_chat_has_pending_work() {
        " \u{2190}/\u{2192} edits  Enter/Esc locked while pending  Ctrl-r chat "
    } else {
        " \u{2190}/\u{2192} edits  Enter accept  Ctrl-r chat  Esc close "
    };
    let w = area.width as usize;
    let mode_width = UnicodeWidthStr::width(" REVIEW ");
    let max_hint_width = w.saturating_sub(mode_width + 12).min(44);
    let hints = truncate_tail(hints, max_hint_width);
    let max_info_width = w.saturating_sub(mode_width + UnicodeWidthStr::width(hints.as_str()));
    let info = truncate_middle(&info, max_info_width);

    let info_span = Span::styled(info, Style::default().fg(status_fg).bg(status_bg));
    let hints_span = Span::styled(
        hints,
        Style::default()
            .fg(Color::DarkGray)
            .bg(status_bg)
            .add_modifier(Modifier::DIM),
    );
    let used = mode_width
        + UnicodeWidthStr::width(info_span.content.as_ref())
        + UnicodeWidthStr::width(hints_span.content.as_ref());
    let gap = w.saturating_sub(used);
    let gap_span = Span::styled(" ".repeat(gap), Style::default().bg(status_bg));

    let line = Line::from(vec![mode_span, info_span, gap_span, hints_span]);
    frame.render_widget(Paragraph::new(vec![line]), area);
}

fn review_target_path_hint(editor: &Editor, max_chars: usize) -> String {
    let path = editor
        .ai_chat_state()
        .and_then(|c| editor.get_buffer_by_id(c.active_buffer_id))
        .and_then(|b| b.file_path().or_else(|| b.display_name()))
        .unwrap_or("[No Name]");
    compact_path_hint(path, max_chars)
}

fn compact_path_hint(path: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return truncate_middle(path, max_chars);
    }

    let mut tail = parts[parts.len() - 1].to_string();
    for idx in (0..parts.len().saturating_sub(1)).rev() {
        let candidate = format!("{}/{}", parts[idx], tail);
        if candidate.chars().count() + 2 > max_chars {
            break;
        }
        tail = candidate;
    }

    if tail == normalized || parts.len() == 1 {
        truncate_middle(&tail, max_chars)
    } else {
        truncate_middle(&format!("\u{2026}/{}", tail), max_chars)
    }
}

fn truncate_tail(text: &str, max_cols: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_cols {
        return text.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "\u{2026}".to_string();
    }
    let mut out = crate::ui::renderer::helpers::truncate_to_width(text, max_cols - 1);
    out.push('\u{2026}');
    out
}

fn truncate_middle(text: &str, max_cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    if UnicodeWidthStr::width(text) <= max_cols {
        return text.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    if max_cols <= 3 {
        return truncate_tail(text, max_cols);
    }

    // Head and tail budgets in display columns; graphemes stay whole so
    // emoji sequences and wide chars never get split by the ellipsis.
    let head_budget = (max_cols - 1) / 2;
    let tail_budget = max_cols - head_budget - 1;
    let start = crate::ui::renderer::helpers::truncate_to_width(text, head_budget);

    let mut tail_graphemes: Vec<&str> = Vec::new();
    let mut tail_width = 0;
    for grapheme in text.graphemes(true).rev() {
        let width = crate::display::grapheme_display_width(grapheme);
        if tail_width + width > tail_budget {
            break;
        }
        tail_graphemes.push(grapheme);
        tail_width += width;
    }
    let end: String = tail_graphemes.into_iter().rev().collect();
    format!("{}\u{2026}{}", start, end)
}

#[cfg(test)]
mod tests {
    use super::{
        compact_path_hint, hidden_ai_chat_status_for, progress_padding, truncate_middle,
        truncate_with_ellipsis, ToastLevel, ToastRow,
    };
    use crate::editor::{ToastRequest, ToastSource};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn compact_path_hint_keeps_disambiguating_tail() {
        let path = "/workspace/packages/ovim/src/ui/renderer/ai_chat.rs";
        let hint = compact_path_hint(path, 24);
        assert!(hint.ends_with("renderer/ai_chat.rs"));
    }

    #[test]
    fn truncate_middle_preserves_both_sides() {
        let text = "edits in 3 files · src/ui/renderer/ai_chat.rs";
        let out = truncate_middle(text, 20);
        assert!(out.starts_with("edits"));
        assert!(out.ends_with("chat.rs"));
        assert!(out.contains('…'));
    }

    #[test]
    fn hidden_running_chat_has_compact_top_right_status() {
        use ovim_core::editor::AiChatActivity;

        assert!(hidden_ai_chat_status_for(true, AiChatActivity::Inference).is_none());
        let (text, _) = hidden_ai_chat_status_for(false, AiChatActivity::RunningShell)
            .expect("hidden AI status");
        assert_eq!(text, " AI working… ");
        let (text, level) =
            hidden_ai_chat_status_for(false, AiChatActivity::WaitingCodeExplanation)
                .expect("walkthrough status");
        assert_eq!(text, " AI walkthrough ready ");
        assert_eq!(level, ToastLevel::Warning);
    }

    #[test]
    fn toast_row_uses_title_once_and_preserves_multiline_messages() {
        let mut center = crate::editor::ToastCenter::new();
        center.push(
            ToastRequest::new(
                ToastSource::Lsp,
                ToastLevel::Error,
                "Completion failed\nNo server is available",
            )
            .with_title("LSP"),
        );

        let row = ToastRow::from_toast(center.visible_toasts_newest_first(1).remove(0));

        assert_eq!(row.label, "LSP");
        assert_eq!(row.message, "Completion failed\nNo server is available");
    }

    #[test]
    fn toast_card_wraps_complete_unicode_text_at_word_boundaries() {
        let row = ToastRow::status(
            ToastLevel::Warning,
            "LSP",
            "解析中: the language server did not respond in time. Retry with :LspRestart.",
        );
        let card = super::layout_toast_card(&row, 34, 6).expect("toast card layout");
        let content_width = card.width.saturating_sub(4);

        assert!(card.width <= 34);
        assert!(card.body.len() > 1);
        assert!(card.body.iter().all(|line| line.width() <= content_width));
        assert_eq!(
            card.body.join(" "),
            "解析中: the language server did not respond in time. Retry with :LspRestart."
        );
    }

    #[test]
    fn toast_card_caps_extreme_messages_with_one_ellipsis() {
        let row = ToastRow::status(
            ToastLevel::Error,
            "Plugin failed",
            "First recovery detail that wraps across rows and keeps going with more context than a notification should cover.",
        );
        let card = super::layout_toast_card(&row, 30, 2).expect("toast card layout");

        assert_eq!(card.body.len(), 2);
        assert!(card.body.last().is_some_and(|line| line.ends_with('…')));
        assert_eq!(card.body.join(" ").matches('…').count(), 1);
        assert_eq!(super::line_with_ellipsis("full", 4), "ful…");
    }

    #[test]
    fn toast_levels_have_distinct_theme_aware_accents() {
        let theme = crate::syntax::Theme::default();
        let accents = [
            ToastLevel::Info,
            ToastLevel::Success,
            ToastLevel::Warning,
            ToastLevel::Error,
        ]
        .map(|level| super::toast_accent(&theme, level));

        for (index, accent) in accents.iter().enumerate() {
            assert!(!accents[..index].contains(accent));
        }
    }

    #[test]
    fn toast_renderer_draws_a_rounded_type_colored_badge() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};

        let mut editor = crate::editor::Editor::with_content("fn main() {}\n");
        editor.push_toast(
            ToastRequest::new(
                ToastSource::Lsp,
                ToastLevel::Warning,
                "Completion failed because no language server is available for this document. Install one and retry.",
            )
            .with_title("LSP")
            .with_sticky(true),
        );
        let theme = crate::syntax::Theme::default();
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                super::render_top_right_toasts(frame, &editor, &theme, Rect::new(0, 0, 60, 18))
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let top_left = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "╭")
            .expect("rounded toast border");

        assert!(rows.iter().any(|row| row.contains("! LSP")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("Completion failed")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Install one and retry.")),
            "{rows:?}"
        );
        assert_eq!(
            top_left.fg,
            super::toast_accent(&theme, ToastLevel::Warning)
        );
    }

    #[test]
    fn toast_renderer_separates_stacked_badges() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};

        let mut editor = crate::editor::Editor::with_content("fn main() {}\n");
        for (level, title, message) in [
            (ToastLevel::Success, "Saved", "All changes were written."),
            (
                ToastLevel::Info,
                "Update",
                "A newer language server is available.",
            ),
        ] {
            editor.push_toast(
                ToastRequest::new(ToastSource::System, level, message)
                    .with_title(title)
                    .with_sticky(true),
            );
        }

        let theme = crate::syntax::Theme::default();
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                super::render_top_right_toasts(frame, &editor, &theme, Rect::new(0, 0, 60, 22))
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let border_rows = (0..buffer.area.height)
            .filter(|&y| (0..buffer.area.width).any(|x| buffer[(x, y)].symbol() == "╭"))
            .collect::<Vec<_>>();

        assert_eq!(border_rows.len(), 2);
        assert!(border_rows[1] >= border_rows[0] + 4, "{border_rows:?}");
    }

    #[test]
    fn toast_truncation_uses_a_single_width_aware_ellipsis() {
        assert_eq!(truncate_with_ellipsis("alpha beta", 6), "alpha…");
        assert_eq!(truncate_with_ellipsis("界abc", 4), "界a…");
        assert_eq!(truncate_with_ellipsis("👩‍💻abc", 3), "👩‍💻…");
        assert_eq!(truncate_with_ellipsis("hello", 1), "…");
    }

    #[test]
    fn progress_alignment_measures_terminal_cells_not_utf8_bytes() {
        assert_eq!(progress_padding(20, "解析中"), 12);
    }
}
