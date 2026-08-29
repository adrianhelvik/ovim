//! Integration tests for shell command features
//!
//! Tests for:
//! - :!cmd - run shell command and display output
//! - :.!cmd - replace current line with command output
//! - :%!cmd - pipe buffer through command
//! - :r !cmd - insert command output
//! - :w !cmd - write buffer to command stdin
//! - % and # expansion in shell commands

mod helpers;

use helpers::EditorTest;
use ovim::editor::InputHandler;

#[test]
fn test_shell_command_echo() {
    let mut test = EditorTest::new("hello world\n");

    // Execute :!echo test — should queue a pending shell command
    // (actual execution happens in the TUI event loop with terminal access)
    InputHandler::execute_command_string(&mut test.editor, "!echo test").unwrap();

    let pending = test.editor.take_pending_shell_command();
    assert!(pending.is_some(), "should have a pending shell command");
    assert_eq!(pending.unwrap().command, "echo test");
}

#[test]
fn terminal_command_preserves_shell_pipeline() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "terminal printf x | cat").unwrap();

    let pending = test
        .editor
        .take_pending_terminal_session()
        .expect(":terminal should own its complete command tail");
    assert_eq!(pending.command.as_deref(), Some("printf x | cat"));
}

#[test]
fn shell_pipeline_is_not_split_as_an_ex_command_chain() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "!printf x | cat").unwrap();

    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert_eq!(pending.command, "printf x | cat");
}

#[test]
fn filter_pipeline_is_not_split_as_an_ex_command_chain() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "%!printf 'second\\nfirst\\n' | sort")
        .unwrap();

    assert_eq!(test.buffer_content(), "first\nsecond\n");
}

#[test]
fn read_pipeline_is_not_split_as_an_ex_command_chain() {
    let mut test = EditorTest::new("first line\n");

    InputHandler::execute_command_string(&mut test.editor, "r !printf inserted | cat").unwrap();

    assert_eq!(test.buffer_content(), "first line\ninserted\n");
}

#[test]
fn read_shell_output_separates_an_unterminated_final_line() {
    let mut test = EditorTest::new("first line");

    InputHandler::execute_command_string(&mut test.editor, "r !printf inserted").unwrap();

    assert_eq!(test.buffer_content(), "first line\ninserted\n");
}

#[test]
fn terminal_queues_interactive_user_shell() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "terminal").unwrap();

    let pending = test
        .editor
        .take_pending_terminal_session()
        .expect(":terminal should queue a session");
    assert_eq!(pending.command, None);
}

#[test]
fn shell_alias_queues_interactive_user_shell() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "shell").unwrap();

    let pending = test
        .editor
        .take_pending_terminal_session()
        .expect(":shell should queue a session");
    assert_eq!(pending.command, None);
}

#[test]
fn term_queues_requested_command() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "term cargo test").unwrap();

    let pending = test
        .editor
        .take_pending_terminal_session()
        .expect(":term should queue a session");
    assert_eq!(pending.command.as_deref(), Some("cargo test"));
}

#[test]
fn terminal_name_must_end_at_a_command_boundary() {
    let mut test = EditorTest::new("hello world\n");

    InputHandler::execute_command_string(&mut test.editor, "terminally").unwrap();

    assert!(test.editor.take_pending_terminal_session().is_none());
    assert!(test
        .editor
        .status_message()
        .contains("Not an editor command"));
}

#[test]
fn test_shell_command_repeat_last() {
    let mut test = EditorTest::new("hello world\n");

    // First command sets last_shell_command
    InputHandler::execute_command_string(&mut test.editor, "!echo first").unwrap();
    let _ = test.editor.take_pending_shell_command();

    // Bare :! should repeat it
    InputHandler::execute_command_string(&mut test.editor, "!").unwrap();
    let pending = test.editor.take_pending_shell_command();
    assert!(pending.is_some(), "bare :! should repeat last command");
    assert_eq!(pending.unwrap().command, "echo first");
}

#[test]
fn test_shell_command_repeat_empty() {
    let mut test = EditorTest::new("hello world\n");

    // Bare :! with no previous command should not queue
    InputHandler::execute_command_string(&mut test.editor, "!").unwrap();
    let pending = test.editor.take_pending_shell_command();
    assert!(
        pending.is_none(),
        "bare :! with no history should not queue"
    );
}

#[test]
fn test_filter_current_line() {
    let mut test = EditorTest::new("hello world\nfoo bar\nbaz qux\n");
    test.editor
        .buffer_mut()
        .cursor_mut()
        .set_position(1, ovim::unicode::GraphemeCol::ZERO); // Middle line

    // Execute :.!tr 'a-z' 'A-Z' to uppercase current line
    InputHandler::execute_command_string(&mut test.editor, ".!tr 'a-z' 'A-Z'").unwrap();

    // Line 1 should be uppercased
    let line = test.editor.buffer().line_text(1).unwrap();
    assert_eq!(
        line.trim(),
        "FOO BAR",
        "Line should be uppercased, got: {}",
        line
    );
}

#[test]
fn test_filter_entire_buffer() {
    let mut test = EditorTest::new("cherry\napple\nbanana\n");

    // Execute :%!sort to sort all lines
    InputHandler::execute_command_string(&mut test.editor, "%!sort").unwrap();

    // Buffer should be sorted
    let content = test.editor.buffer().rope().to_string();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "apple", "First line should be apple");
    assert_eq!(lines[1], "banana", "Second line should be banana");
    assert_eq!(lines[2], "cherry", "Third line should be cherry");
}

#[test]
fn test_filter_entire_buffer_undo_redo_macro_flow() {
    editor_flow_test! {
        content "cherry\napple\nbanana\n";
        step ":%!sort<Enter>" => |test| {
            assert_eq!(test.buffer_content(), "apple\nbanana\ncherry\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "cherry\napple\nbanana\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "apple\nbanana\ncherry\n");
        }
    }
}

#[test]
fn test_read_shell_command() {
    let mut test = EditorTest::new("first line\nsecond line\n");
    test.editor
        .buffer_mut()
        .cursor_mut()
        .set_position(0, ovim::unicode::GraphemeCol::ZERO); // First line

    // Execute :r !echo "inserted"
    InputHandler::execute_command_string(&mut test.editor, "r !echo inserted").unwrap();

    // "inserted" should be somewhere in the buffer (after current line)
    let content = test.editor.buffer().rope().to_string();
    assert!(
        content.contains("inserted"),
        "Buffer should contain 'inserted', got: {}",
        content
    );
}

#[test]
#[ignore = "requires tokio runtime for buffer operations"]
fn test_write_to_shell_command() {
    let mut test = EditorTest::new("hello world\n");

    // Execute :w !cat (should succeed and show line count)
    InputHandler::execute_command_string(&mut test.editor, "w !cat").unwrap();

    // Status should mention lines written
    let status = test.editor.status_message();
    assert!(
        status.contains("written") || status.contains("line"),
        "Status should mention lines written, got: {}",
        status
    );
}

#[test]
fn test_percent_expansion_in_shell() {
    let mut test = EditorTest::new("content\n");

    test.editor
        .buffer_mut()
        .set_file_path("test_file.rs".to_string());

    // :!echo % should expand % to the current filename in the queued command
    InputHandler::execute_command_string(&mut test.editor, "!echo %").unwrap();

    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("test_file.rs"),
        "Expanded command should contain filename, got: {}",
        pending.command
    );
}

#[test]
fn test_percent_tail_modifier() {
    let mut test = EditorTest::new("content\n");
    test.editor
        .buffer_mut()
        .set_file_path("src/main.rs".to_string());

    InputHandler::execute_command_string(&mut test.editor, "!echo %:t").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("main.rs"),
        "Tail should be main.rs, got: {}",
        pending.command
    );
}

#[test]
fn test_percent_head_modifier() {
    let mut test = EditorTest::new("content\n");
    test.editor
        .buffer_mut()
        .set_file_path("src/main.rs".to_string());

    InputHandler::execute_command_string(&mut test.editor, "!echo %:h").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("src"),
        "Head should be src, got: {}",
        pending.command
    );
}

#[test]
fn test_percent_root_modifier() {
    let mut test = EditorTest::new("content\n");
    test.editor
        .buffer_mut()
        .set_file_path("src/main.rs".to_string());

    InputHandler::execute_command_string(&mut test.editor, "!echo %:r").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("src/main"),
        "Root should be src/main, got: {}",
        pending.command
    );
}

#[test]
fn test_percent_extension_modifier() {
    let mut test = EditorTest::new("content\n");
    test.editor
        .buffer_mut()
        .set_file_path("src/main.rs".to_string());

    InputHandler::execute_command_string(&mut test.editor, "!echo %:e").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("rs"),
        "Extension should be rs, got: {}",
        pending.command
    );
}

#[test]
fn test_escaped_percent() {
    let mut test = EditorTest::new("content\n");
    test.editor
        .buffer_mut()
        .set_file_path("test.rs".to_string());

    // :!echo \% — escaped percent should be literal, not expanded
    InputHandler::execute_command_string(&mut test.editor, r"!echo \%").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains('%'),
        "Should contain literal %, got: {}",
        pending.command
    );
    assert!(
        !pending.command.contains("test.rs"),
        "Should NOT contain filename, got: {}",
        pending.command
    );
}

#[test]
fn test_chained_modifiers() {
    let mut test = EditorTest::new("content\n");

    // Set file path with nested directories
    test.editor
        .buffer_mut()
        .set_file_path("src/editor/main.rs".to_string());

    // Test :t:r (tail then root = "main")
    InputHandler::execute_command_string(&mut test.editor, "!echo %:t:r").unwrap();
    let pending = test.editor.take_pending_shell_command().expect("pending");
    assert!(
        pending.command.contains("main"),
        "Tail+root should be main, got: {}",
        pending.command
    );
    assert!(
        !pending.command.contains(".rs"),
        "Should not contain .rs, got: {}",
        pending.command
    );
}

#[test]
#[ignore = "requires tokio runtime for file operations"]
fn test_edit_force_reload() {
    // Create a temp file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("ovim_shell_test_reload.txt");
    std::fs::write(&temp_file, "original content\n").unwrap();

    let mut test = EditorTest::new("placeholder\n");

    // Load the file
    test.editor.load_file(temp_file.to_str().unwrap()).unwrap();

    // Verify content
    let content = test.editor.buffer().rope().to_string();
    assert!(content.contains("original"), "Should have original content");

    // Modify the buffer
    test.editor
        .buffer_mut()
        .insert_text_at(0, ovim::unicode::CharCol::ZERO, "MODIFIED ");
    assert!(test.editor.buffer().is_modified());

    // Execute :e! to force reload
    InputHandler::execute_command_string(&mut test.editor, "e!").unwrap();

    // Buffer should be back to original
    let content = test.editor.buffer().rope().to_string();
    assert!(
        content.contains("original content"),
        "Buffer should contain original content, got: {}",
        content
    );
    assert!(
        !content.contains("MODIFIED"),
        "Buffer should not contain MODIFIED, got: {}",
        content
    );

    // Clean up
    std::fs::remove_file(temp_file).ok();
}
