//! Right-side test panel: live state for `<Space>t` test runs.
//!
//! Test runs stream their output line-by-line into a `TestRun` record shown
//! in a dedicated panel on the right side of the editor (toggled with
//! `<Space>tt` / `:TestPanel`). The panel keeps a short history of runs so
//! you can see at a glance what passed recently.

use crate::editor::Editor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Cap on retained output lines per run. Test logs can be huge; the panel is
/// a tail view and `:TestOutput` has the full log.
const MAX_RUN_LINES: usize = 5_000;
/// Cap on retained run history.
const MAX_RUNS: usize = 10;

/// One event streamed from a running test process.
pub enum TestEvent {
    /// A line of combined stdout/stderr output.
    Line(String),
    /// Process exited (or failed to spawn).
    Finished { success: bool },
}

/// A background test job streaming into the panel.
pub struct PendingTest {
    pub receiver: Receiver<TestEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestRunStatus {
    Running,
    Passed,
    Failed,
    /// Superseded by a newer run before finishing.
    Cancelled,
}

/// A source position reported by a test framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSourceLocation {
    pub path: PathBuf,
    /// 1-indexed line, matching test-runner output and quickfix conventions.
    pub line: usize,
    /// 1-indexed column when the runner reports one.
    pub column: Option<usize>,
}

/// Structured failure data produced by the output adapter layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    pub test_name: Option<String>,
    pub message: String,
    pub location: Option<TestSourceLocation>,
    pub frames: Vec<TestSourceLocation>,
}

/// One test invocation, streamed live and kept in panel history.
pub struct TestRun {
    /// Short human label: "nearest", "file", "suite", "re-run".
    pub scope_label: &'static str,
    pub command: String,
    /// Display name of the working directory (package/project root).
    pub dir_name: String,
    /// Absolute working directory used to resolve runner-relative locations.
    pub cwd: PathBuf,
    pub status: TestRunStatus,
    pub lines: Vec<String>,
    /// Lines dropped from the front once `MAX_RUN_LINES` was exceeded.
    pub truncated: usize,
    pub started: Instant,
    pub duration: Option<Duration>,
    /// Parsed pass/fail summary, e.g. "12 passed, 1 failed".
    pub summary: Option<String>,
    /// Parsed failures; the panel never needs to interpret raw console text.
    pub failures: Vec<TestFailure>,
}

impl TestRun {
    fn push_line(&mut self, line: String) {
        self.lines.push(line);
        if self.lines.len() > MAX_RUN_LINES {
            let excess = self.lines.len() - MAX_RUN_LINES;
            self.lines.drain(..excess);
            self.truncated += excess;
        }
    }

    /// Elapsed time: final duration once finished, wall clock while running.
    pub fn elapsed(&self) -> Duration {
        self.duration.unwrap_or_else(|| self.started.elapsed())
    }
}

#[derive(Default)]
pub struct TestPanelState {
    pub open: bool,
    /// Run history, newest last.
    pub runs: Vec<TestRun>,
}

impl TestPanelState {
    pub fn latest(&self) -> Option<&TestRun> {
        self.runs.last()
    }

    /// Starts tracking a new run; any still-running previous run is marked
    /// cancelled (its channel receiver has been replaced, so its remaining
    /// output is discarded).
    pub(crate) fn start_run(&mut self, scope_label: &'static str, command: String, cwd: PathBuf) {
        for run in &mut self.runs {
            if run.status == TestRunStatus::Running {
                run.status = TestRunStatus::Cancelled;
                run.duration = Some(run.started.elapsed());
            }
        }
        let dir_name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd.to_string_lossy().to_string());
        self.runs.push(TestRun {
            scope_label,
            command,
            dir_name,
            cwd,
            status: TestRunStatus::Running,
            lines: Vec::new(),
            truncated: 0,
            started: Instant::now(),
            duration: None,
            summary: None,
            failures: Vec::new(),
        });
        if self.runs.len() > MAX_RUNS {
            let excess = self.runs.len() - MAX_RUNS;
            self.runs.drain(..excess);
        }
        self.open = true;
    }
}

impl Editor {
    pub fn test_panel(&self) -> &TestPanelState {
        &self.build.test_panel
    }

    pub fn is_test_panel_open(&self) -> bool {
        self.build.test_panel.open
    }

    /// `<Space>tt` / `:TestPanel` - toggle the test panel.
    pub fn toggle_test_panel(&mut self) {
        let panel = &mut self.build.test_panel;
        panel.open = !panel.open;
        if panel.open && panel.runs.is_empty() {
            self.set_status_message(
                "Test panel open. <Space>tn runs the nearest test, <Space>tf the file".to_string(),
            );
        }
    }

    /// Hide the test panel without affecting a test running in the background.
    pub fn close_test_panel(&mut self) {
        self.build.test_panel.open = false;
    }

    /// `<Space>to` / `:TestOutput` - open the raw output of the last
    /// test/make run in a scratch buffer.
    pub fn open_test_output_buffer(&mut self) {
        let Some(output) = self.build.last_make_output.clone() else {
            self.set_status_message("No make/test output available".to_string());
            return;
        };
        let buf = crate::buffer::Buffer::new_from_str(&output);
        let idx = self.push_buffer(buf);
        self.switch_to_buffer(idx);
        self.set_status_message("Make/test output".to_string());
    }

    /// Drains streamed test output. Returns true if a redraw is needed.
    ///
    /// While a run is in flight this always returns true so the elapsed
    /// timer and spinner stay live.
    pub fn poll_pending_test(&mut self) -> bool {
        let Some(pending) = self.build.pending_test.as_ref() else {
            return false;
        };

        let mut new_lines: Vec<String> = Vec::new();
        let mut finished: Option<bool> = None;
        let mut disconnected = false;
        loop {
            match pending.receiver.try_recv() {
                Ok(TestEvent::Line(line)) => new_lines.push(line),
                Ok(TestEvent::Finished { success }) => {
                    finished = Some(success);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if let Some(run) = self.build.test_panel.runs.last_mut() {
            for line in new_lines {
                run.push_line(line);
            }
        }

        if let Some(success) = finished {
            self.build.pending_test = None;
            self.finish_test_run(success);
        } else if disconnected {
            self.build.pending_test = None;
            self.finish_test_run(false);
        }
        true
    }

    fn finish_test_run(&mut self, success: bool) {
        let Some(run) = self.build.test_panel.runs.last_mut() else {
            return;
        };
        run.status = if success {
            TestRunStatus::Passed
        } else {
            TestRunStatus::Failed
        };
        run.duration = Some(run.started.elapsed());
        let output = run.lines.join("\n");
        run.summary = extract_summary(&run.lines);
        run.failures = parse_test_failures(&output, &run.cwd);
        let elapsed = format_duration(run.duration.unwrap_or_default());
        let summary = run.summary.clone();
        let command = run.command.clone();

        // Keep :TestOutput working on the full log.
        self.build.last_make_output = Some(output.clone());

        // Populate the quickfix list silently so :cn / <Space>tv style
        // navigation works, but let the panel be the visible surface
        // (no auto-jump, no bottom window).
        let entries = run
            .failures
            .iter()
            .filter_map(failure_quickfix_entry)
            .collect::<Vec<_>>();
        if !entries.is_empty() {
            self.set_quickfix_list(entries, format!("test {}", command));
        }

        let status = match (success, summary) {
            (true, Some(s)) => format!("✓ Tests passed in {} ({})", elapsed, s),
            (true, None) => format!("✓ Tests passed in {}", elapsed),
            (false, Some(s)) => format!("✗ Tests failed in {} ({})", elapsed, s),
            (false, None) => format!("✗ Tests failed in {}", elapsed),
        };
        self.set_status_message(status);
    }
}

fn failure_quickfix_entry(failure: &TestFailure) -> Option<crate::editor::QuickfixEntry> {
    let location = failure.location.as_ref()?;
    Some(crate::editor::QuickfixEntry::error(
        Some(location.path.clone()),
        location.line,
        location.column.unwrap_or(0),
        failure.message.clone(),
    ))
}

/// Converts framework-specific human output into a stable result model.
/// Built-in runners can move to machine-readable reporters later without
/// changing panel rendering or source navigation.
pub fn parse_test_failures(output: &str, cwd: &Path) -> Vec<TestFailure> {
    let mut failures = Vec::new();

    // Preserve compiler errors and Rust panic support already understood by
    // the quickfix parser, then supplement it with runtime stack formats.
    for entry in crate::commands::parse_compiler_output(output) {
        if entry.entry_type != crate::editor::QuickfixEntryType::Error {
            continue;
        }
        let Some(path) = entry.filename else { continue };
        // JavaScript stack frames are parsed below after removing their
        // function-name/parenthesis wrapper. The generic compiler parser
        // otherwise mistakes that wrapper for part of the filename.
        if path.to_string_lossy().starts_with("at ") {
            continue;
        }
        let location = TestSourceLocation {
            path: resolve_test_path(cwd, &path),
            line: entry.lnum,
            column: (entry.col > 0).then_some(entry.col),
        };
        push_failure(&mut failures, location, entry.text);
    }

    let lines: Vec<&str> = output.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let parsed = parse_js_frame(trimmed).or_else(|| parse_pytest_or_go_location(trimmed));
        let Some((path, line, column)) = parsed else {
            continue;
        };
        if path
            .components()
            .any(|part| part.as_os_str() == "node_modules")
        {
            continue;
        }
        let location = TestSourceLocation {
            path: resolve_test_path(cwd, &path),
            line,
            column,
        };
        let message = nearby_failure_message(&lines, index);
        push_failure(&mut failures, location, message);
    }
    failures
}

fn resolve_test_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn push_failure(failures: &mut Vec<TestFailure>, location: TestSourceLocation, message: String) {
    if let Some(existing) = failures.iter_mut().find(|failure| {
        failure
            .location
            .as_ref()
            .is_some_and(|candidate| candidate == &location)
    }) {
        if (existing.message.is_empty()
            || existing.message.starts_with("in ")
            || existing.message == ")")
            && !message.is_empty()
        {
            existing.message = message;
        }
        return;
    }
    failures.push(TestFailure {
        test_name: None,
        message,
        location: Some(location.clone()),
        frames: vec![location],
    });
}

/// JavaScript stacks: `at name (path:line:col)` or `at path:line:col`.
fn parse_js_frame(line: &str) -> Option<(PathBuf, usize, Option<usize>)> {
    let frame = line.strip_prefix("at ")?;
    let location = frame
        .rsplit_once('(')
        .map(|(_, value)| value)
        .unwrap_or(frame)
        .trim_end_matches(')');
    parse_path_line_column(location, true)
}

/// pytest: `tests/test_x.py:12: in test_x`; Go: `file_test.go:12: message`.
fn parse_pytest_or_go_location(line: &str) -> Option<(PathBuf, usize, Option<usize>)> {
    let (path, line_number, column) = parse_path_line_column(line, false)?;
    let extension = path.extension()?.to_string_lossy();
    matches!(
        extension.as_ref(),
        "py" | "go" | "rs" | "js" | "jsx" | "ts" | "tsx"
    )
    .then_some((path, line_number, column))
}

fn parse_path_line_column(
    text: &str,
    require_column: bool,
) -> Option<(PathBuf, usize, Option<usize>)> {
    let mut parts = text.rsplitn(3, ':');
    let tail = parts.next()?.trim();
    let middle = parts.next()?.trim();
    let head = parts.next()?.trim();

    if let (Ok(column), Ok(line)) = (tail.parse::<usize>(), middle.parse::<usize>()) {
        return Some((PathBuf::from(head), line, Some(column)));
    }
    if require_column {
        return None;
    }

    // The final field is descriptive text rather than a column.
    let line = middle.parse::<usize>().ok()?;
    Some((PathBuf::from(head), line, None))
}

fn nearby_failure_message(lines: &[&str], location_index: usize) -> String {
    lines[..location_index]
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("at ")
                && !line.starts_with("File ")
                && !line.contains("Traceback")
        })
        .unwrap_or("test failed")
        .trim_start_matches("E   ")
        .to_string()
}

/// Human-readable duration: "0.4s", "12.3s", "2m 05s".
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// Extracts a pass/fail summary from runner output, aggregating across
/// multiple summary lines (cargo prints one per target).
///
/// Understands cargo ("N passed; M failed"), pytest/vitest/jest style
/// ("N passed", "M failed") and falls back to None when nothing matches.
pub fn extract_summary(lines: &[String]) -> Option<String> {
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut found = false;

    for line in lines {
        // cargo: "test result: ok. 39 passed; 0 failed; 0 ignored; ..."
        if let Some(rest) = line.trim_start().strip_prefix("test result:") {
            if let (Some(p), Some(f)) =
                (count_before(rest, " passed"), count_before(rest, " failed"))
            {
                passed += p;
                failed += f;
                found = true;
            }
            continue;
        }
        // pytest: "==== 1 failed, 12 passed in 0.21s ====" (also matches
        // vitest/jest "Tests  1 failed | 12 passed (13)" loosely)
        let is_summary_line = (line.contains("passed") || line.contains("failed"))
            && (line.contains("====") || line.trim_start().starts_with("Tests"));
        if is_summary_line {
            if let Some(p) = count_before(line, " passed") {
                passed += p;
                found = true;
            }
            if let Some(f) = count_before(line, " failed") {
                failed += f;
                found = true;
            }
        }
    }

    if !found {
        return None;
    }
    if failed > 0 {
        Some(format!("{} passed, {} failed", passed, failed))
    } else {
        Some(format!("{} passed", passed))
    }
}

/// The number immediately preceding `marker` in `s`, e.g.
/// `count_before("12 passed in", " passed")` → `Some(12)`.
fn count_before(s: &str, marker: &str) -> Option<usize> {
    let idx = s.find(marker)?;
    let before = &s[..idx];
    let num: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    num.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn summary_cargo_aggregates_multiple_targets() {
        let out = lines(&[
            "running 3 tests",
            "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out",
            "running 2 tests",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out",
        ]);
        assert_eq!(extract_summary(&out).as_deref(), Some("4 passed, 1 failed"));
    }

    #[test]
    fn summary_pytest() {
        let out = lines(&["========= 1 failed, 12 passed in 0.21s ========="]);
        assert_eq!(
            extract_summary(&out).as_deref(),
            Some("12 passed, 1 failed")
        );
    }

    #[test]
    fn summary_pytest_all_passing() {
        let out = lines(&["========= 12 passed in 0.21s ========="]);
        assert_eq!(extract_summary(&out).as_deref(), Some("12 passed"));
    }

    #[test]
    fn summary_vitest() {
        let out = lines(&[" Tests  1 failed | 12 passed (13)"]);
        assert_eq!(
            extract_summary(&out).as_deref(),
            Some("12 passed, 1 failed")
        );
    }

    #[test]
    fn summary_absent_for_unrecognized_output() {
        let out = lines(&["make: *** [all] Error 1"]);
        assert_eq!(extract_summary(&out), None);
        // "passed"/"failed" in ordinary prose must not count.
        let prose = lines(&["the guard failed to trip"]);
        assert_eq!(extract_summary(&prose), None);
    }

    #[test]
    fn parses_rust_panic_as_structured_failure() {
        let failures = parse_test_failures(
            "thread 'parser' panicked at src/parser.rs:42:7:\nassertion failed",
            Path::new("/workspace/project"),
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].location,
            Some(TestSourceLocation {
                path: PathBuf::from("/workspace/project/src/parser.rs"),
                line: 42,
                column: Some(7),
            })
        );
    }

    #[test]
    fn parses_pytest_failure_location_and_message() {
        let failures = parse_test_failures(
            "E   AssertionError: expected 2\ntests/test_math.py:18: in test_add",
            Path::new("/workspace/project"),
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].message, "AssertionError: expected 2");
        assert_eq!(failures[0].location.as_ref().unwrap().line, 18);
        assert_eq!(failures[0].location.as_ref().unwrap().column, None);
    }

    #[test]
    fn parses_javascript_stack_frame() {
        let failures = parse_test_failures(
            "AssertionError: expected true\n    at check (/workspace/project/src/a.test.ts:9:13)",
            Path::new("/workspace/project"),
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].message, "AssertionError: expected true");
        assert_eq!(failures[0].location.as_ref().unwrap().line, 9);
        assert_eq!(failures[0].location.as_ref().unwrap().column, Some(13));
    }

    #[test]
    fn parses_go_failure_location_relative_to_package() {
        let failures = parse_test_failures(
            "expected 3, got 4\n    calculator_test.go:27: mismatch",
            Path::new("/workspace/project/pkg/calc"),
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].location.as_ref().unwrap().path,
            PathBuf::from("/workspace/project/pkg/calc/calculator_test.go")
        );
        assert_eq!(failures[0].location.as_ref().unwrap().line, 27);
    }

    #[test]
    fn run_line_cap_keeps_tail() {
        let mut state = TestPanelState::default();
        state.start_run("file", "cargo test".into(), "ovim".into());
        let run = state.runs.last_mut().unwrap();
        for i in 0..(MAX_RUN_LINES + 10) {
            run.push_line(format!("line {}", i));
        }
        assert_eq!(run.lines.len(), MAX_RUN_LINES);
        assert_eq!(run.truncated, 10);
        assert_eq!(
            run.lines.last().unwrap(),
            &format!("line {}", MAX_RUN_LINES + 9)
        );
    }

    #[test]
    fn starting_a_run_cancels_the_previous_running_one() {
        let mut state = TestPanelState::default();
        state.start_run("file", "cargo test a".into(), "ovim".into());
        state.start_run("nearest", "cargo test b".into(), "ovim".into());
        assert_eq!(state.runs.len(), 2);
        assert_eq!(state.runs[0].status, TestRunStatus::Cancelled);
        assert_eq!(state.runs[1].status, TestRunStatus::Running);
        assert!(state.open);
    }

    #[test]
    fn closing_the_panel_does_not_cancel_the_running_test() {
        let mut editor = Editor::with_content("");
        editor
            .build
            .test_panel
            .start_run("nearest", "cargo test example".into(), "ovim".into());

        editor.close_test_panel();

        assert!(!editor.is_test_panel_open());
        assert_eq!(
            editor.test_panel().latest().unwrap().status,
            TestRunStatus::Running
        );
    }

    #[test]
    fn run_history_is_capped() {
        let mut state = TestPanelState::default();
        for i in 0..(MAX_RUNS + 5) {
            state.start_run("file", format!("cargo test {}", i), "ovim".into());
        }
        assert_eq!(state.runs.len(), MAX_RUNS);
        assert_eq!(
            state.runs.last().unwrap().command,
            format!("cargo test {}", MAX_RUNS + 4)
        );
    }

    #[test]
    fn format_duration_styles() {
        assert_eq!(format_duration(Duration::from_millis(400)), "0.4s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
    }
}
