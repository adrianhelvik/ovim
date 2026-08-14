use crate::editor::{AiRegionStatus, Editor};
use crate::syntax::{Theme, UiGroup};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::display::char_display_width;
use crate::ui::renderer::markdown_conceal::scan_markdown_conceal;

use super::helpers::{compose_conceal_and_tabs, expand_tabs_with_mapping, remap_char_col};
use super::layout::{BufferLayout, GUTTER_SPACING, SIGN_WIDTH};
use super::styles::{
    blame_color_for_hash, get_diagnostic_sign_style, get_git_sign_style, get_line_number_style,
    remap_highlights,
};
use crate::syntax::HighlightGroup;
use ovim_core::buffer::Cursor;
use std::ops::Range;

/// Window-specific rendering context for multi-window support.
/// When provided, these values override the editor's focused window state.
#[derive(Default)]
pub struct WindowRenderContext {
    /// Override cursor position (for non-focused windows)
    pub cursor: Option<Cursor>,
    /// Override scroll offset (for non-focused windows)
    pub scroll_offset: Option<usize>,
    /// Override visual sub-row offset within the top line (for non-focused windows)
    pub scroll_subrow: Option<usize>,
    /// Override horizontal scroll offset (for non-focused windows)
    pub horizontal_offset: Option<usize>,
    /// Use this window's own soft-wrap map (built at *this* window's content
    /// width) instead of the editor-global one. `None` ⇒ use `editor.wrap_map()`
    /// (single-window / focused-window path). (roadmap 19 / OV-00209)
    pub wrap_map_window_index: Option<usize>,
}

/// Converts an expanded char index to a display column.
fn expanded_char_to_display_col(text: &str, char_idx: usize) -> usize {
    let mut display_col = 0;
    for (i, ch) in text.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        display_col += char_display_width(ch);
    }
    display_col
}

/// Converts a display column to a char index within a string.
/// If the display column falls in the middle of a wide char, returns that char's index.
fn display_col_to_char_idx(text: &str, target_display_col: usize) -> usize {
    let mut display_col = 0;
    for (char_idx, ch) in text.chars().enumerate() {
        if display_col >= target_display_col {
            return char_idx;
        }
        display_col += char_display_width(ch);
        if display_col > target_display_col {
            return char_idx;
        }
    }
    text.chars().count()
}

/// Converts a UTF-16 offset to a char index within a line of text.
/// LSP uses UTF-16 offsets for character positions.
fn utf16_offset_to_char_idx(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (char_idx, ch) in text.chars().enumerate() {
        if utf16_count >= utf16_offset {
            return char_idx;
        }
        utf16_count += ch.len_utf16();
    }
    text.chars().count()
}

/// Slices a line for horizontal viewport with visual indicators.
/// h_offset and width are in display columns.
/// Returns (sliced_text, precedes_indicator, extends_indicator)
fn slice_horizontal_viewport(line: &str, h_offset: usize, width: usize) -> (String, bool, bool) {
    // Safety check: if width is 0 or too small, return empty or minimal content
    if width == 0 {
        return (String::new(), false, false);
    }

    // Calculate total display width of the line
    let total_display_width: usize = line.chars().map(char_display_width).sum();

    // Line fits entirely in viewport
    if total_display_width <= width {
        return (line.to_string(), false, false);
    }

    let precedes = h_offset > 0;
    let extends = h_offset + width < total_display_width;

    // Available width for actual content (reserve space for indicators)
    let indicator_cols = (if precedes { 1 } else { 0 }) + (if extends { 1 } else { 0 });
    let content_width = width.saturating_sub(indicator_cols);

    let mut result = String::new();

    // Add precedes indicator (<) if scrolled right
    if precedes {
        result.push('<');
    }

    // Walk chars to find the start position (skip h_offset display columns)
    let mut display_col = 0;
    let mut chars = line.chars().peekable();

    // Skip characters until we reach h_offset
    while let Some(&ch) = chars.peek() {
        let ch_width = char_display_width(ch);
        if display_col + ch_width > h_offset {
            break;
        }
        display_col += ch_width;
        chars.next();
    }

    // Collect characters that fit within content_width display columns
    let mut content_display_width = 0;
    while let Some(&ch) = chars.peek() {
        let ch_width = char_display_width(ch);
        if content_display_width + ch_width > content_width {
            break;
        }
        result.push(ch);
        content_display_width += ch_width;
        chars.next();
    }

    // Pad if a wide char didn't fit exactly
    while content_display_width < content_width {
        result.push(' ');
        content_display_width += 1;
    }

    // Add extends indicator (>) if content continues right
    if extends {
        result.push('>');
    }

    (result, precedes, extends)
}

/// Reusable scratch buffers for `shift_highlights_for_viewport` to avoid
/// allocating two `Vec<usize>` per visible line per frame.
struct HighlightShiftBuffers {
    byte_to_display: Vec<usize>,
    display_to_byte: Vec<usize>,
}

impl HighlightShiftBuffers {
    fn new() -> Self {
        Self {
            byte_to_display: Vec::with_capacity(256),
            display_to_byte: Vec::with_capacity(256),
        }
    }
}

/// Shifts syntax highlight ranges for horizontal viewport.
/// Highlights are in expanded byte ranges; h_offset and width are in display columns.
/// Returns byte ranges into the sliced text.
///
/// `buffers` provides reusable scratch space — the caller keeps one instance
/// across all lines in the render pass, eliminating per-line allocation.
fn shift_highlights_for_viewport(
    highlights: &[(Range<usize>, HighlightGroup)],
    expanded_text: &str,
    sliced_text: &str,
    h_offset: usize,
    width: usize,
    precedes: bool,
    buffers: &mut HighlightShiftBuffers,
) -> Vec<(Range<usize>, HighlightGroup)> {
    let offset_adjustment = if precedes { 1 } else { 0 }; // Account for '<' indicator

    // Build a byte-offset-to-display-column mapping for the expanded text
    let byte_to_display = &mut buffers.byte_to_display;
    byte_to_display.clear();
    {
        let mut display_col = 0;
        for (byte_idx, ch) in expanded_text.char_indices() {
            while byte_to_display.len() <= byte_idx {
                byte_to_display.push(display_col);
            }
            display_col += char_display_width(ch);
        }
        while byte_to_display.len() <= expanded_text.len() {
            byte_to_display.push(display_col);
        }
    }

    // Build display-column-to-byte-offset mapping for the sliced text
    let sliced_display_to_byte = &mut buffers.display_to_byte;
    sliced_display_to_byte.clear();
    {
        for (byte_idx, ch) in sliced_text.char_indices() {
            let ch_width = char_display_width(ch);
            for _ in 0..ch_width {
                sliced_display_to_byte.push(byte_idx);
            }
        }
        sliced_display_to_byte.push(sliced_text.len()); // sentinel
    }

    let viewport_end = h_offset + width;

    highlights
        .iter()
        .filter_map(|(range, group)| {
            let start_display = if range.start < byte_to_display.len() {
                byte_to_display[range.start]
            } else {
                *byte_to_display.last().unwrap_or(&0)
            };
            let end_display = if range.end < byte_to_display.len() {
                byte_to_display[range.end]
            } else {
                *byte_to_display.last().unwrap_or(&0)
            };

            // Highlight is completely before viewport
            if end_display <= h_offset {
                return None;
            }
            // Highlight is completely after viewport
            if start_display >= viewport_end {
                return None;
            }

            // Clip to viewport display columns
            let clipped_start = start_display.saturating_sub(h_offset) + offset_adjustment;
            let clipped_end = end_display.saturating_sub(h_offset).min(width) + offset_adjustment;

            // Convert viewport display columns to byte offsets in sliced text
            let byte_start = if clipped_start < sliced_display_to_byte.len() {
                sliced_display_to_byte[clipped_start]
            } else {
                sliced_text.len()
            };
            let byte_end = if clipped_end < sliced_display_to_byte.len() {
                sliced_display_to_byte[clipped_end]
            } else {
                sliced_text.len()
            };

            if byte_start < byte_end {
                Some((byte_start..byte_end, *group))
            } else {
                None
            }
        })
        .collect()
}

/// Apply a style to a specific column in a line, splitting spans as needed.
fn apply_style_at_column(line: &mut Line<'static>, target_col: usize, style: Style) {
    let mut current_col = 0;
    for i in 0..line.spans.len() {
        let span_len = line.spans[i].content.chars().count();
        if target_col >= current_col && target_col < current_col + span_len {
            let offset = target_col - current_col;
            if offset == 0 && span_len == 1 {
                // Span is exactly the target character
                line.spans[i].style = line.spans[i].style.patch(style);
            } else {
                // Split the span into up to 3 parts: before, target char, after
                let chars: Vec<char> = line.spans[i].content.chars().collect();
                let base_style = line.spans[i].style;
                let mut new_spans = Vec::with_capacity(3);

                if offset > 0 {
                    let before: String = chars[..offset].iter().collect();
                    new_spans.push(Span::styled(before, base_style));
                }

                let target: String = chars[offset..=offset].iter().collect();
                new_spans.push(Span::styled(target, base_style.patch(style)));

                if offset + 1 < chars.len() {
                    let after: String = chars[offset + 1..].iter().collect();
                    new_spans.push(Span::styled(after, base_style));
                }

                // Replace the original span with the split parts
                line.spans.splice(i..=i, new_spans);
            }
            return;
        }
        current_col += span_len;
    }
}

/// Apply a background color to a column range within a Line.
/// Splits spans as needed to cover exactly the given range.
fn apply_bg_to_column_range(line: &mut Line<'static>, start_col: usize, end_col: usize, bg: Color) {
    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut current_col = 0;

    for span in line.spans.drain(..) {
        let span_len = span.content.chars().count();
        let span_end = current_col + span_len;

        if span_end <= start_col || current_col > end_col {
            // Entirely outside the flash range
            new_spans.push(span);
        } else if current_col >= start_col && span_end <= end_col + 1 {
            // Entirely inside the flash range
            new_spans.push(Span::styled(span.content, span.style.bg(bg)));
        } else {
            // Partially overlapping - split the span
            let chars: Vec<char> = span.content.chars().collect();
            let flash_start = start_col.saturating_sub(current_col);
            let flash_end = (end_col + 1).saturating_sub(current_col).min(chars.len());

            if flash_start > 0 {
                let before: String = chars[..flash_start].iter().collect();
                new_spans.push(Span::styled(before, span.style));
            }
            if flash_start < flash_end {
                let middle: String = chars[flash_start..flash_end].iter().collect();
                new_spans.push(Span::styled(middle, span.style.bg(bg)));
            }
            if flash_end < chars.len() {
                let after: String = chars[flash_end..].iter().collect();
                new_spans.push(Span::styled(after, span.style));
            }
        }

        current_col = span_end;
    }

    line.spans = new_spans;
}

/// Apply a foreground color and modifier to a column range of a rendered line.
fn apply_fg_modifier_to_column_range(
    line: &mut Line<'static>,
    start_col: usize,
    end_col_exclusive: usize,
    fg: Color,
    modifier: Modifier,
) {
    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut current_col = 0;

    for span in line.spans.drain(..) {
        let span_len = span.content.chars().count();
        let span_end = current_col + span_len;

        if span_end <= start_col || current_col >= end_col_exclusive {
            new_spans.push(span);
        } else if current_col >= start_col && span_end <= end_col_exclusive {
            new_spans.push(Span::styled(
                span.content,
                span.style.fg(fg).add_modifier(modifier),
            ));
        } else {
            let chars: Vec<char> = span.content.chars().collect();
            let range_start = start_col.saturating_sub(current_col);
            let range_end = end_col_exclusive
                .saturating_sub(current_col)
                .min(chars.len());

            if range_start > 0 {
                let before: String = chars[..range_start].iter().collect();
                new_spans.push(Span::styled(before, span.style));
            }
            if range_start < range_end {
                let middle: String = chars[range_start..range_end].iter().collect();
                new_spans.push(Span::styled(
                    middle,
                    span.style.fg(fg).add_modifier(modifier),
                ));
            }
            if range_end < chars.len() {
                let after: String = chars[range_end..].iter().collect();
                new_spans.push(Span::styled(after, span.style));
            }
        }

        current_col = span_end;
    }

    line.spans = new_spans;
}

/// Find matching bracket position if cursor is on a bracket
fn find_matching_bracket_position(buffer: &crate::buffer::Buffer) -> Option<(usize, usize)> {
    let cursor = buffer.cursor();
    let rope = buffer.rope();
    let line_idx = cursor.line();

    if line_idx >= rope.len_lines() {
        return None;
    }

    let line = rope.line(line_idx);
    let col = cursor.col().0;

    if col >= line.len_chars() {
        return None;
    }

    let current_char = line.char(col);

    // Check if on a bracket
    let (matching_char, search_forward) = match current_char {
        '(' => (')', true),
        ')' => ('(', false),
        '[' => (']', true),
        ']' => ('[', false),
        '{' => ('}', true),
        '}' => ('{', false),
        '<' => ('>', true),
        '>' => ('<', false),
        _ => return None,
    };

    // Calculate absolute position
    let abs_pos = rope.line_to_char(line_idx) + col;
    let total_chars = rope.len_chars();

    // Search for matching bracket
    let mut depth = 1;
    let mut pos = abs_pos;

    if search_forward {
        pos += 1;
        while pos < total_chars && depth > 0 {
            let c = rope.char(pos);
            if c == current_char {
                depth += 1;
            } else if c == matching_char {
                depth -= 1;
            }
            if depth > 0 {
                pos += 1;
            }
        }
    } else {
        if pos == 0 {
            return None;
        }
        pos -= 1;
        while depth > 0 {
            let c = rope.char(pos);
            if c == current_char {
                depth += 1;
            } else if c == matching_char {
                depth -= 1;
            }
            if depth > 0 {
                if pos == 0 {
                    return None;
                }
                pos -= 1;
            }
        }
    }

    if depth == 0 {
        // Convert absolute position to line/col
        let match_line = rope.char_to_line(pos);
        let line_start = rope.line_to_char(match_line);
        let match_col = pos - line_start;
        Some((match_line, match_col))
    } else {
        None
    }
}

/// Bracket character for blame grouping
#[derive(Debug, Clone, Copy, PartialEq)]
enum BlameBracket {
    /// Single-line commit (no bracket)
    None,
    /// First line of a multi-line group
    Top,
    /// Middle line of a multi-line group
    Mid,
    /// Last line of a multi-line group
    Bottom,
}

/// Pre-computes blame bracket characters for visible lines.
/// Returns a vec of (bracket, hash, author, color) for each line in the range.
fn compute_blame_brackets(
    blame: &crate::GitBlame,
    start_line: usize,
    end_line: usize,
    author_width: usize,
) -> Vec<(BlameBracket, String, String, Color)> {
    let mut result = Vec::with_capacity(end_line.saturating_sub(start_line));

    for line_idx in start_line..end_line {
        if let Some(info) = blame.get(line_idx) {
            let hash = &info.commit_hash;
            let color = blame_color_for_hash(hash);

            // Check if prev/next lines have the same commit
            let same_as_prev = line_idx > 0
                && blame
                    .get(line_idx - 1)
                    .map(|p| p.commit_hash == *hash)
                    .unwrap_or(false);
            let same_as_next = blame
                .get(line_idx + 1)
                .map(|n| n.commit_hash == *hash)
                .unwrap_or(false);

            let bracket = match (same_as_prev, same_as_next) {
                (false, false) => BlameBracket::None,
                (false, true) => BlameBracket::Top,
                (true, true) => BlameBracket::Mid,
                (true, false) => BlameBracket::Bottom,
            };

            // Truncate author to fit
            let author: String = info.author.chars().take(author_width).collect();

            result.push((bracket, hash.clone(), author, color));
        } else {
            result.push((
                BlameBracket::None,
                String::new(),
                String::new(),
                Color::DarkGray,
            ));
        }
    }

    result
}

/// Builds a gutter line for a logical line (line number + git sign / diagnostic sign).
/// If `is_continuation` is true, produces a blank gutter row.
/// Diagnostic signs take priority over git signs when both are present.
/// Invariant context for gutter rendering within a single render pass.
/// Constructed once before the line loop and passed to all `build_gutter_line` calls.
struct GutterContext<'a> {
    editor: &'a Editor,
    buffer: &'a crate::buffer::Buffer,
    theme: &'a Theme,
    line_num_width: usize,
    cursor_line: usize,
    blame_width: usize,
    walkthrough_range: Option<(usize, usize)>,
}

const WALKTHROUGH_SELECTION_BG: Color = Color::Rgb(34, 57, 76);
const WALKTHROUGH_GUTTER_FG: Color = Color::Rgb(96, 176, 255);

fn line_is_in_walkthrough(range: Option<(usize, usize)>, line_idx: usize) -> bool {
    range.is_some_and(|(start, end)| line_idx >= start && line_idx <= end)
}

fn build_gutter_line(
    ctx: &GutterContext,
    line_idx: usize,
    is_continuation: bool,
    line_diagnostics: &[lsp_types::Diagnostic],
    blame_info: Option<&(BlameBracket, String, String, Color)>,
) -> Line<'static> {
    let editor = ctx.editor;
    let buffer = ctx.buffer;
    let theme = ctx.theme;
    let line_num_width = ctx.line_num_width;
    let cursor_line = ctx.cursor_line;
    let blame_width = ctx.blame_width;

    if is_continuation {
        // Blank gutter for wrap continuation rows
        let width = blame_width + SIGN_WIDTH + line_num_width + GUTTER_SPACING;
        return Line::from(" ".repeat(width));
    }

    let mut spans = Vec::new();

    // Blame column (if active)
    if blame_width > 0 {
        if let Some((bracket, hash, author, color)) = blame_info {
            let bracket_ch = match bracket {
                BlameBracket::None => ' ',
                BlameBracket::Top => '╭',
                BlameBracket::Mid => '│',
                BlameBracket::Bottom => '╰',
            };

            // Show hash+author only on first line of group or single lines
            let show_info = *bracket == BlameBracket::None || *bracket == BlameBracket::Top;
            let content_width = blame_width - 2; // minus bracket + leading space

            let text = if show_info && !hash.is_empty() {
                let info_str = format!("{} {}", hash, author);
                format!(
                    "{} {:content_width$}",
                    bracket_ch,
                    info_str,
                    content_width = content_width
                )
            } else {
                format!(
                    "{} {:content_width$}",
                    bracket_ch,
                    "",
                    content_width = content_width
                )
            };

            // Truncate to blame_width
            let text: String = text.chars().take(blame_width).collect();
            spans.push(Span::styled(text, Style::default().fg(*color)));
        } else {
            spans.push(Span::raw(" ".repeat(blame_width)));
        }
    }

    let line_num_text = if editor.options.relative_number {
        let rel = if line_idx == cursor_line {
            line_idx + 1
        } else {
            line_idx.abs_diff(cursor_line)
        };
        format!("{:>width$} ", rel, width = line_num_width)
    } else if editor.options.number {
        format!("{:>width$} ", line_idx + 1, width = line_num_width)
    } else {
        "  ".to_string()
    };

    // Sign priority: breakpoint+exec > breakpoint > execution line > diagnostics >
    // walkthrough focus > agent edits > git. The walkthrough marker makes the
    // explained block visible even on blank or very short lines.
    let line_1based = (line_idx + 1) as u64;
    let has_breakpoint = editor.has_breakpoint_at(line_1based);
    let is_exec_line = editor
        .execution_position()
        .is_some_and(|(_, exec_line)| exec_line == line_1based);

    let buffer_id = buffer.id();
    let is_agent_edit = editor
        .ai_chat_state()
        .map(|c| c.agent_edits.is_line_modified(buffer_id, line_idx))
        .unwrap_or(false);
    let is_walkthrough_line = line_is_in_walkthrough(ctx.walkthrough_range, line_idx);

    let (sign_text, sign_color) = if has_breakpoint && is_exec_line {
        ("●▶", Color::Red)
    } else if has_breakpoint {
        ("● ", Color::Red)
    } else if is_exec_line {
        ("▶ ", Color::Yellow)
    } else if !line_diagnostics.is_empty() {
        let severity = line_diagnostics[0].severity;
        get_diagnostic_sign_style(severity)
    } else if is_walkthrough_line {
        ("▎ ", WALKTHROUGH_GUTTER_FG)
    } else if is_agent_edit {
        ("▎ ", Color::Rgb(82, 139, 255))
    } else {
        let git_status = buffer.git_status().get_line_status(line_idx);
        get_git_sign_style(git_status)
    };
    let line_num_style = get_line_number_style(line_idx == cursor_line, theme);

    let sign_span = Span::styled(
        sign_text,
        Style::default().fg(sign_color).add_modifier(Modifier::BOLD),
    );
    let line_num_span = Span::styled(line_num_text, line_num_style);

    spans.push(sign_span);
    spans.push(line_num_span);

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Unified decoration rendering
// ---------------------------------------------------------------------------

use ovim_core::editor::decoration::{
    Decoration, DecorationPlacement, DecorationStyle as DecStyle, ProjectedDecorations,
};
use ovim_core::editor::ProjectedDiagnostics;

/// Per-line render cache fingerprint: the projected EOL/inline decorations
/// plus the line's full diagnostic set (ranges + severities). The underline
/// squiggle is baked into cached rows and the diagnostic set can change
/// without a buffer edit (save → republish), so the decoration hash alone —
/// which only sees the line's single best-severity EOL message — is not
/// enough to invalidate. (OV-00329)
fn line_decoration_cache_hash(
    decorations: &ProjectedDecorations,
    diagnostics: &ProjectedDiagnostics,
    line_idx: usize,
) -> u64 {
    decorations
        .line_hash(line_idx)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ diagnostics.line_hash(line_idx)
}

/// Convert a `DecorationStyle` (framework-independent) to a ratatui `Style`.
fn decoration_to_ratatui_style(ds: &DecStyle) -> Style {
    let mut style = Style::default();
    if let Some(fg) = &ds.fg {
        style = style.fg(ovim_color_to_ratatui(*fg));
    }
    if let Some(bg) = &ds.bg {
        style = style.bg(ovim_color_to_ratatui(*bg));
    }
    if ds.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if ds.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if ds.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn ovim_color_to_ratatui(c: ovim_core::color::Color) -> Color {
    use ovim_core::color::Color as C;
    match c {
        C::Black => Color::Black,
        C::Red => Color::Red,
        C::Green => Color::Green,
        C::Yellow => Color::Yellow,
        C::Blue => Color::Blue,
        C::Magenta => Color::Magenta,
        C::Cyan => Color::Cyan,
        C::White => Color::White,
        C::DarkGray => Color::DarkGray,
        C::LightRed => Color::LightRed,
        C::LightGreen => Color::LightGreen,
        C::LightYellow => Color::LightYellow,
        C::LightBlue => Color::LightBlue,
        C::LightMagenta => Color::LightMagenta,
        C::LightCyan => Color::LightCyan,
        C::Gray => Color::Gray,
        C::Rgb(r, g, b) => Color::Rgb(r, g, b),
        C::Indexed(i) => Color::Indexed(i),
        C::Reset => Color::Reset,
    }
}

/// Truncate a rendered line to `max_width` display columns.
///
/// Walks spans left-to-right, keeping whole characters that fit.  Partial
/// spans at the boundary are split so the total display width is exactly
/// `max_width` (or less if the last character is wide).
fn truncate_line_to_width(line: &mut Line<'static>, max_width: usize) {
    let mut total: usize = 0;
    let mut keep_spans = 0;

    // First pass: find the truncation point.
    let mut split_at: Option<(usize, usize)> = None; // (span_index, budget_cols)
    for (i, span) in line.spans.iter().enumerate() {
        let span_width: usize = span.content.chars().map(char_display_width).sum();
        if total + span_width <= max_width {
            total += span_width;
            keep_spans += 1;
        } else {
            split_at = Some((i, max_width - total));
            break;
        }
    }

    // Second pass: apply truncation.
    if let Some((span_idx, budget)) = split_at {
        let style = line.spans[span_idx].style;
        let mut kept = String::new();
        let mut used = 0;
        for ch in line.spans[span_idx].content.chars() {
            let w = char_display_width(ch);
            if used + w > budget {
                break;
            }
            kept.push(ch);
            used += w;
        }
        line.spans.truncate(keep_spans);
        if !kept.is_empty() {
            line.spans.push(Span::styled(kept, style));
        }
    }
    // All spans fit — nothing to truncate.
}

/// Apply inline decorations to a rendered line by splicing styled spans.
///
/// Decorations are inserted right-to-left (highest char_idx first) so earlier
/// insertions don't shift the positions of later ones.
fn apply_inline_decorations(
    line: &mut Line<'static>,
    decorations: &[&Decoration],
    char_mapping: &[usize],
    h_offset: usize,
    wrap: bool,
    line_start_offset: usize,
) {
    if decorations.is_empty() {
        return;
    }

    // Sort right-to-left by char_offset
    let mut sorted: Vec<&&Decoration> = decorations.iter().collect();
    sorted.sort_by(|a, b| {
        let a_off = a.placement.char_offset();
        let b_off = b.placement.char_offset();
        b_off.cmp(&a_off) // reverse order
    });

    for dec in sorted {
        // Derive line-relative char_idx from absolute char_offset.
        let char_idx = match &dec.placement {
            DecorationPlacement::Inline { char_offset } => {
                char_offset.saturating_sub(line_start_offset)
            }
            _ => continue,
        };

        // Map through char_mapping (handles tab expansion)
        let expanded_col = if char_idx < char_mapping.len() {
            char_mapping[char_idx]
        } else if !char_mapping.is_empty() {
            *char_mapping.last().unwrap() + 1
        } else {
            char_idx
        };

        // Adjust for horizontal scroll in nowrap mode
        let insert_col = if !wrap {
            if expanded_col < h_offset {
                continue;
            }
            expanded_col - h_offset
        } else {
            expanded_col
        };

        let style = decoration_to_ratatui_style(&dec.style);

        // Walk spans to find insertion point, then split and insert
        let mut char_count = 0;
        let mut span_idx = 0;
        let mut found = false;

        while span_idx < line.spans.len() {
            let span_chars: usize = line.spans[span_idx].content.chars().count();
            if char_count + span_chars > insert_col {
                let offset_in_span = insert_col - char_count;
                let content = line.spans[span_idx].content.to_string();
                let span_style = line.spans[span_idx].style;

                let before: String = content.chars().take(offset_in_span).collect();
                let after: String = content.chars().skip(offset_in_span).collect();

                line.spans.remove(span_idx);
                let mut insert_at = span_idx;
                if !before.is_empty() {
                    line.spans
                        .insert(insert_at, Span::styled(before, span_style));
                    insert_at += 1;
                }
                line.spans
                    .insert(insert_at, Span::styled(dec.text.clone(), style));
                insert_at += 1;
                if !after.is_empty() {
                    line.spans
                        .insert(insert_at, Span::styled(after, span_style));
                }
                found = true;
                break;
            } else if char_count + span_chars == insert_col {
                line.spans
                    .insert(span_idx + 1, Span::styled(dec.text.clone(), style));
                found = true;
                break;
            }
            char_count += span_chars;
            span_idx += 1;
        }

        if !found {
            line.spans.push(Span::styled(dec.text.clone(), style));
        }
    }
}

/// Width of the gap (in columns) between rendered code and an EOL diagnostic.
const EOL_DIAG_GAP: usize = 2;

/// Truncate `text` to at most `max_chars` characters, appending `...` when
/// truncation happens. If `max_chars` is too small to fit even the ellipsis,
/// returns whatever prefix fits with no marker.
///
/// Note: char count, not display width — a wide-char (CJK, emoji) message
/// can render up to 2x the budget. Pre-existing behavior; sharpening this
/// is a separate concern.
fn fit_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars < 3 {
        return text.chars().take(max_chars).collect();
    }
    let prefix: String = text.chars().take(max_chars - 3).collect();
    format!("{prefix}...")
}

/// Total display width of all spans in a Line.
fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| s.content.chars().map(char_display_width).sum::<usize>())
        .sum()
}

/// Display width of the row's real content: trailing all-space spans with a
/// default style (the padding `split_line_into_rows` appends) are ignored.
/// Placement decisions must use this, not `line_display_width` — under soft
/// wrap every row arrives padded to `text_width`, and measuring the padding
/// made `place_eol_on_line` treat every short line as "full", pushing its
/// diagnostic to the far screen edge instead of next to the code.
fn content_display_width(line: &Line<'_>) -> usize {
    let mut spans = line.spans.as_slice();
    while let Some(last) = spans.last() {
        if last.content.chars().all(|c| c == ' ') && last.style == Style::default() {
            spans = &spans[..spans.len() - 1];
        } else {
            break;
        }
    }
    spans
        .iter()
        .map(|s| s.content.chars().map(char_display_width).sum::<usize>())
        .sum()
}

/// Pad the line with trailing space spans until it reaches `target_width`.
/// No-op if the line already meets or exceeds `target_width`.
fn pad_line_to(line: &mut Line<'static>, target_width: usize) {
    let width = line_display_width(line);
    if width < target_width {
        line.spans.push(Span::raw(" ".repeat(target_width - width)));
    }
}

/// Where an EOL decoration should be placed within a rendered row.
#[derive(Debug, Clone, Copy)]
enum EolPlacement {
    /// Append the diagnostic right after the row's existing content,
    /// pad the row to `text_width`. Used when the line lives inside a
    /// single budget: non-centered mode, single-row no-overflow case.
    Append { text_width: usize },
    /// Clip the row at `code_box_width` (no bleed past the box), anchor
    /// the diagnostic immediately after the rendered code (so it sits
    /// close to the line, not at the far edge), and pad to `render_width`.
    /// Used in centered (textwidth) mode where lines render into a wider
    /// rect than the code-box: code stays inside the code-box, but the
    /// diagnostic is free to extend into the right margin.
    AtBoxEdge {
        code_box_width: usize,
        render_width: usize,
    },
}

/// Apply end-of-line decorations to a rendered row.
///
/// Strips trailing padding, appends each decoration's styled text (with a
/// `EOL_DIAG_GAP`-column gap), truncates to fit, and re-pads. The exact
/// anchor and final width depend on `placement` — see [`EolPlacement`].
///
/// `AtBoxEdge` performs its clip + anchor + pad work even when there are
/// no decorations, so callers can use it to enforce no-bleed geometry on
/// every row of a wrapped line in centered mode. `Append` short-circuits
/// when there's nothing to do (caller has already padded to text_width).
fn apply_eol_decorations(
    row: &mut Line<'static>,
    decorations: &[&Decoration],
    placement: EolPlacement,
) {
    // Append with no decorations: caller's padding already handles this.
    if decorations.is_empty() && matches!(placement, EolPlacement::Append { .. }) {
        return;
    }

    // Remove trailing padding spans so we know where the code actually ends.
    while let Some(last) = row.spans.last() {
        if last.content.chars().all(|c| c == ' ') && last.style == Style::default() {
            row.spans.pop();
        } else {
            break;
        }
    }

    // Resolve where the diagnostic anchors and what we pad to. AtBoxEdge
    // also clips any content past the code-box edge so hints/code don't
    // bleed into the diagnostic margin, then anchors the diagnostic right
    // after the (clipped) rendered code — short lines get the diagnostic
    // close, long lines (clipped at code_box_width) get it at the box edge.
    let (diag_start, final_width) = match placement {
        EolPlacement::Append { text_width } => (line_display_width(row), text_width),
        EolPlacement::AtBoxEdge {
            code_box_width,
            render_width,
        } => {
            truncate_line_to_width(row, code_box_width);
            (line_display_width(row), render_width)
        }
    };

    // Pad the row up to the anchor (extends short lines to a consistent column).
    pad_line_to(row, diag_start);

    // Append the diagnostic if we have one and there's room for gap + ≥1 char.
    let remaining = final_width.saturating_sub(diag_start);
    if !decorations.is_empty() && remaining >= EOL_DIAG_GAP + 4 {
        // First decoration wins (already priority-sorted). The message may
        // use everything up to the row edge: the space right of the code is
        // otherwise unused, and diagnostics are exactly what the user wants
        // to read there.
        let dec = &decorations[0];
        let msg = fit_with_ellipsis(&dec.text, remaining - EOL_DIAG_GAP);
        let style = decoration_to_ratatui_style(&dec.style);
        row.spans.push(Span::raw(" ".repeat(EOL_DIAG_GAP)));
        row.spans.push(Span::styled(msg, style));
    }

    pad_line_to(row, final_width);
}

/// Place an EOL decoration on a single rendered line, choosing the right
/// strategy from `(text_width, render_width, line_width, has_decs)`:
///
/// - **Centered (render_width > text_width)** → `AtBoxEdge`, which clips the
///   line at the code-box edge (no bleed into the margin) and anchors the
///   diagnostic immediately after the rendered code. Short lines get the
///   diagnostic close; lines that reach (or exceed) the box edge get the
///   diagnostic at the box edge — the message is free to extend into the
///   right margin in both cases.
/// - **Non-centered, line + hints overflow text_width with decs present**
///   → `overlay_eol_decoration_at_edge`, which steals the rightmost columns
///   so the diagnostic stays visible.
/// - **Non-centered, line fits or no decs** → `Append`, the default
///   "diagnostic floats after code" behavior.
fn place_eol_on_line(
    line: &mut Line<'static>,
    eol_decs: &[&Decoration],
    text_width: usize,
    render_width: usize,
) {
    if render_width > text_width {
        apply_eol_decorations(
            line,
            eol_decs,
            EolPlacement::AtBoxEdge {
                code_box_width: text_width,
                render_width,
            },
        );
    } else if !eol_decs.is_empty() && content_display_width(line) >= text_width {
        overlay_eol_decoration_at_edge(line, eol_decs, text_width);
    } else {
        apply_eol_decorations(line, eol_decs, EolPlacement::Append { text_width });
    }
}

/// Place EOL decorations across the visual rows of a wrapped line. The
/// last row gets the diagnostic via [`place_eol_on_line`]; in centered
/// mode every other row is clipped + padded to `render_width` so they
/// don't bleed into the diagnostic margin.
fn place_eol_on_visual_rows(
    rows: &mut [Line<'static>],
    eol_decs: &[&Decoration],
    text_width: usize,
    render_width: usize,
) {
    if rows.is_empty() {
        return;
    }
    let centered = render_width > text_width;
    let last = rows.len() - 1;
    for (i, row) in rows.iter_mut().enumerate() {
        if i == last {
            place_eol_on_line(row, eol_decs, text_width, render_width);
        } else if centered {
            apply_eol_decorations(
                row,
                &[],
                EolPlacement::AtBoxEdge {
                    code_box_width: text_width,
                    render_width,
                },
            );
        }
    }
}

/// Overlay an EOL decoration at the right edge of a line that already exceeds
/// `text_width` (typically because inline decorations pushed it beyond the
/// viewport). The diagnostic replaces the rightmost columns of the rendered
/// line so it's always visible without affecting cursor positioning.
fn overlay_eol_decoration_at_edge(
    line: &mut Line<'static>,
    decorations: &[&Decoration],
    text_width: usize,
) {
    if decorations.is_empty() || text_width < EOL_DIAG_GAP + 6 {
        return;
    }

    let dec = &decorations[0];
    let style = decoration_to_ratatui_style(&dec.style);

    // Budget: gap + message at the right edge, message capped at 1/3 of viewport.
    let msg = fit_with_ellipsis(&dec.text, text_width / 3);
    let overlay_width = EOL_DIAG_GAP + msg.chars().map(char_display_width).sum::<usize>();

    // Truncate code to make room, pad up to the truncation point, then push
    // gap + styled message + final padding.
    let truncate_to = text_width.saturating_sub(overlay_width);
    truncate_line_to_width(line, truncate_to);
    pad_line_to(line, truncate_to);
    line.spans.push(Span::raw(" ".repeat(EOL_DIAG_GAP)));
    line.spans.push(Span::styled(msg, style));
    pad_line_to(line, text_width);
}

/// Per-line viewport context shared between `lock_ranges_for_line` and `ai_region_ranges_for_line`.
struct ViewportSlice<'a> {
    line_idx: usize,
    line_content_chars: usize,
    char_mapping: &'a [usize],
    expanded_text: &'a str,
    line_text: &'a str,
    wrap: bool,
    h_offset: usize,
    text_width: usize,
    precedes: bool,
}

fn lock_ranges_for_line(
    buffer: &crate::buffer::Buffer,
    slice: &ViewportSlice,
) -> Vec<(usize, usize)> {
    let line_start_char = buffer.rope().line_to_char(slice.line_idx);
    let line_end_char = line_start_char.saturating_add(slice.line_content_chars);
    let viewport_end = slice.h_offset.saturating_add(slice.text_width);
    let mut ranges = Vec::new();

    for lock in buffer.ai_locks().iter().filter(|lock| lock.blocks_edits) {
        let start_abs = lock.start_char.max(line_start_char);
        let end_abs = lock.end_char.min(line_end_char);
        if end_abs <= start_abs {
            continue;
        }

        let mut start_col = remap_char_col(start_abs - line_start_char, slice.char_mapping);
        let mut end_col_exclusive = remap_char_col(end_abs - line_start_char, slice.char_mapping);

        if !slice.wrap {
            let start_display = expanded_char_to_display_col(slice.expanded_text, start_col);
            let end_display = expanded_char_to_display_col(slice.expanded_text, end_col_exclusive);
            if end_display <= slice.h_offset || start_display >= viewport_end {
                continue;
            }
            let offset_adjustment = if slice.precedes { 1 } else { 0 };
            start_col = display_col_to_char_idx(
                slice.line_text,
                start_display.saturating_sub(slice.h_offset) + offset_adjustment,
            );
            end_col_exclusive = display_col_to_char_idx(
                slice.line_text,
                end_display.saturating_sub(slice.h_offset) + offset_adjustment,
            );
        }

        if end_col_exclusive > start_col {
            ranges.push((start_col, end_col_exclusive - 1));
        }
    }

    ranges
}

#[allow(clippy::type_complexity)]
fn ai_region_ranges_for_line(
    editor: &Editor,
    slice: &ViewportSlice,
) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let line_start_char = editor.buffer().rope().line_to_char(slice.line_idx);
    let line_end_char = line_start_char.saturating_add(slice.line_content_chars);
    let viewport_end = slice.h_offset.saturating_add(slice.text_width);
    let selected_region_id = editor.ai_selected_region_id();
    let mut generated_ranges = Vec::new();
    let mut selected_ranges = Vec::new();

    for region in editor.ai_regions() {
        let show_generated = region.status == AiRegionStatus::Generated;
        let show_selected = selected_region_id == Some(region.id)
            && matches!(
                region.status,
                AiRegionStatus::Generated | AiRegionStatus::Running
            );
        if !show_generated && !show_selected {
            continue;
        }

        let start_abs = region.start_char.max(line_start_char);
        let end_abs = region.end_char.min(line_end_char);
        if end_abs <= start_abs {
            continue;
        }

        let mut start_col = remap_char_col(start_abs - line_start_char, slice.char_mapping);
        let mut end_col_exclusive = remap_char_col(end_abs - line_start_char, slice.char_mapping);

        if !slice.wrap {
            let start_display = expanded_char_to_display_col(slice.expanded_text, start_col);
            let end_display = expanded_char_to_display_col(slice.expanded_text, end_col_exclusive);
            if end_display <= slice.h_offset || start_display >= viewport_end {
                continue;
            }
            let offset_adjustment = if slice.precedes { 1 } else { 0 };
            start_col = display_col_to_char_idx(
                slice.line_text,
                start_display.saturating_sub(slice.h_offset) + offset_adjustment,
            );
            end_col_exclusive = display_col_to_char_idx(
                slice.line_text,
                end_display.saturating_sub(slice.h_offset) + offset_adjustment,
            );
        }

        if end_col_exclusive > start_col {
            let range = (start_col, end_col_exclusive - 1);
            if show_generated {
                generated_ranges.push(range);
            }
            if show_selected {
                selected_ranges.push(range);
            }
        }
    }

    (generated_ranges, selected_ranges)
}

/// Splits a rendered Line into multiple visual rows for soft wrapping.
/// Each row fits within `width` display columns. Rows are padded to full width.
/// Wide characters (CJK, emoji) that don't fit at a row boundary are pushed to
/// the next row, with the remaining space padded (matching Neovim behavior).
fn split_line_into_rows(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    // Calculate total display width
    let total_width: usize = line
        .spans
        .iter()
        .map(|s| s.content.chars().map(char_display_width).sum::<usize>())
        .sum();

    if total_width <= width {
        // Line fits in one row - just pad it
        let mut row = line;
        let pad = width.saturating_sub(total_width);
        if pad > 0 {
            row.spans.push(Span::raw(" ".repeat(pad)));
        }
        return vec![row];
    }

    // Need to split spans across multiple rows
    let mut rows = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0;

    for span in line.spans {
        let style = span.style;
        let mut chunk = String::new();

        for ch in span.content.chars() {
            let ch_width = char_display_width(ch);

            if current_width + ch_width > width {
                // Flush accumulated chunk for this span
                if !chunk.is_empty() {
                    current_spans.push(Span::styled(chunk.clone(), style));
                    chunk.clear();
                }
                // Pad remaining space in current row
                let pad = width.saturating_sub(current_width);
                if pad > 0 {
                    current_spans.push(Span::raw(" ".repeat(pad)));
                }
                rows.push(Line::from(current_spans));
                current_spans = Vec::new();
                current_width = 0;
            }

            chunk.push(ch);
            current_width += ch_width;

            if current_width >= width {
                // Row exactly full, flush
                current_spans.push(Span::styled(chunk.clone(), style));
                chunk.clear();
                rows.push(Line::from(current_spans));
                current_spans = Vec::new();
                current_width = 0;
            }
        }

        if !chunk.is_empty() {
            current_spans.push(Span::styled(chunk, style));
        }
    }

    // Push remaining content as final row
    if !current_spans.is_empty() || rows.is_empty() {
        let pad = width.saturating_sub(current_width);
        if pad > 0 {
            current_spans.push(Span::raw(" ".repeat(pad)));
        }
        rows.push(Line::from(current_spans));
    }

    rows
}

/// Renders the buffer content and returns the viewport start line
pub fn render_buffer(
    frame: &mut Frame,
    editor: &Editor,
    theme: &Theme,
    layout: &BufferLayout,
    line_cache: &mut super::line_cache::LineRenderCache,
    window_context: Option<&WindowRenderContext>,
) -> usize {
    let area = layout.buffer_area;
    let buffer = editor.buffer();
    let rope = buffer.rope();

    // Use window-specific cursor if provided (for non-focused windows)
    let cursor = window_context
        .and_then(|ctx| ctx.cursor.as_ref())
        .unwrap_or_else(|| buffer.cursor());

    // Use Vim-compatible line count: trailing newline's phantom empty line
    // should not be rendered. The cursor is always bounded to real lines.
    let line_count = buffer.line_count();

    // Calculate visible range using scroll offset (not centering)
    // Use window-specific scroll offset if provided
    let visible_lines = area.height as usize;
    let start_line = window_context
        .and_then(|ctx| ctx.scroll_offset)
        .unwrap_or_else(|| editor.scroll_offset());
    // Visual sub-row offset within `start_line`: the first `top_skip` wrapped
    // rows of the top logical line are scrolled off the top edge. Only meaningful
    // under soft wrap (set to 0 otherwise below, once `has_wrap` is known).
    let mut top_skip = window_context
        .and_then(|ctx| ctx.scroll_subrow)
        .unwrap_or_else(|| editor.scroll_subrow());

    // Get horizontal viewport settings
    // Use window-specific horizontal offset if provided
    let h_offset = window_context
        .and_then(|ctx| ctx.horizontal_offset)
        .unwrap_or_else(|| editor.horizontal_offset());
    let wrap = editor.options.wrap;

    // Use layout-provided dimensions
    let line_num_width = layout.line_num_width;
    let gutter_width_u16 = layout.gutter_width as u16;

    // Split layout into gutter (left of buffer_area) and text_area (from
    // end of gutter to right edge of render_area). When render_area equals
    // buffer_area (the common case), text_area is exactly buffer_area
    // minus the gutter — same as before. In centered mode render_area
    // extends past buffer_area, giving text_area a wider rect that
    // includes the diagnostic margin.
    let render_area = layout.render_area;
    let render_right = render_area.x + render_area.width;
    let gutter_area = if layout.gutter_width > 0 {
        Some(Rect {
            x: area.x,
            y: area.y,
            width: gutter_width_u16,
            height: area.height,
        })
    } else {
        None
    };
    let text_area = Rect {
        x: area.x + gutter_width_u16,
        y: area.y,
        width: render_right.saturating_sub(area.x + gutter_width_u16),
        height: area.height,
    };

    // Get visual selection if in visual mode
    let visual_selection = if editor.mode().is_visual() {
        editor.visual_selection()
    } else {
        None
    };
    let has_code_walkthrough = editor.ai_chat_has_pending_code_explanation();
    let ai_selection = if editor.mode() == crate::mode::Mode::AiPrompt || has_code_walkthrough {
        editor.ai_state.active_selection.as_ref()
    } else {
        None
    };
    let walkthrough_range = if has_code_walkthrough {
        ai_selection.map(|selection| (selection.start_line, selection.end_line))
    } else {
        None
    };

    // Get current search if active
    let current_search = editor.search.current_search.as_ref();

    // Find matching bracket position if showmatch is enabled
    let bracket_positions: Option<((usize, usize), (usize, usize))> = if editor.options.showmatch {
        find_matching_bracket_position(buffer)
            .map(|matching_pos| ((cursor.line(), cursor.col().0), matching_pos))
    } else {
        None
    };

    // Build the visible text with syntax highlighting
    // Gutter lines are built inline to match wrap continuation rows
    let mut lines = Vec::new();
    let mut gutter_lines: Vec<Line<'static>> = Vec::new();
    let tab_width = editor.options.tab_width;
    let cursorline = editor.options.cursorline;
    let cursor_line_idx = cursor.line();
    // `text_width` is the document code-box (fixed at the layout's setting,
    // i.e. textwidth in centered mode); `render_width` is how wide the line
    // actually renders into. They differ in centered mode by exactly the
    // diagnostic margin width.
    let text_width = layout.text_width;
    let render_width = layout.render_width();
    let blank_line = " ".repeat(render_width);
    // Non-focused split panes carry the index of their own wrap map (built at
    // their own width); the single-window / focused path uses the global one.
    let wrap_map = match window_context.and_then(|ctx| ctx.wrap_map_window_index) {
        Some(window_idx) => editor
            .window_manager()
            .and_then(|wm| wm.get_window(window_idx))
            .and_then(|w| w.wrap_map()),
        None => editor.wrap_map(),
    };
    let has_wrap = wrap && wrap_map.is_some();
    // Sub-row scroll only applies under soft wrap — the renderer can't begin
    // partway into a logical line otherwise.
    if !has_wrap {
        top_skip = 0;
    }
    // To start rendering `top_skip` visual rows into the top logical line, emit
    // that many extra rows and drop them from the top after the layout loop.
    let emit_budget = visible_lines + top_skip;
    let mut visual_rows_used = 0;
    let buffer_version = buffer.version();
    let buffer_id = buffer.id();

    // Reset per-frame cache stats
    line_cache.reset_stats();

    // Pre-compute blame brackets for visible lines
    let blame_width = layout.blame_width;
    let blame_brackets = if blame_width > 0 {
        if let Some(blame) = buffer.git_blame() {
            let author_width = blame_width.saturating_sub(1 + 1 + 5 + 1 + 1); // bracket+sp+hash+sp+trailing_sp
            Some(compute_blame_brackets(
                blame,
                start_line,
                line_count.min(start_line + visible_lines + 50),
                author_width,
            ))
        } else {
            None
        }
    } else {
        None
    };

    let gutter_ctx = GutterContext {
        editor,
        buffer,
        theme,
        line_num_width,
        cursor_line: cursor_line_idx,
        blame_width,
        walkthrough_range,
    };

    let is_md_file = buffer
        .file_path()
        .map(|p| p.ends_with(".md"))
        .unwrap_or(false);

    // Reusable scratch buffers for shift_highlights_for_viewport — avoids
    // allocating two Vec<usize> per visible line per frame.
    let mut hl_shift_buffers = HighlightShiftBuffers::new();

    // Project every decoration through the edit log ONCE per render. Per-line
    // lookups in the loop below then read from a line-keyed map instead of
    // re-scanning `iter_all()` and re-projecting each decoration.
    let projected_decorations = if editor.ai_code_explanation_is_presenting_snapshot() {
        // Walkthrough code pages render an immutable virtual snapshot, not the
        // live LSP document. Reusing the editor-global decoration map here can
        // attach diagnostics and inlay hints from a different document state to
        // coincidentally matching lines in the snapshot.
        Default::default()
    } else {
        editor.decorations.project_all(rope, buffer.edit_log())
    };
    // Raw diagnostics projected through the same edit log, once per frame:
    // the squiggle, gutter sign, and echo must land on the same line as the
    // projected EOL virtual text above. (OV-00328)
    let projected_diagnostics = if editor.ai_code_explanation_is_presenting_snapshot() {
        ovim_core::editor::ProjectedDiagnostics::default()
    } else {
        editor.project_diagnostics()
    };

    let mut line_idx = start_line;
    while line_idx < line_count && visual_rows_used < emit_budget {
        if line_idx < rope.len_lines() {
            // --- Cache check: try to reuse a previously rendered stable line ---
            // Determine upfront if this line has transient overlays that prevent caching.
            let has_visual_on_line = visual_selection
                .map(|((sl, _), (el, _))| line_idx >= sl && line_idx <= el)
                .unwrap_or(false);
            let is_cursor_line_early = cursorline && line_idx == cursor_line_idx;
            let is_cursor_line_for_conceal =
                line_idx == cursor_line_idx && editor.options.markdown_conceal && is_md_file;
            let has_yank_flash = editor
                .yank_flash()
                .is_some_and(|f| f.contains_line(line_idx));
            let line_diagnostics_early = projected_diagnostics.for_line(line_idx);
            let has_bracket = bracket_positions
                .is_some_and(|((l1, _), (l2, _))| line_idx == l1 || line_idx == l2);
            let has_search = current_search.is_some();
            let has_ai_selection_on_line = ai_selection
                .map(|selection| line_idx >= selection.start_line && line_idx <= selection.end_line)
                .unwrap_or(false);
            let line_start_char = rope.line_to_char(line_idx);
            let mut line_end_char = if line_idx + 1 < rope.len_lines() {
                rope.line_to_char(line_idx + 1)
            } else {
                rope.len_chars()
            };
            if line_end_char > line_start_char && rope.char(line_end_char - 1) == '\n' {
                line_end_char = line_end_char.saturating_sub(1);
            }
            let has_ai_lock_on_line = buffer
                .ai_locks()
                .iter()
                .filter(|lock| lock.blocks_edits)
                .any(|lock| lock.start_char < line_end_char && lock.end_char > line_start_char);
            let has_ai_generated_on_line = editor.ai_regions().iter().any(|region| {
                region.status == AiRegionStatus::Generated
                    && region.start_char < line_end_char
                    && region.end_char > line_start_char
            });
            let is_stable = !has_visual_on_line
                && !is_cursor_line_early
                && !is_cursor_line_for_conceal
                && !has_yank_flash
                && !has_bracket
                && !has_search
                && !has_ai_selection_on_line
                && !has_ai_lock_on_line
                && !has_ai_generated_on_line;

            let md_conceal = editor.options.markdown_conceal;
            // Per-line decoration hash from the per-frame projection. Lets the
            // line cache invalidate only the lines whose decorations actually
            // changed (vs. the previous global generation counter that wiped
            // every cached line on any LSP push).
            let dec_hash = line_decoration_cache_hash(
                &projected_decorations,
                &projected_diagnostics,
                line_idx,
            );

            if is_stable {
                if let Some(cached_line) = line_cache.get(
                    buffer_id,
                    line_idx,
                    buffer_version,
                    h_offset,
                    text_width,
                    wrap,
                    tab_width,
                    md_conceal,
                    dec_hash,
                ) {
                    let mut cached_line = cached_line.clone();
                    // Cached line has inline decorations but needs
                    // EOL decorations (diagnostics) applied fresh. The
                    // per-frame projection cache (built before the loop)
                    // avoids re-scanning every decoration here.
                    let eol_decs: Vec<&Decoration> = projected_decorations.eol_for_line(line_idx);

                    if has_wrap {
                        let mut visual_rows = split_line_into_rows(cached_line, text_width);
                        place_eol_on_visual_rows(
                            &mut visual_rows,
                            &eol_decs,
                            text_width,
                            render_width,
                        );
                        for (row_idx, row) in visual_rows.into_iter().enumerate() {
                            if visual_rows_used >= emit_budget {
                                break;
                            }
                            if gutter_area.is_some() {
                                gutter_lines.push(build_gutter_line(
                                    &gutter_ctx,
                                    line_idx,
                                    row_idx > 0,
                                    line_diagnostics_early,
                                    blame_brackets
                                        .as_ref()
                                        .and_then(|b| b.get(line_idx - start_line)),
                                ));
                            }
                            lines.push(row);
                            visual_rows_used += 1;
                        }
                    } else {
                        place_eol_on_line(&mut cached_line, &eol_decs, text_width, render_width);
                        pad_line_to(&mut cached_line, render_width);
                        if gutter_area.is_some() {
                            gutter_lines.push(build_gutter_line(
                                &gutter_ctx,
                                line_idx,
                                false,
                                line_diagnostics_early,
                                blame_brackets
                                    .as_ref()
                                    .and_then(|b| b.get(line_idx - start_line)),
                            ));
                        }
                        lines.push(cached_line);
                        visual_rows_used += 1;
                    }
                    line_idx += 1;
                    continue;
                }
            }

            // Visible content of the line (trailing terminator stripped).
            let line_text_original = ovim_core::display::line_content(rope, line_idx);

            // Optional markdown conceal (skip on cursor line so editing isn't blind)
            let (exp, concealed_links) =
                if is_md_file && editor.options.markdown_conceal && !is_cursor_line_for_conceal {
                    let spans = scan_markdown_conceal(&line_text_original);
                    if spans.is_empty() {
                        (
                            expand_tabs_with_mapping(&line_text_original, tab_width),
                            Vec::new(),
                        )
                    } else {
                        let transform = crate::ui::renderer::markdown_conceal::apply_conceal(
                            &line_text_original,
                            &spans,
                        );
                        let links = crate::ui::renderer::markdown_conceal::extract_concealed_links(
                            &spans, &transform,
                        );
                        (
                            compose_conceal_and_tabs(&line_text_original, &transform, tab_width),
                            links,
                        )
                    }
                } else {
                    (
                        expand_tabs_with_mapping(&line_text_original, tab_width),
                        Vec::new(),
                    )
                };
            let expanded_text = exp.text;
            let conceal_byte_map = exp.byte_mapping;
            let control_ranges = exp.control_ranges;
            let char_mapping = exp.char_mapping;

            // Apply horizontal viewport slicing if nowrap is set. In wrap mode
            // the rendered slice IS the expanded text — no allocation needed.
            // We keep the optional sliced String alive in `line_sliced` so
            // `line_text` can borrow from it (!wrap) or from `expanded_text` (wrap).
            let (line_sliced, precedes, _extends): (Option<String>, bool, bool) = if !wrap {
                let (sliced, p, e) =
                    slice_horizontal_viewport(&expanded_text, h_offset, text_width);
                (Some(sliced), p, e)
            } else {
                (None, false, false)
            };
            let line_text: &str = line_sliced.as_deref().unwrap_or(&expanded_text);

            // Get syntax highlights for this line and remap them for expanded text
            let original_highlights = buffer.highlights_for_line(line_idx);
            let mut syntax_highlights = remap_highlights(&original_highlights, &conceal_byte_map);

            // Shift syntax highlights for horizontal viewport if nowrap
            if !wrap {
                syntax_highlights = shift_highlights_for_viewport(
                    &syntax_highlights,
                    &expanded_text,
                    line_text,
                    h_offset,
                    text_width,
                    precedes,
                    &mut hl_shift_buffers,
                );
            }

            // Check if we need special highlighting (visual selection or search)
            let has_visual_selection = has_visual_on_line;

            let search_matches = if let Some(search) = current_search {
                search.find_all_in_line(line_text)
            } else {
                Vec::new()
            };

            // Check if this is the cursor line and cursorline option is on
            let is_cursor_line = is_cursor_line_early;

            // Remap visual selection columns from original to expanded char indices
            let remapped_visual_selection = visual_selection.map(|((sl, sc), (el, ec))| {
                let sc = if line_idx == sl {
                    remap_char_col(sc, &char_mapping)
                } else {
                    sc
                };
                let ec = if line_idx == el {
                    remap_char_col(ec, &char_mapping)
                } else {
                    ec
                };
                ((sl, sc), (el, ec))
            });

            // Adjust visual selection for horizontal viewport if nowrap
            let remapped_visual_selection = if !wrap {
                remapped_visual_selection.map(|((sl, sc), (el, ec))| {
                    let adjust = |expanded_char_col: usize| -> usize {
                        let display_col =
                            expanded_char_to_display_col(&expanded_text, expanded_char_col);
                        let viewport_display_col = display_col.saturating_sub(h_offset);
                        let offset_adjustment = if precedes { 1 } else { 0 };
                        display_col_to_char_idx(line_text, viewport_display_col + offset_adjustment)
                    };
                    let sc = if line_idx == sl { adjust(sc) } else { sc };
                    let ec = if line_idx == el { adjust(ec) } else { ec };
                    ((sl, sc), (el, ec))
                })
            } else {
                remapped_visual_selection
            };

            // Check if this line has a bracket to highlight (remap through char_mapping)
            let bracket_col = bracket_positions.and_then(|((l1, c1), (l2, c2))| {
                if line_idx == l1 {
                    Some(remap_char_col(c1, &char_mapping))
                } else if line_idx == l2 {
                    Some(remap_char_col(c2, &char_mapping))
                } else {
                    None
                }
            });

            // Adjust bracket column for horizontal viewport if nowrap
            let bracket_col = if !wrap {
                bracket_col.and_then(|expanded_char_col| {
                    // Convert expanded char index to display column
                    let display_col =
                        expanded_char_to_display_col(&expanded_text, expanded_char_col);
                    // Check if bracket is in visible horizontal range
                    if display_col >= h_offset && display_col < h_offset + text_width {
                        // Convert to char index in the sliced text
                        let viewport_display_col = display_col - h_offset;
                        let offset_adjustment = if precedes { 1 } else { 0 };
                        let sliced_char_idx = display_col_to_char_idx(
                            line_text,
                            viewport_display_col + offset_adjustment,
                        );
                        Some(sliced_char_idx)
                    } else {
                        None // Bracket is outside viewport
                    }
                })
            } else {
                bracket_col
            };

            // Reuse diagnostics already fetched for cache check
            let line_diagnostics = line_diagnostics_early;
            let has_diagnostics = !line_diagnostics.is_empty();
            let remapped_diagnostics: Vec<RemappedDiagnostic> = line_diagnostics
                .iter()
                .filter_map(|d| {
                    // This renderer only handles diagnostics that start on this line.
                    // LSP ranges for multi-line diagnostics are line-relative, so
                    // clamp the end to line length instead of using end.character.
                    if d.range.start.line as usize != line_idx {
                        return None;
                    }

                    // Convert UTF-16 offsets to char indices, then remap through expansion
                    let start_char = utf16_offset_to_char_idx(
                        &line_text_original,
                        d.range.start.character as usize,
                    );
                    let line_char_count_for_diag = line_text_original.chars().count();
                    let end_char = if d.range.end.line as usize > line_idx {
                        line_char_count_for_diag
                    } else {
                        utf16_offset_to_char_idx(
                            &line_text_original,
                            d.range.end.character as usize,
                        )
                        .min(line_char_count_for_diag)
                    };
                    let color = match d.severity {
                        Some(lsp_types::DiagnosticSeverity::ERROR) => Color::Red,
                        Some(lsp_types::DiagnosticSeverity::WARNING) => Color::Yellow,
                        Some(lsp_types::DiagnosticSeverity::INFORMATION) => Color::Cyan,
                        Some(lsp_types::DiagnosticSeverity::HINT) => Color::Gray,
                        _ => Color::Red,
                    };
                    let expanded_start = remap_char_col(start_char, &char_mapping);
                    let expanded_end = remap_char_col(end_char, &char_mapping);
                    let (expanded_start, expanded_end) = if expanded_start <= expanded_end {
                        (expanded_start, expanded_end)
                    } else {
                        (expanded_end, expanded_start)
                    };
                    if expanded_start == expanded_end {
                        return None;
                    }

                    // Adjust for horizontal viewport in nowrap mode
                    if !wrap {
                        let start_display =
                            expanded_char_to_display_col(&expanded_text, expanded_start);
                        let end_display =
                            expanded_char_to_display_col(&expanded_text, expanded_end);
                        // Skip if entirely outside viewport
                        if end_display <= h_offset || start_display >= h_offset + text_width {
                            return None;
                        }
                        let offset_adj = if precedes { 1 } else { 0 };
                        let mut sliced_start = display_col_to_char_idx(
                            line_text,
                            start_display.saturating_sub(h_offset) + offset_adj,
                        );
                        let mut sliced_end = display_col_to_char_idx(
                            line_text,
                            end_display.saturating_sub(h_offset) + offset_adj,
                        );
                        if sliced_start > sliced_end {
                            std::mem::swap(&mut sliced_start, &mut sliced_end);
                        }
                        if sliced_start == sliced_end {
                            return None;
                        }
                        Some(RemappedDiagnostic {
                            start: sliced_start,
                            end: sliced_end,
                            color,
                        })
                    } else {
                        Some(RemappedDiagnostic {
                            start: expanded_start,
                            end: expanded_end,
                            color,
                        })
                    }
                })
                .collect();
            let line_char_count = line_text_original.chars().count();
            let viewport_slice = ViewportSlice {
                line_idx,
                line_content_chars: line_char_count,
                char_mapping: &char_mapping,
                expanded_text: &expanded_text,
                line_text,
                wrap,
                h_offset,
                text_width,
                precedes,
            };
            let ai_lock_ranges = lock_ranges_for_line(buffer, &viewport_slice);
            let (ai_generated_ranges, ai_selected_ranges) =
                ai_region_ranges_for_line(editor, &viewport_slice);
            let ai_selection_ranges = if let Some(selection) = ai_selection {
                if line_idx < selection.start_line || line_idx > selection.end_line {
                    Vec::new()
                } else {
                    let (start_col, end_col_inclusive) =
                        if selection.mode_before_prompt == crate::mode::Mode::VisualLine {
                            if line_char_count == 0 {
                                (0, 0)
                            } else {
                                (0, line_char_count - 1)
                            }
                        } else if selection.start_line == selection.end_line {
                            (
                                selection.start_col,
                                selection.end_col.min(line_char_count.saturating_sub(1)),
                            )
                        } else if line_idx == selection.start_line {
                            (selection.start_col, line_char_count.saturating_sub(1))
                        } else if line_idx == selection.end_line {
                            (0, selection.end_col.min(line_char_count.saturating_sub(1)))
                        } else if line_char_count == 0 {
                            (0, 0)
                        } else {
                            (0, line_char_count - 1)
                        };

                    if line_char_count == 0 || end_col_inclusive < start_col {
                        Vec::new()
                    } else {
                        let mut start = remap_char_col(start_col, &char_mapping);
                        let mut end_exclusive = remap_char_col(
                            (end_col_inclusive + 1).min(line_char_count),
                            &char_mapping,
                        );

                        if !wrap {
                            let start_display = expanded_char_to_display_col(&expanded_text, start);
                            let end_display =
                                expanded_char_to_display_col(&expanded_text, end_exclusive);
                            if end_display <= h_offset || start_display >= h_offset + text_width {
                                Vec::new()
                            } else {
                                let offset_adj = if precedes { 1 } else { 0 };
                                start = display_col_to_char_idx(
                                    line_text,
                                    start_display.saturating_sub(h_offset) + offset_adj,
                                );
                                end_exclusive = display_col_to_char_idx(
                                    line_text,
                                    end_display.saturating_sub(h_offset) + offset_adj,
                                );
                                if end_exclusive > start {
                                    vec![(start, end_exclusive - 1)]
                                } else {
                                    Vec::new()
                                }
                            }
                        } else if end_exclusive > start {
                            vec![(start, end_exclusive - 1)]
                        } else {
                            Vec::new()
                        }
                    }
                }
            } else {
                Vec::new()
            };

            // Check if this line is in a yank flash region
            let yank_flash = editor.yank_flash().and_then(|flash| {
                if flash.contains_line(line_idx) {
                    Some(flash.col_range_for_line(line_idx))
                } else {
                    None
                }
            });

            // Inline decorations (inlay hints) and EOL decorations (LSP
            // diagnostics rendered as virtual text) both need the detailed
            // path: inline decs require `apply_inline_decorations` to splice
            // their spans into the rendered line, and EOL decs require the
            // EOL placement step that the simple path doesn't run. Use the
            // unprojected `for_line` here for a cheap BTreeMap lookup; the
            // detailed block does the projection.
            let any_decoration = !editor.decorations.for_line(line_idx).is_empty();

            // Always use character-by-character rendering if we have any highlighting
            let needs_detailed_rendering = has_visual_selection
                || !search_matches.is_empty()
                || !syntax_highlights.is_empty()
                || is_cursor_line
                || bracket_col.is_some()
                || has_diagnostics
                || yank_flash.is_some()
                || !ai_selection_ranges.is_empty()
                || !ai_lock_ranges.is_empty()
                || !ai_generated_ranges.is_empty()
                || !ai_selected_ranges.is_empty()
                || !concealed_links.is_empty()
                || any_decoration;

            if needs_detailed_rendering {
                let mut line = render_line_with_highlights(
                    theme,
                    line_text,
                    line_idx,
                    remapped_visual_selection,
                    editor.mode(),
                    &search_matches,
                    &syntax_highlights,
                    &remapped_diagnostics,
                    &control_ranges,
                );

                // Apply cursorline background if this is the cursor line
                if is_cursor_line && yank_flash.is_none() {
                    let cursorline_bg = Color::Rgb(40, 40, 50); // Subtle dark blue background
                    for span in &mut line.spans {
                        span.style = span.style.bg(cursorline_bg);
                    }
                }

                // Apply yank flash highlight
                if let Some(col_range) = yank_flash {
                    let flash_bg = Color::Rgb(60, 50, 20); // Warm amber glow
                    match col_range {
                        None => {
                            // Linewise flash: highlight entire line
                            for span in &mut line.spans {
                                span.style = span.style.bg(flash_bg);
                            }
                        }
                        Some((start_col, end_col)) => {
                            // Character-wise flash: highlight column range
                            apply_bg_to_column_range(&mut line, start_col, end_col, flash_bg);
                        }
                    }
                }

                // Preserve syntax colors while making an interactive walkthrough
                // more prominent than an ordinary AI-prompt selection.
                let ai_selection_bg = if walkthrough_range.is_some() {
                    WALKTHROUGH_SELECTION_BG
                } else {
                    Color::Rgb(62, 70, 82)
                };
                for (start_col, end_col) in &ai_selection_ranges {
                    apply_bg_to_column_range(&mut line, *start_col, *end_col, ai_selection_bg);
                }

                // Generated AI edits stay visible after completion with a muted background.
                let ai_generated_bg = Color::Rgb(30, 48, 38);
                for (start_col, end_col) in &ai_generated_ranges {
                    apply_bg_to_column_range(&mut line, *start_col, *end_col, ai_generated_bg);
                }

                // Highlight active AI lock spans so users see protected regions.
                let ai_lock_bg = Color::Rgb(26, 44, 62);
                for (start_col, end_col) in &ai_lock_ranges {
                    apply_bg_to_column_range(&mut line, *start_col, *end_col, ai_lock_bg);
                }

                // When cursor is inside an AI block, highlight the whole block as selected.
                let ai_selected_bg = Color::Rgb(44, 64, 78);
                for (start_col, end_col) in &ai_selected_ranges {
                    apply_bg_to_column_range(&mut line, *start_col, *end_col, ai_selected_bg);
                }

                // Apply concealed link underline styling
                if !concealed_links.is_empty() {
                    let link_color = Color::Rgb(100, 149, 237); // Cornflower blue
                    for link in &concealed_links {
                        apply_fg_modifier_to_column_range(
                            &mut line,
                            link.view_start,
                            link.view_end,
                            link_color,
                            Modifier::UNDERLINED,
                        );
                    }
                }

                // Apply bracket highlighting
                if let Some(col) = bracket_col {
                    let bracket_style = Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD);
                    apply_style_at_column(&mut line, col, bracket_style);
                }

                // Apply decorations from the unified DecorationMap BEFORE
                // caching.  The cache key includes dec_gen so it misses when
                // decorations change.  Storing the decorated line ensures
                // cache-hit frames render the same decorations that cursor
                // positioning (inline_width_before) accounts for.
                //
                // Step E: read projected offsets from the per-frame
                // projection cache built before the loop.  In steady state
                // (accumulator keeps `source_version` current) this yields
                // the same result as the stored offsets; when Step F removes
                // the accumulator the projection becomes the sole source of
                // truth.
                let line_decorations = projected_decorations.for_line(line_idx);
                let inline_decs: Vec<&Decoration> = line_decorations
                    .iter()
                    .filter(|d| matches!(d.placement, DecorationPlacement::Inline { .. }))
                    .collect();
                let eol_decs: Vec<&Decoration> = line_decorations
                    .iter()
                    .filter(|d| matches!(d.placement, DecorationPlacement::EndOfLine { .. }))
                    .collect();

                if !inline_decs.is_empty() {
                    let line_start_offset = editor.buffer().rope().line_to_char(line_idx);
                    apply_inline_decorations(
                        &mut line,
                        &inline_decs,
                        &char_mapping,
                        h_offset,
                        wrap,
                        line_start_offset,
                    );
                }

                // Store in cache AFTER decorations so cache-hit frames
                // match cursor positioning.
                line_cache.put(
                    buffer_id,
                    line_idx,
                    buffer_version,
                    h_offset,
                    text_width,
                    wrap,
                    tab_width,
                    md_conceal,
                    dec_hash,
                    line.clone(),
                    is_stable,
                );

                // Soft wrap: split into visual rows if needed
                if has_wrap {
                    let mut visual_rows = split_line_into_rows(line, text_width);
                    place_eol_on_visual_rows(&mut visual_rows, &eol_decs, text_width, render_width);
                    for (row_idx, row) in visual_rows.into_iter().enumerate() {
                        if visual_rows_used >= emit_budget {
                            break;
                        }
                        if gutter_area.is_some() {
                            gutter_lines.push(build_gutter_line(
                                &gutter_ctx,
                                line_idx,
                                row_idx > 0,
                                line_diagnostics,
                                blame_brackets
                                    .as_ref()
                                    .and_then(|b| b.get(line_idx - start_line)),
                            ));
                        }
                        lines.push(row);
                        visual_rows_used += 1;
                    }
                } else {
                    // No wrap: in non-centered mode, ratatui clips overflow
                    // at text_area's right edge, so we don't truncate (cursor
                    // tracking would desync). In centered mode, AtBoxEdge
                    // explicitly clips at the code-box and pads into the
                    // diagnostic margin — `place_eol_on_line` picks the
                    // right strategy.
                    place_eol_on_line(&mut line, &eol_decs, text_width, render_width);
                    pad_line_to(&mut line, render_width);
                    if gutter_area.is_some() {
                        gutter_lines.push(build_gutter_line(
                            &gutter_ctx,
                            line_idx,
                            false,
                            line_diagnostics,
                            blame_brackets
                                .as_ref()
                                .and_then(|b| b.get(line_idx - start_line)),
                        ));
                    }
                    lines.push(line);
                    visual_rows_used += 1;
                }
            } else {
                // Simple rendering path (no highlighting) — always stable
                let simple_line = Line::from(line_text.to_string());
                line_cache.put(
                    buffer_id,
                    line_idx,
                    buffer_version,
                    h_offset,
                    text_width,
                    wrap,
                    tab_width,
                    md_conceal,
                    dec_hash,
                    simple_line,
                    true,
                );

                if has_wrap {
                    if line_text.is_empty() {
                        if gutter_area.is_some() {
                            gutter_lines.push(build_gutter_line(
                                &gutter_ctx,
                                line_idx,
                                false,
                                &[],
                                blame_brackets
                                    .as_ref()
                                    .and_then(|b| b.get(line_idx - start_line)),
                            ));
                        }
                        lines.push(Line::from(" ".repeat(render_width)));
                        visual_rows_used += 1;
                    } else {
                        // Split by display width, not char count — CJK/emoji
                        // characters are width 2 and would overflow the terminal
                        // row if counted as 1.
                        let mut chunk_idx = 0;
                        let mut row_text = String::new();
                        let mut row_width = 0;

                        for ch in line_text.chars() {
                            let ch_width = char_display_width(ch);

                            if row_width + ch_width > text_width && !row_text.is_empty() {
                                // Flush current row
                                if visual_rows_used >= emit_budget {
                                    break;
                                }
                                if gutter_area.is_some() {
                                    gutter_lines.push(build_gutter_line(
                                        &gutter_ctx,
                                        line_idx,
                                        chunk_idx > 0,
                                        &[],
                                        blame_brackets
                                            .as_ref()
                                            .and_then(|b| b.get(line_idx - start_line)),
                                    ));
                                }
                                let pad = render_width.saturating_sub(row_width);
                                if pad > 0 {
                                    row_text.push_str(&" ".repeat(pad));
                                }
                                lines.push(Line::from(std::mem::take(&mut row_text)));
                                visual_rows_used += 1;
                                chunk_idx += 1;
                                row_width = 0;
                            }

                            row_text.push(ch);
                            row_width += ch_width;
                        }

                        // Flush the last row
                        if !row_text.is_empty() && visual_rows_used < emit_budget {
                            if gutter_area.is_some() {
                                gutter_lines.push(build_gutter_line(
                                    &gutter_ctx,
                                    line_idx,
                                    chunk_idx > 0,
                                    &[],
                                    blame_brackets
                                        .as_ref()
                                        .and_then(|b| b.get(line_idx - start_line)),
                                ));
                            }
                            let pad = render_width.saturating_sub(row_width);
                            if pad > 0 {
                                row_text.push_str(&" ".repeat(pad));
                            }
                            lines.push(Line::from(row_text));
                            visual_rows_used += 1;
                        }
                    }
                } else {
                    // No wrap: pad simple lines too
                    if gutter_area.is_some() {
                        gutter_lines.push(build_gutter_line(
                            &gutter_ctx,
                            line_idx,
                            false,
                            &[],
                            blame_brackets
                                .as_ref()
                                .and_then(|b| b.get(line_idx - start_line)),
                        ));
                    }
                    let line_display_len = unicode_width::UnicodeWidthStr::width(line_text);
                    let line_text = if line_display_len < render_width {
                        format!(
                            "{}{}",
                            line_text,
                            " ".repeat(render_width - line_display_len)
                        )
                    } else {
                        line_text.to_string()
                    };
                    lines.push(Line::from(line_text));
                    visual_rows_used += 1;
                }
            }
        } else {
            // Line beyond end of file - clear it
            lines.push(Line::from(blank_line.clone()));
            visual_rows_used += 1;
        }
        line_idx += 1;
    }

    // Drop the first `top_skip` visual rows so the viewport begins partway into
    // the wrapped top logical line. These rows always belong to `start_line` (a
    // real line with gutter entries), so `lines` and `gutter_lines` stay aligned.
    if top_skip > 0 {
        let drop = top_skip.min(lines.len());
        lines.drain(0..drop);
        if !gutter_lines.is_empty() {
            let gdrop = top_skip.min(gutter_lines.len());
            gutter_lines.drain(0..gdrop);
        }
        visual_rows_used = visual_rows_used.saturating_sub(top_skip);
    }

    // Fill remaining rows with blanks
    while visual_rows_used < visible_lines {
        lines.push(Line::from(blank_line.clone()));
        visual_rows_used += 1;
    }

    // Render gutter
    if let Some(gutter_area) = gutter_area {
        let gutter_paragraph = Paragraph::new(gutter_lines);
        frame.render_widget(gutter_paragraph, gutter_area);
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().bg(Color::Reset));
    frame.render_widget(paragraph, text_area);

    start_line
}

/// A diagnostic range remapped to expanded char indices for rendering
pub struct RemappedDiagnostic {
    pub start: usize,
    pub end: usize,
    pub color: Color,
}

/// Renders a single line with all highlighting (syntax, visual selection, search, diagnostics, control chars)
#[allow(clippy::too_many_arguments)]
pub fn render_line_with_highlights(
    theme: &Theme,
    line_text: &str,
    line_idx: usize,
    visual_selection: Option<((usize, usize), (usize, usize))>,
    mode: crate::mode::Mode,
    search_matches: &[(usize, usize)],
    syntax_highlights: &[(std::ops::Range<usize>, crate::syntax::HighlightGroup)],
    diagnostics: &[RemappedDiagnostic],
    control_ranges: &[std::ops::Range<usize>],
) -> Line<'static> {
    let chars: Vec<char> = line_text.chars().collect();
    let num_chars = chars.len();
    let mut spans = Vec::new();

    if num_chars == 0 {
        return Line::from(spans);
    }

    // Build a map from character index to byte index
    let mut byte_indices: Vec<usize> = Vec::with_capacity(num_chars + 1);
    byte_indices.push(0);
    for (byte_idx, _) in line_text.char_indices().skip(1) {
        byte_indices.push(byte_idx);
    }
    byte_indices.push(line_text.len()); // End position

    // --- Pre-compute per-character style attributes in one pass each ---

    // 1. Visual selection: computed inline (cheap — no array scan needed, just arithmetic)

    // 2. Search matches: mark each char position
    let mut search_flags: Vec<bool> = vec![false; num_chars];
    for &(start, end) in search_matches {
        let s = start.min(num_chars);
        let e = end.min(num_chars);
        for flag in search_flags[s..e].iter_mut() {
            *flag = true;
        }
    }

    // 3. Syntax highlights: resolve most-specific (smallest range) group per byte position,
    //    then map to char positions.
    let mut syntax_per_char: Vec<Option<crate::syntax::HighlightGroup>> = vec![None; num_chars];
    if !syntax_highlights.is_empty() {
        // For each byte, track (group, range_size) of the most specific highlight.
        let byte_len = line_text.len();
        let mut best_group: Vec<Option<(crate::syntax::HighlightGroup, usize)>> =
            vec![None; byte_len];

        for (range, group) in syntax_highlights {
            let range_size = range.end - range.start;
            let s = range.start.min(byte_len);
            let e = range.end.min(byte_len);
            for slot in best_group[s..e].iter_mut() {
                match slot {
                    Some((_, prev_size)) if *prev_size <= range_size => {} // keep tighter
                    _ => *slot = Some((*group, range_size)),
                }
            }
        }

        // Map byte-level results to char positions
        for (char_idx, &byte_idx) in byte_indices[..num_chars].iter().enumerate() {
            if byte_idx < byte_len {
                syntax_per_char[char_idx] = best_group[byte_idx].map(|(g, _)| g);
            }
        }
    }

    // 4. Diagnostics: mark each char position with underline color
    let mut diag_per_char: Vec<Option<Color>> = vec![None; num_chars];
    for d in diagnostics {
        let s = d.start.min(num_chars);
        let e = d.end.min(num_chars);
        let (s, e) = if s <= e { (s, e) } else { (e, s) };
        if s == e {
            continue;
        }
        for slot in diag_per_char[s..e].iter_mut() {
            if slot.is_none() {
                *slot = Some(d.color);
            }
        }
    }

    // 5. Control char ranges: mark each byte position, then map to chars
    let mut control_per_char: Vec<bool> = vec![false; num_chars];
    if !control_ranges.is_empty() {
        let byte_len = line_text.len();
        let mut control_bytes: Vec<bool> = vec![false; byte_len];
        for r in control_ranges {
            let s = r.start.min(byte_len);
            let e = r.end.min(byte_len);
            for flag in control_bytes[s..e].iter_mut() {
                *flag = true;
            }
        }
        for (char_idx, &byte_idx) in byte_indices[..num_chars].iter().enumerate() {
            if byte_idx < byte_len {
                control_per_char[char_idx] = control_bytes[byte_idx];
            }
        }
    }

    // --- Helper: compute visual selection for a given column ---
    let is_col_selected = |col: usize| -> bool {
        if let Some(((sel_start_line, sel_start_col), (sel_end_line, sel_end_col))) =
            visual_selection
        {
            match mode {
                crate::mode::Mode::VisualBlock => {
                    line_idx >= sel_start_line
                        && line_idx <= sel_end_line
                        && col >= sel_start_col
                        && col <= sel_end_col
                }
                _ => {
                    if line_idx == sel_start_line && line_idx == sel_end_line {
                        col >= sel_start_col && col <= sel_end_col
                    } else if line_idx == sel_start_line {
                        col >= sel_start_col
                    } else if line_idx == sel_end_line {
                        col <= sel_end_col
                    } else {
                        line_idx > sel_start_line && line_idx < sel_end_line
                    }
                }
            }
        } else {
            false
        }
    };

    // --- Main loop: group consecutive characters with identical styling ---
    let mut col_idx = 0;
    while col_idx < num_chars {
        let is_selected = is_col_selected(col_idx);
        let is_search_match = search_flags[col_idx];
        let syntax_group = syntax_per_char[col_idx];
        let diag_underline_color = diag_per_char[col_idx];
        let is_control = control_per_char[col_idx];

        // Extend span while styling is identical
        let mut end_col = col_idx + 1;
        while end_col < num_chars
            && is_col_selected(end_col) == is_selected
            && search_flags[end_col] == is_search_match
            && syntax_per_char[end_col] == syntax_group
            && diag_per_char[end_col] == diag_underline_color
            && control_per_char[end_col] == is_control
        {
            end_col += 1;
        }

        // Build the span for this range
        let text: String = chars[col_idx..end_col].iter().collect();

        // Apply styling based on priority: visual selection > search match > control char > syntax > normal
        let mut style = if is_selected {
            Style::default()
                .bg(crate::key_convert::convert_core_color(
                    theme.get_ui_color(UiGroup::Visual),
                ))
                .fg(Color::White)
        } else if is_search_match {
            Style::default()
                .bg(crate::key_convert::convert_core_color(
                    theme.get_ui_color(UiGroup::Search),
                ))
                .fg(Color::Black)
        } else if is_control {
            let color =
                crate::key_convert::convert_core_color(theme.get_color(HighlightGroup::SpecialKey));
            Style::default().fg(color)
        } else if let Some(group) = syntax_group {
            let color = crate::key_convert::convert_core_color(theme.get_color(group));
            let mut style = Style::default().fg(color);

            // Add modifiers for markup elements
            match group {
                HighlightGroup::MarkupHeading => {
                    style = style.add_modifier(Modifier::BOLD);
                }
                HighlightGroup::MarkupBold => {
                    style = style.add_modifier(Modifier::BOLD);
                }
                HighlightGroup::MarkupItalic => {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                _ => {}
            }
            style
        } else {
            Style::default()
        };

        // Apply diagnostic underline (additive — works on top of any style)
        if let Some(underline_color) = diag_underline_color {
            style = style.fg(underline_color).add_modifier(Modifier::UNDERLINED);
        }

        spans.push(Span::styled(text, style));
        col_idx = end_col;
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walkthrough_range_marks_every_inclusive_logical_line() {
        let range = Some((4, 6));
        assert!(!line_is_in_walkthrough(range, 3));
        assert!(line_is_in_walkthrough(range, 4));
        assert!(line_is_in_walkthrough(range, 5));
        assert!(line_is_in_walkthrough(range, 6));
        assert!(!line_is_in_walkthrough(range, 7));
        assert!(!line_is_in_walkthrough(None, 5));
    }

    #[test]
    fn test_split_line_wide_char_at_boundary() {
        // Width=4, content "abc世d"
        // 'a'=1, 'b'=1, 'c'=1, '世'=2 -> doesn't fit (3+2=5 > 4), pad row 1
        // Row 1: "abc " (padded), Row 2: "世d  " (padded)
        let line = Line::from(vec![Span::raw("abc世d")]);
        let rows = split_line_into_rows(line, 4);
        assert_eq!(rows.len(), 2);

        let row0_text: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let row1_text: String = rows[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(row0_text, "abc ");
        assert_eq!(row1_text, "世d "); // 世=2 + d=1 = 3, pad 1 to fill width 4
    }

    #[test]
    fn test_split_line_ascii_no_wide() {
        let line = Line::from(vec![Span::raw("abcdefgh")]);
        let rows = split_line_into_rows(line, 4);
        assert_eq!(rows.len(), 2);

        let row0_text: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let row1_text: String = rows[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(row0_text, "abcd");
        assert_eq!(row1_text, "efgh");
    }

    #[test]
    fn test_split_line_fits_in_one_row() {
        let line = Line::from(vec![Span::raw("ab")]);
        let rows = split_line_into_rows(line, 4);
        assert_eq!(rows.len(), 1);

        let text: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "ab  "); // padded
    }

    // --- slice_horizontal_viewport tests ---

    #[test]
    fn test_slice_viewport_ascii_fits() {
        let (text, precedes, extends) = slice_horizontal_viewport("hello", 0, 10);
        assert_eq!(text, "hello");
        assert!(!precedes);
        assert!(!extends);
    }

    #[test]
    fn test_slice_viewport_ascii_extends() {
        let (text, precedes, extends) = slice_horizontal_viewport("hello world!", 0, 6);
        assert_eq!(text.len(), 6);
        assert!(!precedes);
        assert!(extends);
        assert!(text.ends_with('>'));
    }

    #[test]
    fn test_slice_viewport_ascii_scrolled() {
        // "hello world!" scrolled 3 display cols, width 6
        let (text, precedes, extends) = slice_horizontal_viewport("hello world!", 3, 6);
        assert!(precedes);
        assert!(extends);
        assert_eq!(text.chars().next(), Some('<'));
        assert!(text.ends_with('>'));
        assert_eq!(text.chars().count(), 6);
    }

    #[test]
    fn test_slice_viewport_cjk_fits() {
        // "a世b" has display width 4 (1+2+1)
        let (text, precedes, extends) = slice_horizontal_viewport("a世b", 0, 5);
        assert_eq!(text, "a世b");
        assert!(!precedes);
        assert!(!extends);
    }

    #[test]
    fn test_slice_viewport_cjk_extends() {
        // "a世b世c" has display width 8 (1+2+1+2+1), viewport width 5
        let (text, precedes, extends) = slice_horizontal_viewport("a世b世c", 0, 5);
        assert!(!precedes);
        assert!(extends);
        // Should fit "a世b" (4 cols) + '>' = 5 display cols total
        assert!(text.ends_with('>'));
    }

    #[test]
    fn test_slice_viewport_cjk_scrolled() {
        // "a世b世c" scrolled past 'a' and '世' (3 display cols), width 5
        let (text, precedes, extends) = slice_horizontal_viewport("a世b世c", 3, 5);
        assert!(precedes);
        assert!(!extends);
        // Should show '<' + content from display col 3 + possibly '>'
        assert_eq!(text.chars().next(), Some('<'));
    }

    #[test]
    fn test_slice_viewport_zero_width() {
        let (text, precedes, extends) = slice_horizontal_viewport("hello", 0, 0);
        assert_eq!(text, "");
        assert!(!precedes);
        assert!(!extends);
    }

    // --- Helper function tests ---

    #[test]
    fn test_expanded_char_to_display_col() {
        // "a世b" → char 0='a'(width 1), char 1='世'(width 2), char 2='b'(width 1)
        assert_eq!(expanded_char_to_display_col("a世b", 0), 0);
        assert_eq!(expanded_char_to_display_col("a世b", 1), 1);
        assert_eq!(expanded_char_to_display_col("a世b", 2), 3);
    }

    #[test]
    fn test_display_col_to_char_idx_basic() {
        // "a世b" display cols: a=0, 世=1-2, b=3
        assert_eq!(display_col_to_char_idx("a世b", 0), 0);
        assert_eq!(display_col_to_char_idx("a世b", 1), 1);
        assert_eq!(display_col_to_char_idx("a世b", 2), 1); // mid-wide → same char
        assert_eq!(display_col_to_char_idx("a世b", 3), 2);
    }

    #[test]
    fn test_utf16_offset_to_char_idx() {
        // ASCII: 1 UTF-16 unit per char
        assert_eq!(utf16_offset_to_char_idx("hello", 2), 2);
        // BMP CJK: 1 UTF-16 unit per char
        assert_eq!(utf16_offset_to_char_idx("a世b", 1), 1);
        assert_eq!(utf16_offset_to_char_idx("a世b", 2), 2);
        // Supplementary (emoji): 2 UTF-16 units
        assert_eq!(utf16_offset_to_char_idx("a😀b", 1), 1); // start of emoji
        assert_eq!(utf16_offset_to_char_idx("a😀b", 3), 2); // after emoji (2 UTF-16 units)
    }

    #[test]
    fn test_apply_eol_decorations_on_padded_wrapped_row() {
        use ovim_core::editor::decoration::*;

        let base = Line::from("let x = 1;".to_string());
        let mut rows = split_line_into_rows(base, 30);
        let mut first = rows.remove(0);

        let dec = Decoration {
            placement: DecorationPlacement::EndOfLine { char_offset: 0 },
            source: DecorationSource::Diagnostic,
            text: "\u{f057} uh oh".to_string(),
            display_width: 7,
            style: DecorationStyle::new(ovim_core::color::Color::Red).with_italic(),
            priority: 0,
            source_version: 0,
        };

        apply_eol_decorations(&mut first, &[&dec], EolPlacement::Append { text_width: 30 });

        let mut rendered = String::new();
        for span in &first.spans {
            rendered.push_str(span.content.as_ref());
        }
        assert!(rendered.contains("uh oh"));

        let display_width: usize = rendered.chars().map(char_display_width).sum();
        assert_eq!(display_width, 30);
    }

    /// Regression: under soft wrap (the default), `split_line_into_rows` pads
    /// every row to `text_width`. Measuring that padding made
    /// `place_eol_on_line` treat every short line as "full" and take the
    /// overlay branch — the diagnostic landed right-aligned at the screen
    /// edge, capped to a third of the width, far away from its code.
    #[test]
    fn test_padded_wrapped_row_gets_adjacent_eol_not_edge_overlay() {
        use ovim_core::editor::decoration::*;

        let text_width = 60;
        let base = Line::from("let x = 1;".to_string());
        let mut rows = split_line_into_rows(base, text_width);
        assert_eq!(rows.len(), 1);
        let mut row = rows.remove(0);

        let dec = Decoration {
            placement: DecorationPlacement::EndOfLine { char_offset: 0 },
            source: DecorationSource::Diagnostic,
            text: "unused variable: `x`".to_string(),
            display_width: 20,
            style: DecorationStyle::new(ovim_core::color::Color::Red).with_italic(),
            priority: 0,
            source_version: 0,
        };

        place_eol_on_line(&mut row, &[&dec], text_width, text_width);

        let rendered: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.starts_with("let x = 1;  unused variable: `x`"),
            "diagnostic must sit {EOL_DIAG_GAP} columns after the code, not at the far edge; got {rendered:?}"
        );
        let display_width: usize = rendered.chars().map(char_display_width).sum();
        assert_eq!(display_width, text_width, "row is re-padded to full width");
    }

    /// A row whose real content fills the box still uses the overlay so the
    /// diagnostic stays visible.
    #[test]
    fn test_full_content_row_still_overlays_at_edge() {
        use ovim_core::editor::decoration::*;

        let text_width = 40;
        let full: String = "x".repeat(text_width);
        let mut row = Line::from(full);

        let dec = Decoration {
            placement: DecorationPlacement::EndOfLine { char_offset: 0 },
            source: DecorationSource::Diagnostic,
            text: "too long".to_string(),
            display_width: 8,
            style: DecorationStyle::new(ovim_core::color::Color::Red).with_italic(),
            priority: 0,
            source_version: 0,
        };

        place_eol_on_line(&mut row, &[&dec], text_width, text_width);

        let rendered: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.ends_with("too long"),
            "overlay should place the message at the right edge; got {rendered:?}"
        );
        let display_width: usize = rendered.chars().map(char_display_width).sum();
        assert_eq!(display_width, text_width);
    }

    /// OV-00329 regression: a diagnostic republish WITHOUT a buffer edit
    /// (save → cargo-check adds a second diagnostic at a new span while the
    /// top message stays the same) must change the line's render cache key.
    /// The decoration fingerprint alone — the only diagnostic-derived key
    /// component before the fix — collides in this scenario, so the cached
    /// row (with the old squiggle baked in) would be served until an
    /// edit/scroll/resize.
    #[test]
    fn test_diagnostic_republish_without_edit_changes_line_cache_key() {
        use crate::ui::renderer::line_cache::LineRenderCache;
        use ovim_core::editor::decoration::{decorations_from_diagnostics, DecorationSource};

        let mut editor = Editor::with_content("let x = 1;\n");

        let top = lsp_types::Diagnostic {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 4),
                lsp_types::Position::new(0, 5),
            ),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: "top message".to_string(),
            ..lsp_types::Diagnostic::default()
        };
        let extra = lsp_types::Diagnostic {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 8),
                lsp_types::Position::new(0, 9),
            ),
            severity: Some(lsp_types::DiagnosticSeverity::WARNING),
            message: "second span".to_string(),
            ..lsp_types::Diagnostic::default()
        };

        let rope = editor.buffer().rope().clone();
        let version = editor.buffer().version() as u64;

        editor.set_test_diagnostics(vec![top.clone()]);
        editor.decorations.replace_source(
            DecorationSource::Diagnostic,
            decorations_from_diagnostics(std::slice::from_ref(&top), &rope, version),
            &rope,
        );
        let decs1 = editor
            .decorations
            .project_all(&rope, editor.buffer().edit_log());
        let h1 = line_decoration_cache_hash(&decs1, &editor.project_diagnostics(), 0);

        // Republish: second diagnostic at a new span, top message unchanged,
        // no buffer edit.
        editor.set_test_diagnostics(vec![top.clone(), extra.clone()]);
        editor.decorations.replace_source(
            DecorationSource::Diagnostic,
            decorations_from_diagnostics(&[top, extra], &rope, version),
            &rope,
        );
        let decs2 = editor
            .decorations
            .project_all(&rope, editor.buffer().edit_log());

        // The pre-fix key component cannot see the change (same best-severity
        // EOL decoration) — this is exactly the collision the fix closes.
        assert_eq!(decs1.line_hash(0), decs2.line_hash(0));

        let h2 = line_decoration_cache_hash(&decs2, &editor.project_diagnostics(), 0);
        assert_ne!(
            h2, h1,
            "cache key must change when the diagnostic set changes"
        );

        // And a row cached under the old key re-renders under the new one.
        let mut cache = LineRenderCache::new();
        let _ = cache.get(1, 0, 1, 0, 80, false, 4, false, h1); // sync version bookkeeping
        cache.put(1, 0, 1, 0, 80, false, 4, false, h1, Line::from("row"), true);
        assert!(cache.get(1, 0, 1, 0, 80, false, 4, false, h1).is_some());
        assert!(cache.get(1, 0, 1, 0, 80, false, 4, false, h2).is_none());
    }

    #[test]
    fn test_lock_ranges_for_line_basic() {
        let mut buffer = crate::buffer::Buffer::new_from_str("hello world\n");
        buffer.add_ai_lock(1, 6, 11);

        let line_text = "hello world";
        let exp = expand_tabs_with_mapping(line_text, 4);
        let expanded = exp.text;
        let char_mapping = exp.char_mapping;

        let slice = ViewportSlice {
            line_idx: 0,
            line_content_chars: line_text.chars().count(),
            char_mapping: &char_mapping,
            expanded_text: &expanded,
            line_text: &expanded,
            wrap: true,
            h_offset: 0,
            text_width: 80,
            precedes: false,
        };
        let ranges = lock_ranges_for_line(&buffer, &slice);
        assert_eq!(ranges, vec![(6, 10)]);
    }

    #[test]
    fn test_render_line_empty_string() {
        let theme = Theme::default();
        let line = render_line_with_highlights(
            &theme,
            "",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &[],
            &[],
            &[],
        );
        assert!(line.spans.is_empty());
    }

    #[test]
    fn test_render_line_plain_text_single_span() {
        let theme = Theme::default();
        let line = render_line_with_highlights(
            &theme,
            "hello world",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &[],
            &[],
            &[],
        );
        // No highlights → should coalesce into one span
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "hello world");
    }

    #[test]
    fn test_render_line_syntax_highlight_splits_spans() {
        let theme = Theme::default();
        // Highlight bytes 0..2 ("fn") as Keyword
        let highlights = vec![(0..2, crate::syntax::HighlightGroup::Keyword)];
        let line = render_line_with_highlights(
            &theme,
            "fn main()",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &highlights,
            &[],
            &[],
        );
        // Should have at least 2 spans: "fn" (highlighted) and " main()" (default)
        assert!(line.spans.len() >= 2);
        assert_eq!(line.spans[0].content.as_ref(), "fn");
    }

    #[test]
    fn test_render_line_search_match_overrides_syntax() {
        let theme = Theme::default();
        let highlights = vec![(0..5, crate::syntax::HighlightGroup::Function)];
        // Search match on chars 0..5 ("hello")
        let search = vec![(0, 5)];
        let line = render_line_with_highlights(
            &theme,
            "hello world",
            0,
            None,
            crate::mode::Mode::Normal,
            &search,
            &highlights,
            &[],
            &[],
        );
        // First span should be search-highlighted, not syntax-highlighted
        assert!(line.spans.len() >= 2);
        assert_eq!(line.spans[0].content.as_ref(), "hello");
        // Search highlight has bg color (non-default style)
        assert_ne!(line.spans[0].style, Style::default());
    }

    #[test]
    fn test_render_line_multibyte_chars() {
        let theme = Theme::default();
        // "aé" - 'é' is 2 bytes in UTF-8. Highlight byte range 0..1 ("a" only).
        let highlights = vec![(0..1, crate::syntax::HighlightGroup::Keyword)];
        let line = render_line_with_highlights(
            &theme,
            "aéb",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &highlights,
            &[],
            &[],
        );
        assert!(line.spans.len() >= 2);
        assert_eq!(line.spans[0].content.as_ref(), "a");
    }

    #[test]
    fn test_render_line_diagnostic_underline() {
        let theme = Theme::default();
        let diags = vec![RemappedDiagnostic {
            start: 0,
            end: 5,
            color: Color::Red,
        }];
        let line = render_line_with_highlights(
            &theme,
            "error here",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &[],
            &diags,
            &[],
        );
        // First span should have underline modifier
        assert!(line.spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED));
    }

    #[test]
    fn test_render_line_most_specific_syntax_wins() {
        let theme = Theme::default();
        // Two overlapping highlights: broad (0..10) and narrow (2..4).
        // The narrow one should win for chars at byte positions 2-3.
        let highlights = vec![
            (0..10, crate::syntax::HighlightGroup::Variable),
            (2..4, crate::syntax::HighlightGroup::Keyword),
        ];
        let line = render_line_with_highlights(
            &theme,
            "abcdefghij",
            0,
            None,
            crate::mode::Mode::Normal,
            &[],
            &highlights,
            &[],
            &[],
        );
        // Should have 3 spans: "ab" (Variable), "cd" (Keyword), "efghij" (Variable)
        assert!(line.spans.len() >= 3);
        assert_eq!(line.spans[0].content.as_ref(), "ab");
        assert_eq!(line.spans[1].content.as_ref(), "cd");
    }
}
