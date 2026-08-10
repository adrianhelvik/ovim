use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::editor::lsp_manager_panel::{InstallStatus, LspManagerPanel, LspSection};
use unicode_width::UnicodeWidthStr;

fn list_row_padding(area_width: u16, language_name: &str, command: &str) -> usize {
    usize::from(area_width)
        .saturating_sub(6 + language_name.width() + command.width())
        .max(2)
}

// Color palette
mod colors {
    use ratatui::style::Color;

    pub const BG: Color = Color::Reset;
    pub const BORDER: Color = Color::Rgb(80, 85, 110);
    pub const TITLE: Color = Color::Rgb(140, 160, 240);
    pub const SELECTED_BG: Color = Color::Rgb(45, 50, 70);
    pub const SECTION_HEADER: Color = Color::Rgb(100, 110, 140);
    pub const TEXT: Color = Color::Rgb(200, 205, 215);
    pub const TEXT_MUTED: Color = Color::Rgb(100, 110, 140);
    pub const TEXT_BRIGHT: Color = Color::Rgb(240, 240, 255);
    pub const GREEN: Color = Color::Rgb(80, 200, 120);
    pub const CYAN: Color = Color::Rgb(100, 200, 220);
    pub const YELLOW: Color = Color::Rgb(230, 200, 80);
    pub const RED: Color = Color::Rgb(220, 80, 80);
    pub const HINT_KEY: Color = Color::Rgb(140, 160, 240);
    pub const HINT_TEXT: Color = Color::Rgb(100, 110, 140);
    pub const FILTER_BG: Color = Color::Rgb(35, 38, 52);
}

pub fn get_lsp_manager_area(full_area: Rect) -> Rect {
    let width = ((full_area.width * 80) / 100).max(60).min(full_area.width);
    let height = ((full_area.height * 75) / 100)
        .max(15)
        .min(full_area.height);
    let x = full_area.width.saturating_sub(width) / 2;
    let y = full_area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

pub fn render_lsp_manager(frame: &mut Frame, panel: &LspManagerPanel) {
    let area = get_lsp_manager_area(frame.area());

    // Clear underlying content
    frame.render_widget(Clear, area);

    let (running, installed, available, _syntax_only) = panel.section_counts();
    let header_right = format!(" {} running, {} available ", running, installed + available);

    let block = Block::default()
        .title_top(Line::from(Span::styled(
            "  LSP Manager ",
            Style::default()
                .fg(colors::TITLE)
                .add_modifier(Modifier::BOLD),
        )))
        .title_top(
            Line::from(Span::styled(
                &header_right,
                Style::default().fg(colors::TEXT_MUTED),
            ))
            .right_aligned(),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::BORDER))
        .style(Style::default().bg(colors::BG));

    frame.render_widget(&block, area);
    let inner = block.inner(area);

    if inner.height < 4 || inner.width < 20 {
        return;
    }

    // Layout: filter bar (1 line) + separator (1 line) + content + hint bar (1 line)
    let content_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // filter
            Constraint::Length(1), // separator
            Constraint::Min(1),    // content
            Constraint::Length(1), // hint bar
        ])
        .split(inner);

    render_filter_bar(frame, panel, content_chunks[0]);
    render_separator(frame, content_chunks[1]);

    // Split content into list + detail (if detail is shown and wide enough)
    let content_area = content_chunks[2];
    let show_detail = panel.show_detail && content_area.width >= 50;

    if show_detail {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(content_area);

        render_entry_list(frame, panel, h_chunks[0]);
        render_detail_separator(frame, h_chunks[1]);
        let detail_area = Rect {
            x: h_chunks[1].x + 1,
            y: h_chunks[1].y,
            width: h_chunks[1].width.saturating_sub(1),
            height: h_chunks[1].height,
        };
        render_detail_pane(frame, panel, detail_area);
    } else {
        render_entry_list(frame, panel, content_area);
    }

    render_hint_bar(frame, panel, content_chunks[3]);
}

fn render_filter_bar(frame: &mut Frame, panel: &LspManagerPanel, area: Rect) {
    let icon = "/ ";
    let query = panel.filter_query();

    let text = if panel.filter_focused {
        Line::from(vec![
            Span::styled(icon, Style::default().fg(colors::HINT_KEY)),
            Span::styled(
                if query.is_empty() { "Filter..." } else { query },
                Style::default().fg(if query.is_empty() {
                    colors::TEXT_MUTED
                } else {
                    colors::TEXT_BRIGHT
                }),
            ),
        ])
    } else if query.is_empty() {
        Line::from(Span::styled(
            format!("{icon}Filter..."),
            Style::default().fg(colors::TEXT_MUTED),
        ))
    } else {
        Line::from(vec![
            Span::styled(icon, Style::default().fg(colors::HINT_KEY)),
            Span::styled(query, Style::default().fg(colors::TEXT)),
        ])
    };

    let bg = if panel.filter_focused {
        colors::FILTER_BG
    } else {
        colors::BG
    };

    frame.render_widget(Paragraph::new(text).style(Style::default().bg(bg)), area);
}

fn render_separator(frame: &mut Frame, area: Rect) {
    let sep = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            sep,
            Style::default().fg(colors::BORDER),
        ))),
        area,
    );
}

fn render_detail_separator(frame: &mut Frame, area: Rect) {
    // Render a vertical separator on the left edge of the detail area
    for row in 0..area.height {
        let sep_area = Rect {
            x: area.x,
            y: area.y + row,
            width: 1,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "│",
                Style::default().fg(colors::BORDER),
            ))),
            sep_area,
        );
    }
}

fn render_entry_list(frame: &mut Frame, panel: &LspManagerPanel, area: Rect) {
    let filtered = panel.filtered_entries();
    if filtered.is_empty() {
        let msg = if panel.filter_query().is_empty() {
            "No languages configured"
        } else {
            "No matching languages"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {msg}"),
                Style::default().fg(colors::TEXT_MUTED),
            ))),
            area,
        );
        return;
    }

    // Build lines with section headers
    let mut lines: Vec<(Line, bool, usize)> = Vec::new(); // (line, is_header, original_idx)
    let mut current_section: Option<LspSection> = None;

    for (idx, entry) in &filtered {
        // Insert section header if section changed
        if current_section != Some(entry.section) {
            current_section = Some(entry.section);
            let count = filtered
                .iter()
                .filter(|(_, e)| e.section == entry.section)
                .count();
            let header = Line::from(vec![Span::styled(
                format!("  {} ({count})", entry.section.label()),
                Style::default()
                    .fg(colors::SECTION_HEADER)
                    .add_modifier(Modifier::BOLD),
            )]);
            lines.push((header, true, *idx));
        }

        let is_selected = *idx == panel.selected_index;
        let icon_color = match entry.section {
            LspSection::Running => colors::GREEN,
            LspSection::Installed => colors::CYAN,
            LspSection::Available => colors::TEXT_MUTED,
            LspSection::SyntaxOnly => colors::TEXT_MUTED,
        };

        // Check install status
        let install_info = panel.active_installs.get(&entry.language_id);

        let mut spans = vec![
            Span::styled(
                format!("  {} ", entry.section.icon()),
                Style::default().fg(icon_color),
            ),
            Span::styled(
                &entry.language_name,
                Style::default()
                    .fg(if is_selected {
                        colors::TEXT_BRIGHT
                    } else {
                        colors::TEXT
                    })
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ];

        // Show LSP server name
        if let Some(cmd) = &entry.lsp_command {
            let padding = list_row_padding(area.width, &entry.language_name, cmd);
            spans.push(Span::styled(" ".repeat(padding), Style::default()));
            spans.push(Span::styled(
                cmd.as_str(),
                Style::default().fg(colors::TEXT_MUTED),
            ));
        }

        // Show install status
        if let Some(status) = install_info {
            let status_span = match status {
                InstallStatus::Installing(msg) => {
                    Span::styled(format!(" ⟳ {msg}"), Style::default().fg(colors::YELLOW))
                }
                InstallStatus::Success => {
                    Span::styled(" ✓ Installed", Style::default().fg(colors::GREEN))
                }
                InstallStatus::Failed(err) => {
                    Span::styled(format!(" ✗ {err}"), Style::default().fg(colors::RED))
                }
            };
            spans.push(status_span);
        }

        let style = if is_selected {
            Style::default().bg(colors::SELECTED_BG)
        } else {
            Style::default()
        };

        lines.push((Line::from(spans).style(style), false, *idx));
    }

    // Compute scroll offset
    let selected_line_idx = lines
        .iter()
        .position(|(_, is_header, idx)| !is_header && *idx == panel.selected_index)
        .unwrap_or(0);
    let visible_height = area.height as usize;
    let scroll = if selected_line_idx >= visible_height {
        selected_line_idx - visible_height + 1
    } else {
        0
    };

    // Render visible lines
    for (i, (line, _, _)) in lines.iter().skip(scroll).take(visible_height).enumerate() {
        let line_area = Rect {
            x: area.x,
            y: area.y + i as u16,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(line.clone()), line_area);
    }

    // Scrollbar
    if lines.len() > visible_height {
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut state =
            ScrollbarState::new(lines.len().saturating_sub(visible_height)).position(scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("█"),
            scrollbar_area,
            &mut state,
        );
    }
}

fn render_detail_pane(frame: &mut Frame, panel: &LspManagerPanel, area: Rect) {
    let Some(entry) = panel.selected_entry() else {
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        &entry.language_name,
        Style::default()
            .fg(colors::TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "─".repeat(entry.language_name.width().min(area.width as usize)),
        Style::default().fg(colors::BORDER),
    )));
    lines.push(Line::default());

    // Server info
    if let Some(cmd) = &entry.lsp_command {
        lines.push(detail_line("Server", cmd));
    }

    if let Some(state) = &entry.server_state {
        let color = if state == "Running" {
            colors::GREEN
        } else {
            colors::TEXT
        };
        lines.push(Line::from(vec![
            Span::styled("  State:  ", Style::default().fg(colors::TEXT_MUTED)),
            Span::styled(state.as_str(), Style::default().fg(color)),
        ]));
    }

    // Extensions
    if !entry.extensions.is_empty() {
        lines.push(Line::default());
        let exts = entry
            .extensions
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(detail_line("Extensions", &exts));
    }

    // Root markers
    if !entry.root_markers.is_empty() {
        let markers = entry.root_markers.join(", ");
        lines.push(detail_line("Root markers", &markers));
    }

    // Install info
    if entry.has_auto_install {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  Auto-install: available",
            Style::default().fg(colors::GREEN),
        )));
    }

    if let Some(hint) = &entry.install_hint {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  Install:",
            Style::default()
                .fg(colors::TEXT_MUTED)
                .add_modifier(Modifier::BOLD),
        )));
        // Wrap long install hints
        for line in hint.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(colors::TEXT),
            )));
        }
    }

    // Section status
    lines.push(Line::default());
    let section_text = match entry.section {
        LspSection::Running => "● Server is running",
        LspSection::Installed => "○ Server installed (not running)",
        LspSection::Available => "◻ Server not installed",
        LspSection::SyntaxOnly => "─ No LSP configured (syntax only)",
    };
    let section_color = match entry.section {
        LspSection::Running => colors::GREEN,
        LspSection::Installed => colors::CYAN,
        LspSection::Available => colors::TEXT_MUTED,
        LspSection::SyntaxOnly => colors::TEXT_MUTED,
    };
    lines.push(Line::from(Span::styled(
        format!("  {section_text}"),
        Style::default().fg(section_color),
    )));

    // Render
    let height = area.height as usize;
    for (i, line) in lines.iter().take(height).enumerate() {
        let line_area = Rect {
            x: area.x + 1,
            y: area.y + i as u16,
            width: area.width.saturating_sub(1),
            height: 1,
        };
        frame.render_widget(Paragraph::new(line.clone()), line_area);
    }
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {label}: "),
            Style::default().fg(colors::TEXT_MUTED),
        ),
        Span::styled(value.to_string(), Style::default().fg(colors::TEXT)),
    ])
}

fn render_hint_bar(frame: &mut Frame, panel: &LspManagerPanel, area: Rect) {
    let sep = "─".repeat(area.width as usize);
    // Draw a thin separator above the hint bar
    if area.y > 0 {
        let sep_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                sep,
                Style::default().fg(colors::BORDER),
            ))),
            sep_area,
        );
    }

    // Build hint spans
    let hints = if panel.filter_focused {
        vec![("Esc", "cancel"), ("Enter", "apply")]
    } else {
        let mut h = vec![("j/k", "navigate"), ("/", "filter"), ("K", "details")];
        if let Some(entry) = panel.selected_entry() {
            match entry.section {
                LspSection::Available => {
                    if entry.has_auto_install {
                        h.push(("i", "install"));
                    }
                }
                LspSection::Installed => {
                    h.push(("x", "uninstall"));
                    h.push(("u", "update"));
                }
                LspSection::Running => {
                    h.push(("x", "uninstall"));
                    h.push(("u", "update"));
                }
                LspSection::SyntaxOnly => {}
            }
        }
        h.push(("q", "close"));
        h
    };

    let mut spans = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(colors::HINT_KEY)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            desc.to_string(),
            Style::default().fg(colors::HINT_TEXT),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::list_row_padding;

    #[test]
    fn list_padding_uses_terminal_cells_for_unicode_names_and_commands() {
        assert_eq!(list_row_padding(30, "Rust", "rust"), 16);
        assert_eq!(list_row_padding(30, "界界", "λx"), 18);
    }
}
