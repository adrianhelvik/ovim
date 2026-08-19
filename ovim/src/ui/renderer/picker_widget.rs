use crate::editor::fuzzy::rematch_positions;
use crate::editor::Editor;
use crate::syntax::HighlightGroup;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
    Frame,
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

use super::helpers::{expand_tabs, expand_tabs_with_mapping, truncate_to_width};
use super::styles::remap_highlights;

// Picker color palette — single point of tuning for the entire fuzzy finder UI
mod picker_colors {
    use ratatui::style::Color;

    pub const BG: Color = Color::Reset;
    pub const BG_ALT: Color = Color::Reset;
    pub const SELECTED: Color = Color::Rgb(45, 50, 70);
    pub const BORDER: Color = Color::Rgb(80, 85, 110);
    pub const SEPARATOR: Color = Color::Rgb(50, 55, 75);
    pub const TITLE: Color = Color::Rgb(140, 160, 240);
    pub const TEXT: Color = Color::Rgb(200, 205, 215);
    pub const TEXT_BRIGHT: Color = Color::Rgb(240, 240, 255);
    pub const TEXT_MUTED: Color = Color::Rgb(100, 110, 140);
    pub const GREEN: Color = Color::Rgb(129, 250, 183);
    /// Soft blue for filenames in grep results — visually distinct anchor
    pub const FILENAME: Color = Color::Rgb(130, 170, 255);
}

/// A widget that fills every cell in an area with a styled space.
/// This is the proper way to create a solid background - unlike Block which
/// only renders borders, Fill writes to every cell, preventing bleed-through.
pub(super) struct Fill {
    style: Style,
}

impl Fill {
    fn new(style: Style) -> Self {
        Self { style }
    }

    pub(super) fn bg(color: Color) -> Self {
        Self::new(Style::default().bg(color))
    }
}

impl Widget for Fill {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_char(' ').set_style(self.style);
            }
        }
    }
}

/// Binary search for the highlight group at a given byte index.
/// Highlights must be sorted by range.start (ascending).
/// Returns the highlight group if byte_idx falls within any range.
/// O(log n) instead of O(n) linear search.
#[inline]
fn find_highlight_at_byte(
    highlights: &[(Range<usize>, HighlightGroup)],
    byte_idx: usize,
) -> Option<HighlightGroup> {
    if highlights.is_empty() {
        return None;
    }

    // Binary search: find the first range where start > byte_idx
    // All ranges that could contain byte_idx have start <= byte_idx
    let partition = highlights.partition_point(|(range, _)| range.start <= byte_idx);

    if partition == 0 {
        // No range starts at or before byte_idx
        return None;
    }

    // Check ranges from partition-1 backwards (most specific/last defined wins)
    // In practice, we usually only need to check 1-2 ranges
    for i in (0..partition).rev() {
        let (range, group) = &highlights[i];
        if range.end > byte_idx {
            return Some(*group);
        }
        // Optimization: if this range ends before byte_idx and the previous
        // range also starts before this one ends, we can stop
        if i > 0 && highlights[i - 1].0.end <= range.start {
            break;
        }
    }

    None
}

/// Calculates the picker overlay area (centered, takes up 80% of screen)
pub fn get_picker_area(full_area: Rect) -> Rect {
    let width = ((full_area.width * 80) / 100).max(60).min(full_area.width);
    let height = ((full_area.height * 60) / 100)
        .max(15)
        .min(full_area.height);
    let x = full_area.width.saturating_sub(width) / 2;
    let y = full_area.height.saturating_sub(height) / 2;

    Rect::new(x, y, width, height)
}

/// Determines if we should show the preview panel based on available width
fn should_show_preview(area: Rect) -> bool {
    // Show preview only if we have at least 100 columns total
    // This leaves ~40 cols for the list and ~60 for preview
    area.width >= 100
}

/// Renders the picker overlay
pub fn render_picker(frame: &mut Frame, editor: &mut Editor) {
    let Some(picker) = editor.picker() else {
        return;
    };

    let picker_area = get_picker_area(frame.area());
    let show_preview = should_show_preview(picker_area)
        && matches!(
            picker.mode(),
            crate::editor::PickerMode::FindFiles
                | crate::editor::PickerMode::LiveGrep
                | crate::editor::PickerMode::LspLocations
        );

    // Clear underlying content, then fill with picker background
    frame.render_widget(ratatui::widgets::Clear, picker_area);
    frame.render_widget(Fill::bg(picker_colors::BG), picker_area);

    let mode_name = match picker.mode() {
        crate::editor::PickerMode::FindFiles => " \u{f0224} Find Files ",
        crate::editor::PickerMode::LiveGrep => " \u{f0dae} Live Grep ",
        crate::editor::PickerMode::Custom => " \u{f0489} Select ",
        crate::editor::PickerMode::Completion => " \u{f0} Completion ",
        crate::editor::PickerMode::LspLocations => " \u{f0627} Navigation ",
    };

    // Build right-aligned result count for the title bar
    let result_count_title = {
        let filtered = picker.filtered_result_count();
        let total = picker.all_results_count();
        format!(" {}/{} ", filtered, total)
    };

    let block = Block::default()
        .title_top(Line::from(Span::styled(
            mode_name,
            Style::default()
                .fg(picker_colors::TITLE)
                .add_modifier(Modifier::BOLD),
        )))
        .title_top(
            Line::from(Span::styled(
                result_count_title,
                Style::default().fg(picker_colors::TEXT_MUTED),
            ))
            .alignment(Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(picker_colors::BORDER))
        .border_type(ratatui::widgets::BorderType::Rounded)
        .style(Style::default().bg(picker_colors::BG));

    frame.render_widget(block.clone(), picker_area);

    // Layout: query row spans full width, then separator, then body (results | preview)
    let inner_area = block.inner(picker_area);

    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1), // Query row (full width)
                Constraint::Length(1), // Separator
                Constraint::Min(1),    // Body (results + optional preview)
            ]
            .as_ref(),
        )
        .split(inner_area);

    let query_row = vertical_chunks[0];
    let separator_row = vertical_chunks[1];
    let body_area = vertical_chunks[2];

    render_picker_query(frame, picker, query_row);
    render_picker_separator(frame, separator_row);

    // Split body into results (left) and preview (right)
    let body_chunks = if show_preview {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
            .split(body_area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(body_area)
    };

    let results_area = body_chunks[0];
    // NLL: original `picker` ref is no longer used after the shadow below,
    // so the mutable borrow for prefetch is allowed.
    // Prefetch visible range (single nucleo snapshot instead of per-item)
    if let Some(picker_mut) = editor.picker_mut() {
        let selected_idx = picker_mut.selected_index();
        let total = picker_mut.filtered_result_count();
        let max_results = results_area.height as usize;
        let scroll_offset = if total <= max_results {
            0
        } else {
            let half = max_results / 2;
            if selected_idx < half {
                0
            } else if selected_idx + half >= total {
                total.saturating_sub(max_results)
            } else {
                selected_idx.saturating_sub(half)
            }
        };
        picker_mut.prefetch_visible_range(scroll_offset, max_results);
    }
    let picker = editor.picker().unwrap();
    let scroll_offset = render_picker_results(frame, picker, results_area);

    // Cache picker layout for mouse hit-testing
    let has_filter = picker.has_file_filter();
    let (query_field, filter_field) = if has_filter {
        let total_width = query_row.width as usize;
        let search_width = (total_width * 70 / 100).max(10) as u16;
        let sep_width = 1u16;
        let filter_x = query_row.x + search_width + sep_width;
        let filter_width = query_row.width.saturating_sub(search_width + sep_width);
        (
            ratatui::layout::Rect::new(query_row.x, query_row.y, search_width, 1),
            Some(ratatui::layout::Rect::new(
                filter_x,
                query_row.y,
                filter_width,
                1,
            )),
        )
    } else {
        (query_row, None)
    };

    // Get selected result (need to clone to release immutable borrow of picker)
    let selected_result = picker.selected_result().cloned();

    // Drop immutable borrow of picker before calling functions that need mutable borrow
    let _ = picker;

    // Store cached layout on editor
    editor.picker_state.last_layout = Some(crate::editor::PickerLayout {
        query_field: crate::key_convert::convert_ratatui_rect(query_field),
        filter_field: filter_field.map(crate::key_convert::convert_ratatui_rect),
        results_area: crate::key_convert::convert_ratatui_rect(results_area),
        results_scroll_offset: scroll_offset,
    });

    // Render preview panel if enabled
    if show_preview {
        if let Some(selected) = selected_result {
            render_picker_preview(frame, editor, &selected, body_chunks[1]);
        } else {
            // Render empty state when no selection
            render_picker_empty_state(frame, body_chunks[1]);
        }
    }
}

/// Renders the picker query line (single or dual field)
fn render_picker_query(frame: &mut Frame, picker: &crate::editor::Picker, area: Rect) {
    use crate::editor::PickerField;

    let query_text = picker.query();
    let has_filter = picker.has_file_filter();

    if !has_filter {
        // Single field mode — same as before
        let prompt_icon = " ";
        let query_line_width = area.width as usize;
        let content_len = 2 + UnicodeWidthStr::width(query_text);
        let padding = query_line_width.saturating_sub(content_len);

        let mut spans = vec![
            Span::styled(
                prompt_icon,
                Style::default()
                    .fg(picker_colors::GREEN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
            Span::styled(
                query_text.to_string(),
                Style::default()
                    .fg(picker_colors::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        if padding > 0 {
            spans.push(Span::styled(
                " ".repeat(padding),
                Style::default().bg(picker_colors::BG),
            ));
        }

        let query_line = Line::from(spans);
        let query_paragraph =
            Paragraph::new(query_line).style(Style::default().bg(picker_colors::BG));
        frame.render_widget(query_paragraph, area);
        return;
    }

    // Dual field mode: [search icon] query | [filter icon] file_filter
    let active = picker.active_field();
    let filter_text = picker.file_filter();
    let total_width = area.width as usize;

    // Split: ~70% for search, 1 char separator, rest for filter
    let search_width = (total_width * 70 / 100).max(10);
    let sep_width = 1;
    let filter_width = total_width.saturating_sub(search_width + sep_width);

    // Search field
    let search_active = active == PickerField::Query;
    let search_icon_color = if search_active {
        picker_colors::GREEN
    } else {
        picker_colors::TEXT_MUTED
    };
    let search_text_style = if search_active {
        Style::default()
            .fg(picker_colors::TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(picker_colors::TEXT_MUTED)
    };

    let search_icon = " ";
    let search_content_len = 2 + UnicodeWidthStr::width(query_text); // icon + space + text
    let search_padding = search_width.saturating_sub(search_content_len);

    let mut spans = vec![
        Span::styled(
            search_icon,
            Style::default()
                .fg(search_icon_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(query_text.to_string(), search_text_style),
    ];

    if search_padding > 0 {
        spans.push(Span::styled(
            " ".repeat(search_padding),
            Style::default().bg(picker_colors::BG),
        ));
    }

    // Separator
    spans.push(Span::styled(
        "\u{2502}",
        Style::default()
            .fg(picker_colors::SEPARATOR)
            .bg(picker_colors::BG),
    ));

    // File filter field
    let filter_active = active == PickerField::FileFilter;
    let filter_icon = " ";
    let filter_icon_color = if filter_active {
        picker_colors::GREEN
    } else {
        picker_colors::TEXT_MUTED
    };
    let filter_text_style = if filter_active {
        Style::default()
            .fg(picker_colors::TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(picker_colors::TEXT_MUTED)
    };

    spans.push(Span::styled(
        filter_icon,
        Style::default()
            .fg(filter_icon_color)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" ", Style::default()));

    if filter_text.is_empty() && !filter_active {
        // Show placeholder hint
        let hint = if cfg!(target_os = "macos") {
            "\u{2325}\u{2192} file filter"
        } else {
            "C-\u{2192} file filter"
        };
        let hint_len = 2 + UnicodeWidthStr::width(hint); // icon + space + hint
        let hint_padding = filter_width.saturating_sub(hint_len);
        spans.push(Span::styled(
            hint.to_string(),
            Style::default()
                .fg(picker_colors::TEXT_MUTED)
                .add_modifier(Modifier::ITALIC),
        ));
        if hint_padding > 0 {
            spans.push(Span::styled(
                " ".repeat(hint_padding),
                Style::default().bg(picker_colors::BG),
            ));
        }
    } else {
        let filter_content_len = 2 + UnicodeWidthStr::width(filter_text); // icon + space + text
        let filter_padding = filter_width.saturating_sub(filter_content_len);
        spans.push(Span::styled(filter_text.to_string(), filter_text_style));
        if filter_padding > 0 {
            spans.push(Span::styled(
                " ".repeat(filter_padding),
                Style::default().bg(picker_colors::BG),
            ));
        }
    }

    let query_line = Line::from(spans);
    let query_paragraph = Paragraph::new(query_line).style(Style::default().bg(picker_colors::BG));
    frame.render_widget(query_paragraph, area);
}

/// Renders the picker separator line
fn render_picker_separator(frame: &mut Frame, area: Rect) {
    let separator = "\u{2500}".repeat(area.width as usize);
    let separator_line = Line::from(Span::styled(
        separator,
        Style::default()
            .fg(picker_colors::SEPARATOR)
            .bg(picker_colors::BG),
    ));
    let separator_paragraph =
        Paragraph::new(separator_line).style(Style::default().bg(picker_colors::BG));
    frame.render_widget(separator_paragraph, area);
}

/// Builds spans for a display string with matched positions highlighted.
fn build_highlighted_spans(
    display: &str,
    match_positions: &[usize],
    text_color: Color,
    bg_color: Color,
    is_selected: bool,
) -> Vec<Span<'static>> {
    use std::collections::HashSet;
    let matched: HashSet<usize> = match_positions.iter().copied().collect();
    let chars: Vec<char> = display.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let is_match = matched.contains(&i);
        let start = i;
        while i < chars.len() && matched.contains(&i) == is_match {
            i += 1;
        }
        let segment: String = chars[start..i].iter().collect();
        let style = if is_match {
            Style::default()
                .fg(picker_colors::GREEN)
                .bg(bg_color)
                .add_modifier(Modifier::UNDERLINED)
                .add_modifier(if is_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                })
        } else {
            Style::default()
                .fg(text_color)
                .bg(bg_color)
                .add_modifier(if is_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                })
        };
        spans.push(Span::styled(segment, style));
    }

    spans
}

/// For a list of file paths, compute the shortest unambiguous display names.
/// Unique basenames stay as-is; duplicates get parent directories prepended until
/// distinguishable (e.g., "Dashboard/index.tsx" vs "Settings/index.tsx").
/// Results from the same file (identical paths) keep just the basename since
/// line numbers already distinguish them.
fn disambiguate_filenames(paths: &[&str]) -> Vec<String> {
    let n = paths.len();
    if n == 0 {
        return vec![];
    }

    // Split each path into reversed components: [filename, parent, grandparent, ...]
    let components: Vec<Vec<&str>> = paths
        .iter()
        .map(|p| p.trim_end_matches('/').rsplit('/').collect())
        .collect();

    // depth[i] = how many path components to show for path i (start with 1 = basename only)
    let mut depth = vec![1usize; n];

    loop {
        // Build display name for each path at its current depth
        let names: Vec<String> = (0..n)
            .map(|i| {
                let comps = &components[i];
                let d = depth[i].min(comps.len());
                let mut parts: Vec<&str> = comps[..d].to_vec();
                parts.reverse();
                parts.join("/")
            })
            .collect();

        // Find groups with identical display names
        let mut groups: std::collections::HashMap<&str, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, name) in names.iter().enumerate() {
            groups.entry(name.as_str()).or_default().push(i);
        }

        // Increase depth for groups that are ambiguous (same name, different files)
        let mut changed = false;
        for indices in groups.values() {
            if indices.len() <= 1 {
                continue;
            }
            // Skip if all entries point to the same file — line numbers distinguish them
            let first = paths[indices[0]];
            if indices.iter().all(|&i| paths[i] == first) {
                continue;
            }
            for &i in indices {
                if depth[i] < components[i].len() {
                    depth[i] += 1;
                    changed = true;
                }
            }
        }

        if !changed {
            return names;
        }
    }
}

/// Renders the picker results list
/// Returns the scroll offset used for rendering (needed for mouse hit-testing).
fn render_picker_results(frame: &mut Frame, picker: &crate::editor::Picker, area: Rect) -> usize {
    let selected_idx = picker.selected_index();
    let max_results = area.height as usize;
    let result_width = area.width as usize;
    let total = picker.filtered_result_count();
    let query = picker.query();

    // Center-scroll: keep selected item in the middle of visible area
    let scroll_offset = if total <= max_results {
        0
    } else {
        let half = max_results / 2;
        if selected_idx < half {
            0
        } else if selected_idx + half >= total {
            total.saturating_sub(max_results)
        } else {
            selected_idx.saturating_sub(half)
        }
    };

    let is_live_grep = matches!(picker.mode(), crate::editor::PickerMode::LiveGrep);

    // Collect visible results first so we can disambiguate filenames across them
    let visible_entries: Vec<(usize, &crate::editor::PickerResult)> = (scroll_offset
        ..total.min(scroll_offset + max_results))
        .filter_map(|i| picker.filtered_result(i).map(|result| (i, result)))
        .collect();

    // For live grep: compute shortest unambiguous display names for duplicate filenames
    let disambiguated = if is_live_grep {
        let paths: Vec<&str> = visible_entries
            .iter()
            .map(|(_, r)| r.location.as_str())
            .collect();
        disambiguate_filenames(&paths)
    } else {
        Vec::new()
    };

    let visible_results: Vec<Line> = visible_entries
        .iter()
        .enumerate()
        .map(|(vis_idx, &(actual_idx, result))| {
            let is_selected = actual_idx == selected_idx;

            let max_display_len = result_width.saturating_sub(5);
            let display = crate::editor::Picker::truncate_path(&result.display, max_display_len);

            let icon = if result.line > 0 {
                "\u{f002}"
            } else if display.ends_with('/') {
                "\u{f024b}"
            } else {
                "\u{f15b}"
            };

            let (icon_color, text_color, bg_color) = if is_selected {
                (
                    picker_colors::GREEN,
                    picker_colors::TEXT_BRIGHT,
                    picker_colors::SELECTED,
                )
            } else {
                (
                    picker_colors::TEXT_MUTED,
                    picker_colors::TEXT,
                    picker_colors::BG,
                )
            };

            let icon_style =
                Style::default()
                    .fg(icon_color)
                    .bg(bg_color)
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    });

            let prefix = if is_selected { " \u{25b8} " } else { "   " };

            let mut spans = vec![
                Span::styled(icon.to_string(), icon_style),
                Span::styled(
                    prefix.to_string(),
                    Style::default()
                        .fg(text_color)
                        .bg(bg_color)
                        .add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ];

            if is_live_grep {
                if let Some(content) = &result.content {
                    // Grep result layout: filename:line  content
                    // - filename in blue (prominent anchor)
                    // - :line in muted (navigation metadata)
                    // - content in normal text with green match highlights
                    let bold = if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    };

                    // Use disambiguated name (adds parent dirs for duplicate basenames)
                    let display_name = &disambiguated[vis_idx];
                    let line_num = format!(":{}", result.line + 1);

                    let filename_style = Style::default()
                        .fg(picker_colors::FILENAME)
                        .bg(bg_color)
                        .add_modifier(bold);
                    let linenum_style = Style::default()
                        .fg(picker_colors::TEXT_MUTED)
                        .bg(bg_color)
                        .add_modifier(bold);

                    spans.push(Span::styled(display_name.clone(), filename_style));
                    spans.push(Span::styled(line_num.clone(), linenum_style));
                    spans.push(Span::styled("  ", Style::default().bg(bg_color)));

                    // Truncate content to fit remaining width
                    let location_len = UnicodeWidthStr::width(display_name.as_str())
                        + UnicodeWidthStr::width(line_num.as_str());
                    let used = UnicodeWidthStr::width(icon)
                        + UnicodeWidthStr::width(prefix)
                        + location_len
                        + 2;
                    let content_max = result_width.saturating_sub(used);
                    let truncated_content = truncate_to_width(content, content_max);

                    // Match highlights on the content (user searches content, not filenames)
                    let positions = rematch_positions(query, &truncated_content);
                    spans.extend(build_highlighted_spans(
                        &truncated_content,
                        &positions,
                        text_color,
                        bg_color,
                        is_selected,
                    ));

                    let total_len = used + UnicodeWidthStr::width(truncated_content.as_str());
                    let padding = result_width.saturating_sub(total_len);
                    if padding > 0 {
                        spans.push(Span::styled(
                            " ".repeat(padding),
                            Style::default().bg(bg_color),
                        ));
                    }
                } else {
                    // Fallback: no content field, render display as-is
                    let positions = rematch_positions(query, &display);
                    spans.extend(build_highlighted_spans(
                        &display,
                        &positions,
                        text_color,
                        bg_color,
                        is_selected,
                    ));
                    let content_len = UnicodeWidthStr::width(icon)
                        + UnicodeWidthStr::width(prefix)
                        + UnicodeWidthStr::width(display.as_str());
                    let padding = result_width.saturating_sub(content_len);
                    if padding > 0 {
                        spans.push(Span::styled(
                            " ".repeat(padding),
                            Style::default().bg(bg_color),
                        ));
                    }
                }
            } else {
                // Standard display with fuzzy match highlighting
                let positions = rematch_positions(query, &display);
                spans.extend(build_highlighted_spans(
                    &display,
                    &positions,
                    text_color,
                    bg_color,
                    is_selected,
                ));

                let content_len = UnicodeWidthStr::width(icon)
                    + UnicodeWidthStr::width(prefix)
                    + UnicodeWidthStr::width(display.as_str());
                let padding = result_width.saturating_sub(content_len);
                if padding > 0 {
                    spans.push(Span::styled(
                        " ".repeat(padding),
                        Style::default().bg(bg_color),
                    ));
                }
            }

            Line::from(spans)
        })
        .collect();

    let mut all_lines = visible_results;

    if total == 0 {
        let text = "  \u{f0349} No matches found";
        let padding = result_width.saturating_sub(UnicodeWidthStr::width(text));
        all_lines.push(Line::from(vec![
            Span::styled(
                text,
                Style::default()
                    .fg(Color::Rgb(240, 120, 120))
                    .bg(picker_colors::BG),
            ),
            Span::styled(" ".repeat(padding), Style::default().bg(picker_colors::BG)),
        ]));
    }
    // Result count is shown in the title bar — no footer needed

    // Fill remaining lines with background
    let lines_to_fill = max_results.saturating_sub(all_lines.len());
    for _ in 0..lines_to_fill {
        all_lines.push(Line::from(vec![Span::styled(
            " ".repeat(result_width),
            Style::default().bg(picker_colors::BG),
        )]));
    }

    let results_paragraph = Paragraph::new(all_lines).style(Style::default().bg(picker_colors::BG));
    frame.render_widget(results_paragraph, area);

    scroll_offset
}

/// Renders the file preview for the picker
fn render_picker_preview(
    frame: &mut Frame,
    editor: &mut crate::editor::Editor,
    result: &crate::editor::PickerResult,
    area: Rect,
) {
    let preview_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(picker_colors::SEPARATOR))
        .style(Style::default().bg(picker_colors::BG_ALT));

    let inner_area = preview_block.inner(area);
    frame.render_widget(preview_block, area);

    frame.render_widget(Fill::bg(picker_colors::BG_ALT), inner_area);

    // Try to get preview with fallback - show stale preview while new one loads
    // This eliminates the jarring "Loading..." flash when navigating quickly
    let file_path = &result.location;
    let (preview, _is_stale) = match editor.get_preview_with_fallback(file_path) {
        Some((p, is_stale)) => (p, is_stale),
        None => {
            // No preview available at all (first time opening picker)
            let loading_msg = " \u{f0996}  Loading preview...";
            let paragraph = Paragraph::new(loading_msg)
                .style(
                    Style::default()
                        .fg(picker_colors::TEXT_MUTED)
                        .bg(picker_colors::BG_ALT)
                        .add_modifier(Modifier::ITALIC),
                )
                .alignment(Alignment::Center);

            // Center vertically
            let centered_area = Rect {
                x: inner_area.x,
                y: inner_area.y + inner_area.height / 2,
                width: inner_area.width,
                height: 1,
            };
            frame.render_widget(paragraph, centered_area);
            return;
        }
    };

    let theme = crate::syntax::Theme::default();
    let mut lines_to_render = Vec::new();

    let max_lines = inner_area.height as usize;
    let total_lines = preview.content.lines().count();

    // Calculate which lines to show
    let (start_line, end_line) = if result.line > 0 && result.line < total_lines {
        // For LiveGrep results, center around the matched line
        let context = max_lines / 2;
        let start = result.line.saturating_sub(context);
        let end = (result.line + context).min(total_lines);
        (start, end)
    } else {
        // For file finder, show from the top
        (0, max_lines.min(total_lines))
    };

    // Skip expensive syntax highlighting during rapid scrolling for responsive feel.
    // Plain text rendering is ~10x faster than syntax-highlighted rendering.
    let use_syntax = preview.language.is_some() && !editor.is_picker_scrolling_rapidly();

    if use_syntax {
        // Use syntax highlighting (only when not scrolling rapidly)
        match crate::syntax::SyntaxHighlighter::new(preview.language.unwrap()) {
            Ok(mut highlighter) => {
                render_preview_with_syntax(
                    frame,
                    &mut highlighter,
                    preview,
                    result,
                    &theme,
                    inner_area,
                    start_line,
                    end_line,
                    total_lines,
                    &mut lines_to_render,
                );
            }
            Err(_) => {
                // Fall back to plain text
                render_plain_preview(&preview.content, result, inner_area, &mut lines_to_render);
            }
        }
    } else {
        // Plain text preview (fast path for rapid scrolling or unsupported languages)
        render_plain_preview(&preview.content, result, inner_area, &mut lines_to_render);
    }

    let paragraph =
        Paragraph::new(lines_to_render).style(Style::default().bg(picker_colors::BG_ALT));
    frame.render_widget(paragraph, inner_area);
}

/// Renders preview with syntax highlighting
#[allow(clippy::too_many_arguments)]
fn render_preview_with_syntax(
    _frame: &mut Frame,
    highlighter: &mut crate::syntax::SyntaxHighlighter,
    preview: &crate::editor::PreviewCache,
    result: &crate::editor::PickerResult,
    theme: &crate::syntax::Theme,
    area: Rect,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    lines_to_render: &mut Vec<Line<'static>>,
) {
    // Parse only once if not already parsed
    let mut need_parsing = false;

    // Check if we need to parse (if any line in our range is not cached)
    {
        let cache = preview.highlighted_lines.borrow();
        for line_idx in start_line..end_line {
            if !cache.contains_key(&line_idx) {
                need_parsing = true;
                break;
            }
        }
    }

    // Parse if needed
    if need_parsing {
        highlighter.parse(&preview.content);

        // Cache highlights for the visible range
        let mut cache = preview.highlighted_lines.borrow_mut();
        for line_idx in start_line..end_line {
            cache
                .entry(line_idx)
                .or_insert_with(|| highlighter.highlights_for_line(line_idx, &preview.content));
        }
    }

    for (line_idx, line_text) in preview.content.lines().enumerate() {
        if line_idx < start_line {
            continue;
        }
        if line_idx >= end_line {
            break;
        }

        // Expand tabs in preview content and get byte mapping
        let super::helpers::ExpandedLine {
            text: line_text,
            byte_mapping: tab_mapping,
            ..
        } = expand_tabs_with_mapping(line_text, 4); // Use default tab width for previews

        // Truncate line to fit width (line number prefix is 7 chars: "  1 | ")
        let content_width = area.width.saturating_sub(7) as usize;
        let line_text = truncate_to_width(&line_text, content_width);

        // Get highlights from cache and remap for expanded tabs
        let original_highlights = preview
            .highlighted_lines
            .borrow()
            .get(&line_idx)
            .cloned()
            .unwrap_or_default();
        let highlights = remap_highlights(&original_highlights, &tab_mapping);
        let is_target_line =
            result.line > 0 && result.line < total_lines && line_idx == result.line;

        // Build the line with syntax highlighting
        let mut spans = Vec::new();

        // Add line number prefix
        let line_num = format!("{:>4} \u{2502} ", line_idx + 1);
        let line_num_style = if is_target_line {
            Style::default()
                .fg(picker_colors::GREEN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(picker_colors::TEXT_MUTED)
        };
        spans.push(Span::styled(line_num, line_num_style));

        // Add syntax-highlighted content
        let chars: Vec<char> = line_text.chars().collect();

        // Build a map from character index to byte index
        let mut byte_indices: Vec<usize> = Vec::with_capacity(chars.len() + 1);
        byte_indices.push(0);
        for (byte_idx, _) in line_text.char_indices().skip(1) {
            byte_indices.push(byte_idx);
        }
        byte_indices.push(line_text.len());

        let mut col_idx = 0;

        while col_idx < chars.len() {
            // Find the syntax group for this character (convert to byte index)
            // Uses O(log n) binary search instead of O(n) linear search
            let byte_idx = byte_indices[col_idx];
            let syntax_group = find_highlight_at_byte(&highlights, byte_idx);

            // Find the end of this styled region
            let mut end_col = col_idx + 1;
            while end_col < chars.len() {
                let next_byte_idx = byte_indices[end_col];
                let next_group = find_highlight_at_byte(&highlights, next_byte_idx);

                if next_group != syntax_group {
                    break;
                }
                end_col += 1;
            }

            let text: String = chars[col_idx..end_col].iter().collect();
            let mut style = if let Some(group) = syntax_group {
                let color = crate::key_convert::convert_core_color(theme.get_color(group));
                Style::default().fg(color)
            } else {
                Style::default().fg(Color::White)
            };

            if is_target_line {
                style = style.bg(picker_colors::SELECTED);
            }

            spans.push(Span::styled(text, style));
            col_idx = end_col;
        }

        lines_to_render.push(Line::from(spans));
    }
}

/// Renders plain text preview without syntax highlighting
fn render_plain_preview(
    content: &str,
    result: &crate::editor::PickerResult,
    area: Rect,
    lines: &mut Vec<Line<'static>>,
) {
    let max_lines = area.height as usize;
    let total_lines = content.lines().count();

    // Calculate which lines to show
    let (start_line, end_line) = if result.line > 0 && result.line < total_lines {
        let context = max_lines / 2;
        let start = result.line.saturating_sub(context);
        let end = (result.line + context).min(total_lines);
        (start, end)
    } else {
        (0, max_lines.min(total_lines))
    };

    for (line_idx, line_text) in content.lines().enumerate() {
        if line_idx < start_line {
            continue;
        }
        if line_idx >= end_line {
            break;
        }

        // Expand tabs in plain preview
        let line_text = expand_tabs(line_text, 4);

        // Truncate line to fit width (line number prefix is 7 chars: "  1 | ")
        let content_width = area.width.saturating_sub(7) as usize;
        let line_text = truncate_to_width(&line_text, content_width);

        let is_target_line =
            result.line > 0 && result.line < total_lines && line_idx == result.line;

        let line_num = format!("{:>4} \u{2502} ", line_idx + 1);
        let line_num_style = if is_target_line {
            Style::default()
                .fg(picker_colors::GREEN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(picker_colors::TEXT_MUTED)
        };

        let text_style = if is_target_line {
            Style::default()
                .fg(Color::White)
                .bg(picker_colors::SELECTED)
        } else {
            Style::default().fg(picker_colors::TEXT)
        };

        lines.push(Line::from(vec![
            Span::styled(line_num, line_num_style),
            Span::styled(line_text.to_string(), text_style),
        ]));
    }
}

/// Renders empty state for the picker preview panel
fn render_picker_empty_state(frame: &mut Frame, area: Rect) {
    let preview_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(picker_colors::SEPARATOR))
        .style(Style::default().bg(picker_colors::BG_ALT));

    let inner_area = preview_block.inner(area);
    frame.render_widget(preview_block, area);

    frame.render_widget(Fill::bg(picker_colors::BG_ALT), inner_area);

    let empty_msg = " \u{f0208}  No file selected";
    let paragraph = Paragraph::new(empty_msg)
        .style(
            Style::default()
                .fg(picker_colors::TEXT_MUTED)
                .bg(picker_colors::BG_ALT)
                .add_modifier(Modifier::ITALIC),
        )
        .alignment(Alignment::Center);

    // Center vertically
    let centered_area = Rect {
        x: inner_area.x,
        y: inner_area.y + inner_area.height / 2,
        width: inner_area.width,
        height: 1,
    };
    frame.render_widget(paragraph, centered_area);
}
