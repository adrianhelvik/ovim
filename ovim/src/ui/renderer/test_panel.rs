//! Right-side test panel rendering.
//!
//! Shown when the test panel is open (`<Space>tt` / auto-opened by a test
//! run). Displays the latest run with live streaming output, plus a
//! one-line summary per previous run.

use ovim_core::editor::{TestRun, TestRunStatus};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::editor::Editor;

mod colors {
    use ratatui::style::Color;

    pub const PASS: Color = Color::Rgb(166, 227, 161); // Green
    pub const FAIL: Color = Color::Rgb(243, 139, 168); // Red
    pub const RUNNING: Color = Color::Rgb(249, 226, 175); // Yellow
    pub const CANCELLED: Color = Color::Rgb(108, 112, 134); // Overlay0
    pub const TEXT: Color = Color::Rgb(205, 214, 244);
    pub const DIM: Color = Color::Rgb(127, 132, 156); // Overlay1
    pub const BORDER: Color = Color::DarkGray;
    pub const KEY: Color = Color::White;
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render_test_panel(frame: &mut Frame, editor: &Editor, area: Rect) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .title(" Tests ")
        .title_style(
            Style::default()
                .fg(colors::TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(colors::BORDER));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    let panel = editor.test_panel();
    let lines = match panel.latest() {
        Some(latest) => run_lines(panel.runs.as_slice(), latest, inner.height as usize),
        None => empty_state_lines(),
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Keybinding cheat sheet shown before any test has run.
fn empty_state_lines() -> Vec<Line<'static>> {
    let key = Style::default()
        .fg(colors::KEY)
        .add_modifier(Modifier::BOLD);
    let label = Style::default().fg(colors::DIM);
    let bindings: &[(&str, &str)] = &[
        ("<Space>tn", "run nearest test"),
        ("<Space>tf", "run this file's tests"),
        ("<Space>ta", "run the whole suite"),
        ("<Space>tl", "re-run the last test"),
        ("<Space>tv", "jump to the last-run test"),
        ("<Space>to", "raw output in a buffer"),
        ("<Esc>", "close this panel"),
        ("<Space>tt", "toggle this panel"),
    ];

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " No test runs yet",
            Style::default().fg(colors::TEXT),
        )),
        Line::from(""),
    ];
    for (keys, what) in bindings {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{:<10}", keys), key),
            Span::raw("  "),
            Span::styled(*what, label),
        ]));
    }
    lines
}

fn status_parts(run: &TestRun) -> (String, Color, &'static str) {
    match run.status {
        TestRunStatus::Running => {
            let frame_idx = (run.started.elapsed().as_millis() / 100) as usize % SPINNER.len();
            (SPINNER[frame_idx].to_string(), colors::RUNNING, "running")
        }
        TestRunStatus::Passed => ("✓".to_string(), colors::PASS, "passed"),
        TestRunStatus::Failed => ("✗".to_string(), colors::FAIL, "failed"),
        TestRunStatus::Cancelled => ("⊘".to_string(), colors::CANCELLED, "superseded"),
    }
}

fn run_lines<'a>(runs: &'a [TestRun], latest: &'a TestRun, height: usize) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();

    // Previous runs, one summary line each (oldest first), dimmed.
    for run in &runs[..runs.len().saturating_sub(1)] {
        let (icon, color, verb) = status_parts(run);
        let detail = run
            .summary
            .clone()
            .unwrap_or_else(|| ovim_core::editor::format_duration(run.elapsed()));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(icon, Style::default().fg(color)),
            Span::styled(
                format!(" {} {} · {}", run.scope_label, verb, detail),
                Style::default().fg(colors::DIM),
            ),
        ]));
    }
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }

    // Latest run header: status, command, directory.
    let (icon, color, verb) = status_parts(latest);
    let elapsed = ovim_core::editor::format_duration(latest.elapsed());
    let mut headline = vec![
        Span::raw(" "),
        Span::styled(
            icon,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} {} · {}", latest.scope_label, verb, elapsed),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(summary) = &latest.summary {
        headline.push(Span::styled(
            format!(" · {}", summary),
            Style::default().fg(colors::TEXT),
        ));
    }
    lines.push(Line::from(headline));
    lines.push(Line::from(Span::styled(
        format!(" $ {}", latest.command),
        Style::default().fg(colors::DIM),
    )));
    lines.push(Line::from(Span::styled(
        format!(" in {}", latest.dir_name),
        Style::default().fg(colors::DIM),
    )));
    lines.push(Line::from(""));

    // Keep the actionable failure visible even when the raw output tail has
    // scrolled past the assertion and stack trace.
    if let Some(failure) = latest.failures.first() {
        if let Some(location) = &failure.location {
            let column = location
                .column
                .map(|column| format!(":{column}"))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!(" {}:{}{}", location.path.display(), location.line, column),
                Style::default()
                    .fg(colors::FAIL)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        if !failure.message.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" {}", failure.message),
                Style::default().fg(colors::TEXT),
            )));
        }
        lines.push(Line::from(Span::styled(
            " :cfirst opens failure · :cn next",
            Style::default().fg(colors::DIM),
        )));
        lines.push(Line::from(""));
    }

    // Output tail fills the rest.
    let used = lines.len();
    let budget = height.saturating_sub(used);
    let start = latest.lines.len().saturating_sub(budget);
    if latest.truncated > 0 || start > 0 {
        // One slot spent on the elision marker keeps the tail honest.
        let start = latest.lines.len().saturating_sub(budget.saturating_sub(1));
        lines.push(Line::from(Span::styled(
            format!(" … {} earlier lines", latest.truncated + start),
            Style::default()
                .fg(colors::DIM)
                .add_modifier(Modifier::ITALIC),
        )));
        for text in &latest.lines[start..] {
            lines.push(output_line(text));
        }
    } else {
        for text in &latest.lines[start..] {
            lines.push(output_line(text));
        }
    }

    lines
}

/// Colorizes a single output line by rough pass/fail signal words.
fn output_line(text: &str) -> Line<'_> {
    let trimmed = text.trim_start();
    let style = if trimmed.contains("FAILED")
        || trimmed.contains("--- FAIL")
        || trimmed.starts_with("FAIL")
        || trimmed.starts_with("error")
        || trimmed.contains("panicked at")
        || trimmed.starts_with("✗")
    {
        Style::default().fg(colors::FAIL)
    } else if trimmed.ends_with("... ok")
        || trimmed.contains("test result: ok")
        || trimmed.contains("--- PASS")
        || trimmed.starts_with("ok ")
        || trimmed.starts_with("PASS")
        || trimmed.starts_with("✓")
        || (trimmed.contains("passed") && !trimmed.contains("failed"))
    {
        Style::default().fg(colors::PASS)
    } else if trimmed.starts_with("warning") {
        Style::default().fg(colors::RUNNING)
    } else {
        Style::default().fg(colors::TEXT)
    };
    Line::from(Span::styled(text, style))
}
