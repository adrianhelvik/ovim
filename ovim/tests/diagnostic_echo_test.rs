//! The idle message line echoes the diagnostic under the cursor.
//!
//! Inline EOL virtual text is width-capped, so long messages truncate there.
//! The message line has the whole terminal row to spend: when no status
//! message is active and the cursor sits on a diagnostic line, the renderer
//! echoes the severity tag plus the full first line of the message.
mod helpers;

use helpers::EditorTest;
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

fn diag(line: u32, character: u32, severity: DiagnosticSeverity, message: &str) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position { line, character },
            end: Position {
                line,
                character: character + 1,
            },
        },
        severity: Some(severity),
        message: message.to_string(),
        ..Default::default()
    }
}

/// Render an 80x12 frame and return the bottom (message line) row, ANSI-stripped.
fn message_row(test: &mut EditorTest) -> String {
    let ansi = ovim::ui::render_editor_to_ansi(&mut test.editor, 80, 12).unwrap();
    let plain = ovim::ui::strip_ansi(&ansi);
    plain
        .split('\n')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn message_line_echoes_diagnostic_on_cursor_line() {
    let mut test = EditorTest::new("let x = bad();\nlet y = 2;\n");
    test.editor.set_test_diagnostics(vec![diag(
        0,
        8,
        DiagnosticSeverity::ERROR,
        "cannot find function `bad` in this scope",
    )]);
    test.editor.set_status_message(String::new());

    let row = message_row(&mut test);
    assert!(
        row.starts_with("E: cannot find function `bad` in this scope"),
        "message line should echo the cursor-line diagnostic; got {row:?}"
    );
}

#[test]
fn message_line_stays_empty_off_diagnostic_line() {
    let mut test = EditorTest::new("let x = bad();\nlet y = 2;\n");
    test.editor.set_test_diagnostics(vec![diag(
        0,
        8,
        DiagnosticSeverity::ERROR,
        "cannot find function `bad` in this scope",
    )]);
    test.editor.set_status_message(String::new());
    test.set_cursor(1, 0);

    let row = message_row(&mut test);
    assert!(
        row.trim().is_empty(),
        "no echo expected when the cursor line has no diagnostic; got {row:?}"
    );
}

#[test]
fn status_message_takes_precedence_over_echo() {
    let mut test = EditorTest::new("let x = bad();\n");
    test.editor.set_test_diagnostics(vec![diag(
        0,
        8,
        DiagnosticSeverity::ERROR,
        "cannot find function `bad` in this scope",
    )]);
    test.editor
        .set_status_message("Requesting completions...".to_string());

    let row = message_row(&mut test);
    assert!(
        row.starts_with("Requesting completions..."),
        "an active status message must win over the diagnostic echo; got {row:?}"
    );
}

#[test]
fn echo_includes_source_and_code_and_severity_tag() {
    let mut test = EditorTest::new("let x = 1;\n");
    let mut d = diag(0, 4, DiagnosticSeverity::WARNING, "unused variable: `x`");
    d.source = Some("rustc".to_string());
    d.code = Some(lsp_types::NumberOrString::String("unused_variables".into()));
    test.editor.set_test_diagnostics(vec![d]);
    test.editor.set_status_message(String::new());

    let row = message_row(&mut test);
    assert!(
        row.starts_with("W: unused variable: `x` [rustc unused_variables]"),
        "echo should carry severity tag, source, and code; got {row:?}"
    );
}
