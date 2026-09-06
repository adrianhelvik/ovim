//! Parity tests for the headless API / CLI `exec` command path.
//!
//! Regression coverage for a real divergence: the headless `exec` endpoint
//! (`ApiRequest::ExecuteCommand`) used to call the standard commands module
//! directly, which only knows `:w`, `:q`, `:set`, etc. Substitute (`:s`),
//! global (`:g`), and range commands live in the interactive command handler,
//! so `ovim exec ':%s/a/b/'` returned "Not an editor command" even though the
//! identical keys worked when typed at the `:` prompt.
//!
//! `InputHandler::execute_command_api` is the function the event loop now calls
//! for that endpoint, so exercising it directly is the closest unit-level proxy
//! for "what happens when a headless client sends a command".

mod helpers;

use helpers::EditorTest;
use ovim::command_result::CommandResult;
use ovim::editor::InputHandler;

fn exec(test: &mut EditorTest, command: &str) -> CommandResult {
    InputHandler::execute_command_api(&mut test.editor, command)
}

#[test]
fn exec_terminal_rejects_an_unattached_interactive_session() {
    let mut test = EditorTest::new("text");

    let result = exec(&mut test, "terminal");

    assert_error(&result, ":terminal");
    assert!(test.editor.take_pending_terminal_session().is_none());
}

#[test]
fn exec_openwin_rejects_a_frontend_without_windows() {
    let mut test = EditorTest::new("text");

    // A headless client has no window server, so the queued request must be
    // consumed and reported rather than left for a later frontend to run.
    for command in ["openwin", "openwin /tmp"] {
        let result = exec(&mut test, command);

        assert_error(&result, command);
        assert!(test.editor.take_pending_window_open().is_none());
    }
}

fn assert_success(result: &CommandResult, ctx: &str) {
    assert!(
        matches!(result, CommandResult::Success(_)),
        "expected success for {ctx}, got {result:?}"
    );
}

fn assert_error(result: &CommandResult, ctx: &str) {
    assert!(
        matches!(result, CommandResult::Error(_)),
        "expected error for {ctx}, got {result:?}"
    );
}

// ---- The core bug: substitute over the exec path ----

#[test]
fn exec_substitute_applies_and_reports_success() {
    let mut test = EditorTest::new("the quick brown fox");
    let result = exec(&mut test, "%s/quick/slow/g");

    assert_eq!(test.buffer_content(), "the slow brown fox\n");
    assert_success(&result, "%s/quick/slow/g");
}

#[test]
fn exec_substitute_global_flag_replaces_all_occurrences() {
    let mut test = EditorTest::new("aaa aaa aaa");
    exec(&mut test, "%s/aaa/b/g");
    assert_eq!(test.buffer_content(), "b b b\n");
}

#[test]
fn exec_substitute_without_g_replaces_first_only() {
    let mut test = EditorTest::new("aaa aaa aaa");
    exec(&mut test, "%s/aaa/b/");
    assert_eq!(test.buffer_content(), "b aaa aaa\n");
}

#[test]
fn exec_substitute_respects_line_range() {
    let mut test = EditorTest::new("x\nx\nx\nx");
    exec(&mut test, "2,3s/x/y/g");
    assert_eq!(test.buffer_content(), "x\ny\ny\nx\n");
}

// ---- Other commands that only exist in the interactive handler ----

#[test]
fn exec_global_delete_matching_lines() {
    let mut test = EditorTest::new("keep\nfoo one\nkeep2\nfoo two");
    exec(&mut test, "%g/foo/d");
    assert_eq!(test.buffer_content(), "keep\nkeep2\n");
}

#[test]
fn exec_ranged_delete() {
    let mut test = EditorTest::new("a\nb\nc\nd");
    exec(&mut test, "2,3d");
    assert_eq!(test.buffer_content(), "a\nd\n");
}

#[test]
fn exec_undo_and_redo_mutate_the_live_buffer() {
    let mut test = EditorTest::new("alpha beta");
    test.keys("wx");
    assert_eq!(test.buffer_content(), "alpha eta\n");

    assert_success(&exec(&mut test, "undo"), "undo");
    assert_eq!(test.buffer_content(), "alpha beta\n");

    assert_success(&exec(&mut test, "redo"), "redo");
    assert_eq!(test.buffer_content(), "alpha eta\n");
}

#[test]
fn exec_accepts_vim_undo_and_redo_abbreviations() {
    let mut test = EditorTest::new("abc");
    test.press('x');

    assert_success(&exec(&mut test, "u"), "u");
    assert_eq!(test.buffer_content(), "abc\n");

    assert_success(&exec(&mut test, "red"), "red");
    assert_eq!(test.buffer_content(), "bc\n");
}

// ---- The standard-command contract must be preserved ----

#[test]
fn exec_standard_command_still_returns_structured_success() {
    let mut test = EditorTest::new("hello");
    let result = exec(&mut test, "set number");
    assert_success(&result, "set number");
}

#[test]
fn exec_unknown_command_still_errors() {
    // Agents rely on an error response to detect typos — a genuinely unknown
    // command must not be silently swallowed as success.
    let mut test = EditorTest::new("hello world");
    let result = exec(&mut test, "notacommand");

    assert_error(&result, "notacommand");
    assert_eq!(
        test.buffer_content(),
        "hello world\n",
        "buffer must be untouched"
    );
}

#[test]
fn exec_invalid_substitute_maps_vim_error_to_api_error() {
    // Vim reports substitute syntax errors on the status line as "E146: ...".
    // The API should surface that as an error, not a success, and leave the
    // buffer unchanged.
    let mut test = EditorTest::new("hello world");
    let result = exec(&mut test, "s/hello"); // missing replacement/closing delimiter

    assert_error(&result, "s/hello");
    assert_eq!(test.buffer_content(), "hello world\n");
}

// ---- The invariant that keeps the two paths from drifting again ----

/// The whole point of the fix: a command sent through the headless `exec` path
/// must produce the same buffer as the identical command typed at the `:`
/// prompt. If someone adds an interactive-only command later, this catches the
/// resulting divergence.
#[test]
fn exec_matches_interactive_command_line() {
    let cases = [
        ("the quick brown fox", "%s/quick/slow/g"),
        ("aaa aaa aaa", "%s/aaa/b/"),
        ("x\nx\nx\nx", "2,3s/x/y/g"),
        ("keep\nfoo one\nkeep2\nfoo two", "%g/foo/d"),
        ("a\nb\nc\nd", "2,3d"),
    ];

    for (content, command) in cases {
        let mut via_api = EditorTest::new(content);
        exec(&mut via_api, command);

        let mut via_prompt = EditorTest::new(content);
        via_prompt.command(command);

        assert_eq!(
            via_api.buffer_content(),
            via_prompt.buffer_content(),
            "headless `exec` diverged from interactive `:{command}` on input {content:?}"
        );
    }
}
