//! Test runner integration for vim-test style <Leader>t commands.
//!
//! - `<Space>tn` / `:TestNearest` — run the test at/near the cursor
//! - `<Space>tf` / `:TestFile`    — run the current file's tests
//! - `<Space>ta` / `<Space>ts` / `:TestSuite` — run the whole suite
//! - `<Space>tl` / `:TestLast`    — re-run the last test command
//! - `<Space>tv` / `:TestVisit`   — jump back to the last-tested position
//! - `<Space>tt` / `:TestPanel`   — toggle the right-side test panel
//! - `<Space>to` / `:TestOutput`  — raw output in a scratch buffer
//!
//! Commands run as background jobs in the file's own project root (nearest
//! `Cargo.toml` / `package.json` / `go.mod` / pytest marker — resolved per
//! file, so monorepos work without configuring anything). Output streams
//! live into the right-side test panel (`<Space>tt` toggles it; see
//! `test_panel.rs`). Failures also populate the quickfix list silently for
//! `:cn` navigation; `:TestOutput` shows the raw log.
//!
//! Built-in runners: cargo (rust), vitest/jest/bun/node:test/npm (js/ts), pytest
//! (python), go test (go). Other languages configure `[language.test]` in
//! languages.toml. Nearest-test discovery is tree-sitter based (see
//! `nearest.rs`).

mod nearest;
mod runners;

#[cfg(test)]
mod tests;

pub use runners::TestScope;
use runners::{build_test_command, TestContext, TestInvocation};

use crate::editor::Editor;
use std::path::PathBuf;

/// Remembered state of the last test run, for `:TestLast` / `:TestVisit`.
#[derive(Debug, Clone)]
pub struct LastTest {
    pub command: String,
    pub cwd: PathBuf,
    /// Absolute path of the file the test was run from.
    pub file: String,
    /// 0-indexed cursor line at run time.
    pub line: usize,
}

impl Editor {
    /// `<Space>tf` - Run tests for the current file.
    pub fn run_test_file(&mut self) {
        self.run_test(TestScope::File);
    }

    /// `<Space>tn` - Run the nearest test (at/above/below cursor).
    pub fn run_test_nearest(&mut self) {
        self.run_test(TestScope::Nearest);
    }

    /// `<Space>ta` / `<Space>ts` - Run the whole test suite.
    pub fn run_test_all(&mut self) {
        self.run_test(TestScope::Suite);
    }

    /// `<Space>tl` - Re-run the last test command.
    pub fn run_test_last(&mut self) {
        match self.build.last_test.clone() {
            Some(last) => self.spawn_test_job("re-run", &last.command, last.cwd),
            None => self.set_status_message("No previous test command".to_string()),
        }
    }

    /// `<Space>tv` - Jump back to the file/line of the last test run.
    pub fn test_visit(&mut self) {
        let Some(last) = self.build.last_test.clone() else {
            self.set_status_message("No previous test run".to_string());
            return;
        };
        let last_file = PathBuf::from(&last.file);
        let already_there = self
            .buffer()
            .file_path()
            .is_some_and(|p| absolutize(p) == last_file);
        if !already_there {
            if let Err(e) = self.load_file(&last.file) {
                self.set_status_message(format!("Failed to open {}: {}", last.file, e));
                return;
            }
        }
        let line = last.line.min(self.buffer().line_count().saturating_sub(1));
        self.buffer_mut()
            .cursor_mut()
            .set_position(line, crate::unicode::GraphemeCol(0));
        self.buffer_mut().validate_cursor_position();
        self.center_cursor_in_viewport();
    }

    fn run_test(&mut self, scope: TestScope) {
        let Some(file_path) = self.buffer().file_path().map(str::to_string) else {
            self.set_status_message(
                "Buffer has no file - save it before running tests".to_string(),
            );
            return;
        };
        let abs_file = absolutize(&file_path);
        let source = self.buffer().rope().to_string();
        let cursor_line = self.buffer().cursor().line();

        let language = crate::syntax::LanguageRegistry::detect_from_path(&abs_file);
        let lang_registry = crate::language_config::LanguageRegistry::try_get();
        let test_config = lang_registry
            .and_then(|reg| reg.detect(&abs_file))
            .and_then(|cfg| cfg.test.as_ref());

        let ctx = TestContext {
            file: &abs_file,
            source: &source,
            cursor_line,
            language,
            config: test_config,
        };

        match build_test_command(scope, &ctx) {
            Ok(TestInvocation { command, cwd }) => {
                self.build.last_test = Some(LastTest {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    file: abs_file.to_string_lossy().to_string(),
                    line: cursor_line,
                });
                let label = match scope {
                    TestScope::Nearest => "nearest",
                    TestScope::File => "file",
                    TestScope::Suite => "suite",
                };
                self.spawn_test_job(label, &command, cwd);
            }
            Err(msg) => self.set_status_message(msg),
        }
    }

    /// Runs a test command in the background, streaming its output into the
    /// test panel line by line (stdout and stderr interleaved as they
    /// arrive). Opens the panel and supersedes any run still in flight.
    fn spawn_test_job(&mut self, scope_label: &'static str, cmd: &str, cwd: PathBuf) {
        use crate::editor::test_panel::{PendingTest, TestEvent};
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let (tx, rx) = std::sync::mpsc::channel();
        let cmd_owned = cmd.to_string();
        let cwd_for_spawn = cwd.clone();

        std::thread::spawn(move || {
            let mut child = match Command::new("sh")
                .arg("-c")
                .arg(&cmd_owned)
                .current_dir(&cwd_for_spawn)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(TestEvent::Line(format!(
                        "Failed to run '{}': {}",
                        cmd_owned, e
                    )));
                    let _ = tx.send(TestEvent::Finished { success: false });
                    return;
                }
            };

            let stderr_thread = child.stderr.take().map(|stderr| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if tx.send(TestEvent::Line(line)).is_err() {
                            break;
                        }
                    }
                })
            });
            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(TestEvent::Line(line)).is_err() {
                        break;
                    }
                }
            }
            if let Some(handle) = stderr_thread {
                let _ = handle.join();
            }
            let success = child.wait().map(|s| s.success()).unwrap_or(false);
            let _ = tx.send(TestEvent::Finished { success });
        });

        let dir_name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd.to_string_lossy().to_string());
        self.build
            .test_panel
            .start_run(scope_label, cmd.to_string(), cwd.clone());
        self.build.pending_test = Some(PendingTest { receiver: rx });

        // Show where the command runs when it isn't the process cwd — in a
        // monorepo "Running: cargo test" alone would be ambiguous.
        let here = std::env::current_dir().ok();
        if here.as_deref() != Some(cwd.as_path()) {
            self.set_status_message(format!("Running: {} (in {})", cmd, dir_name));
        } else {
            self.set_status_message(format!("Running: {}", cmd));
        }
    }
}

/// Best-effort absolute path: buffer paths may be cwd-relative.
fn absolutize(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p)
    }
}
