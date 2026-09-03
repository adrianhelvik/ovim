use crate::syntax::{Theme, UiGroup};
use ratatui::style::{Color, Modifier, Style};
use std::ops::Range;

/// Adjusts syntax highlight ranges based on tab expansion mapping
pub fn remap_highlights<T: Copy>(
    highlights: &[(Range<usize>, T)],
    byte_mapping: &[(usize, usize)],
) -> Vec<(Range<usize>, T)> {
    highlights
        .iter()
        .map(|(range, group)| {
            // Find mapped positions for start and end
            let new_start = byte_mapping
                .iter()
                .find(|(orig, _)| *orig >= range.start)
                .map(|(_, expanded)| *expanded)
                .unwrap_or(0);

            let new_end = byte_mapping
                .iter()
                .find(|(orig, _)| *orig >= range.end)
                .map(|(_, expanded)| *expanded)
                .unwrap_or(new_start);

            (new_start..new_end, *group)
        })
        .collect()
}

/// Returns the style for line numbers in the gutter
pub fn get_line_number_style(is_current_line: bool, theme: &Theme) -> Style {
    if is_current_line {
        Style::default()
            .fg(crate::key_convert::convert_core_color(
                theme.get_ui_color(UiGroup::LineNumberCurrent),
            ))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(crate::key_convert::convert_core_color(
            theme.get_ui_color(UiGroup::LineNumber),
        ))
    }
}

/// Returns the style and text for git status signs
pub fn get_git_sign_style(status: Option<crate::LineStatus>) -> (&'static str, Color) {
    match status {
        Some(crate::LineStatus::Added) => ("+ ", Color::Green),
        Some(crate::LineStatus::Modified) => ("~ ", Color::Yellow),
        Some(crate::LineStatus::Removed) => ("- ", Color::Red),
        None => ("  ", Color::DarkGray),
    }
}

/// Returns the sign text and color for diagnostic severity in the gutter.
/// Each string MUST be exactly SIGN_WIDTH (2) display columns.
pub fn get_diagnostic_sign_style(
    severity: Option<lsp_types::DiagnosticSeverity>,
) -> (&'static str, Color) {
    use lsp_types::DiagnosticSeverity;
    match severity {
        Some(DiagnosticSeverity::ERROR) => ("E ", Color::Red),
        Some(DiagnosticSeverity::WARNING) => ("W ", Color::Yellow),
        Some(DiagnosticSeverity::INFORMATION) => ("I ", Color::Cyan),
        Some(DiagnosticSeverity::HINT) => ("H ", Color::Gray),
        _ => ("E ", Color::Red), // Default to error style
    }
}

/// Muted color palette for blame gutter (12 distinct colors)
const BLAME_COLORS: [Color; 12] = [
    Color::Rgb(130, 170, 200), // steel blue
    Color::Rgb(180, 140, 180), // muted purple
    Color::Rgb(140, 180, 140), // sage green
    Color::Rgb(200, 160, 120), // sandy brown
    Color::Rgb(160, 160, 200), // lavender
    Color::Rgb(180, 180, 130), // olive
    Color::Rgb(170, 140, 140), // dusty rose
    Color::Rgb(130, 180, 170), // teal
    Color::Rgb(190, 150, 150), // mauve
    Color::Rgb(150, 170, 130), // fern
    Color::Rgb(170, 160, 180), // wisteria
    Color::Rgb(180, 170, 140), // khaki
];

/// Returns a deterministic color for a blame commit hash
pub fn blame_color_for_hash(hash: &str) -> Color {
    let idx: usize = hash.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    BLAME_COLORS[idx % BLAME_COLORS.len()]
}
