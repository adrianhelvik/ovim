mod helpers;

use helpers::EditorTest;
use ovim::editor::InputHandler;
use ovim_core::KeyCode;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn relative_path(from: &std::path::Path, to: &std::path::Path) -> std::path::PathBuf {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();

    let common_len = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = std::path::PathBuf::new();
    for _ in common_len..from_components.len() {
        out.push("..");
    }
    for component in &to_components[common_len..] {
        out.push(component.as_os_str());
    }
    out
}

fn tilde_workspace_path(file_name: &str) -> (String, std::path::PathBuf) {
    let home = dirs::home_dir().expect("HOME should be available");
    let cwd = std::env::current_dir().expect("cwd should be available");
    let rel = relative_path(&home, &cwd);
    let test_dir = cwd.join("target").join("command_mode_tests");
    fs::create_dir_all(&test_dir).expect("create test dir");
    let rel = rel.to_string_lossy().replace('\\', "/");
    let tilde_prefix = if rel.is_empty() {
        "~".to_string()
    } else {
        format!("~/{}", rel)
    };
    (
        format!("{}/target/command_mode_tests/{}", tilde_prefix, file_name),
        test_dir.join(file_name),
    )
}

/// Test entering command mode with :
#[test]
fn test_enter_command_mode() {
    let mut test = EditorTest::new("test\n");

    test.press(':');

    test.assert_mode(ovim::mode::Mode::Command);
}

#[tokio::test]
async fn test_browser_command_from_normal_context_opens_a_browser_tab() {
    let (browser, mut host) = ovim_core::browser::browser_channel();
    let editor = ovim::editor::Editor::with_content("test\n")
        .with_services(ovim::editor::EditorServices::default().with_browser(browser));
    let mut test = EditorTest { editor };

    test.keys(":browser<Enter>");

    test.assert_mode(ovim::mode::Mode::Normal);
    let host_task = tokio::spawn(async move {
        let request = host.recv().await.expect("browser start request");
        assert_eq!(
            request.command(),
            &ovim_core::browser::BrowserCommand::Start { url: None }
        );
        request.respond(Ok(ovim_core::browser::BrowserResponse::Session(
            ovim_core::browser::BrowserSession {
                session_id: "browser-1".into(),
                url: String::new(),
                title: String::new(),
                visible: false,
                loading: false,
                document_id: 0,
            },
        )));
    });
    test.editor.dispatch_pending_intents().await;
    host_task.await.unwrap();
}

#[test]
fn test_browser_command_name_completes_in_normal_context() {
    let mut test = EditorTest::new("test\n");

    test.keys(":bro<Tab>");

    assert_eq!(test.editor.command_line(), "browser");
}

/// Test :q command
#[test]
fn test_command_quit() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("q");
    test.press_enter();

    // Editor should be marked to quit
    assert!(test.editor.should_quit());
}

/// Test :w command
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_write() {
    let file_name = format!("ovim_test_write_{}.txt", unique_test_id());
    let file_path = std::env::temp_dir().join(file_name);
    let file_path = file_path.to_string_lossy().to_string();

    let mut test = EditorTest::new("content\n");
    test.set_file_path(file_path.clone());

    test.press(':');
    test.type_text("w");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
    let _ = fs::remove_file(file_path);
}

/// Test :w ~/file expands tilde to home directory
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_write_expands_tilde_path() {
    let file_name = format!("ovim_test_write_tilde_{}.txt", unique_test_id());
    let (tilde_path, expanded_path) = tilde_workspace_path(&file_name);

    let _ = fs::remove_file(&expanded_path);

    let mut test = EditorTest::new("tilde-write\n");
    test.press(':');
    test.type_text(&format!("w {}", tilde_path));
    test.press_enter();

    assert!(
        expanded_path.exists(),
        "Expected file at expanded path: {}",
        expanded_path.display()
    );
    let content = fs::read_to_string(&expanded_path).expect("read written file");
    assert_eq!(content, "tilde-write\n");

    let literal_path = std::path::Path::new(&tilde_path);
    assert!(
        !literal_path.exists(),
        "Should not create a literal '~' path: {}",
        literal_path.display()
    );

    fs::remove_file(&expanded_path).ok();
}

/// Test :w <filename> requests diagnostics refresh after path transition (save-as)
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_write_as_requests_diagnostics_refresh() {
    let file_name = format!("ovim_test_write_as_{}.txt", unique_test_id());
    let path = std::env::temp_dir().join(file_name);
    let path_str = path.to_string_lossy().to_string();

    let mut test = EditorTest::new("content\n");
    // Seed stale diagnostics to ensure save-as path transition invalidates them.
    test.editor
        .set_test_diagnostics(vec![lsp_types::Diagnostic::default()]);

    test.press(':');
    test.type_text(&format!("w {}", path_str));
    test.press_enter();

    assert!(
        test.editor.take_diagnostics_refresh_request(),
        "Expected save-as to request diagnostics refresh"
    );

    let _ = std::fs::remove_file(path);
}

/// Test :wq command
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_write_quit() {
    let file_name = format!("ovim_test_wq_{}.txt", unique_test_id());
    let file_path = std::env::temp_dir().join(file_name);
    let file_path = file_path.to_string_lossy().to_string();

    let mut test = EditorTest::new("content\n");
    test.set_file_path(file_path.clone());

    test.press(':');
    test.type_text("wq");
    test.press_enter();

    assert!(test.editor.should_quit());
    let _ = fs::remove_file(file_path);
}

/// Test :q! command (force quit)
#[test]
fn test_command_force_quit() {
    let mut test = EditorTest::new("test\n");

    // Make a change
    test.press('i');
    test.type_text("change");
    test.press_esc();

    test.press(':');
    test.type_text("q!");
    test.press_enter();

    assert!(test.editor.should_quit());
}

/// Test :e command (edit file)
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_edit_file() {
    // Create the file first
    let file_name = format!("ovim_test_edit_{}.txt", unique_test_id());
    let file_path = std::env::temp_dir().join(file_name);
    std::fs::write(&file_path, "new content\n").unwrap();
    let file_path_str = file_path.to_string_lossy().to_string();

    let mut test = EditorTest::new("old\n");

    test.press(':');
    test.type_text(&format!("e {}", file_path_str));
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);

    // Clean up
    std::fs::remove_file(file_path).ok();
}

/// Test :r ~/file expands tilde before reading
#[test]
fn test_command_read_expands_tilde_path() {
    let file_name = format!("ovim_test_read_tilde_{}.txt", unique_test_id());
    let (tilde_path, expanded_path) = tilde_workspace_path(&file_name);
    fs::write(&expanded_path, "from-home\n").expect("write read source");

    let mut test = EditorTest::new("start\n");
    test.press(':');
    test.type_text(&format!("r {}", tilde_path));
    test.press_enter();

    assert_eq!(test.buffer_content(), "start\nfrom-home\n");

    fs::remove_file(&expanded_path).ok();
}

/// Test :s command (substitute)
#[test]
fn test_command_substitute() {
    let mut test = EditorTest::new("hello world\n");

    test.press(':');
    test.type_text("s/hello/goodbye");
    test.press_enter();

    assert!(test.buffer_content().contains("goodbye"));
}

/// Test :s with g flag (global on line)
#[test]
fn test_command_substitute_global() {
    let mut test = EditorTest::new("foo foo foo\n");

    test.press(':');
    test.type_text("s/foo/bar/g");
    test.press_enter();

    assert_eq!(test.buffer_content(), "bar bar bar\n");
}

/// Test :%s command (substitute all lines)
#[test]
fn test_command_substitute_all_lines() {
    let mut test = EditorTest::new("hello\nworld\nhello\n");

    test.press(':');
    test.type_text("%s/hello/hi");
    test.press_enter();

    let content = test.buffer_content();
    assert!(content.contains("hi"));
    assert!(!content.contains("hello") || content.matches("hello").count() == 1);
}

#[test]
fn test_command_substitute_confirm_yes_undo_redo_macro_flow() {
    editor_flow_test! {
        content "foo\n";
        step ":%s/foo/bar/c<CR>" => |test| {
            test.assert_mode(ovim::mode::Mode::SubstituteConfirm);
            assert_eq!(test.buffer_content(), "foo\n");
        }
        step "y" => |test| {
            test.assert_mode(ovim::mode::Mode::Normal);
            assert_eq!(test.buffer_content(), "bar\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "foo\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "bar\n");
        }
    }
}

#[test]
fn test_command_substitute_confirm_all_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "foo foo\n";
        step ":%s/foo/bar/gc<CR>" => |test| {
            test.assert_mode(ovim::mode::Mode::SubstituteConfirm);
            assert_eq!(test.buffer_content(), "foo foo\n");
        }
        step "a" => |test| {
            test.assert_mode(ovim::mode::Mode::Normal);
            assert_eq!(test.buffer_content(), "bar bar\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "bar foo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "foo foo\n");
        }
    }
}

/// Test :d command (delete line)
#[test]
fn test_command_delete() {
    let mut test = EditorTest::new("line1\nline2\nline3\n");

    test.press(':');
    test.type_text("2d");
    test.press_enter();

    let content = test.buffer_content();
    assert!(!content.contains("line2"));
}

/// Test :y command (yank)
#[test]
fn test_command_yank() {
    let mut test = EditorTest::new("yank me\nother\n");

    test.press(':');
    test.type_text("y");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

#[test]
fn test_command_copy_missing_mark_reports_e20() {
    let mut test = EditorTest::new("a\nb\nc\n");

    InputHandler::execute_command_string(&mut test.editor, "1 copy 'z").unwrap();

    assert_eq!(test.editor.status_message(), "E20: Mark not set");
}

#[test]
fn test_command_move_missing_mark_reports_e20() {
    let mut test = EditorTest::new("a\nb\nc\n");

    InputHandler::execute_command_string(&mut test.editor, "%move 'z").unwrap();

    assert_eq!(test.editor.status_message(), "E20: Mark not set");
}

/// Test :set command
#[test]
fn test_command_set() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("set number");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test escaping from command mode
#[test]
fn test_escape_command_mode() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("incomplete command");
    test.press_esc();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test backspace in command mode
#[test]
fn test_backspace_in_command_mode() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("wrong");
    // Backspace 5 times to delete "wrong"
    test.press_backspace();
    test.press_backspace();
    test.press_backspace();
    test.press_backspace();
    test.press_backspace();
    test.type_text("q");
    test.press_enter();

    assert!(test.editor.should_quit());
}

/// Test command-line cursor movement/editing with arrow keys
#[test]
fn test_command_line_left_right_home_end_delete() {
    let mut test = EditorTest::new("test\n");
    test.press(':');
    test.type_text("echo");
    assert_eq!(test.editor.command_line(), "echo");
    assert_eq!(test.editor.command_cursor(), 4);

    // Move to middle and insert
    test.press_key(KeyCode::Left);
    test.press_key(KeyCode::Left);
    test.press('X');
    assert_eq!(test.editor.command_line(), "ecXho");
    assert_eq!(test.editor.command_cursor(), 3);

    // Home inserts at beginning
    test.press_key(KeyCode::Home);
    test.press('!');
    assert_eq!(test.editor.command_line(), "!ecXho");
    assert_eq!(test.editor.command_cursor(), 1);

    // End + Left + Delete removes one char at cursor
    test.press_key(KeyCode::End);
    test.press_key(KeyCode::Left);
    test.press_key(KeyCode::Delete);
    assert_eq!(test.editor.command_line(), "!ecXh");
}

/// Test that '.' and '/' are inserted correctly while editing in command mode
#[test]
fn test_command_line_allows_dot_and_slash_while_editing() {
    let mut test = EditorTest::new("test\n");
    test.press(':');
    test.type_text("e foo");

    // Move before "oo" and insert "./"
    test.press_key(KeyCode::Left);
    test.press_key(KeyCode::Left);
    test.press('.');
    test.press('/');

    assert_eq!(test.editor.command_line(), "e f./oo");
}

/// Test :! command (shell command)
#[test]
fn test_command_shell() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("!echo hello");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :help command
#[test]
fn test_command_help() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("help");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :version command
#[test]
fn test_command_version() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("version");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test line number navigation (:42)
#[test]
fn test_command_line_number() {
    let mut test = EditorTest::new("l1\nl2\nl3\nl4\nl5\n");

    test.press(':');
    test.type_text("3");
    test.press_enter();

    test.assert_cursor(2, 0);
}

/// Test :$ (go to last line)
#[test]
fn test_command_dollar_last_line() {
    let mut test = EditorTest::new("l1\nl2\nl3\nl4\nl5\n");

    test.press(':');
    test.type_text("$");
    test.press_enter();

    // Should be on last line
    assert!(test.cursor().0 >= 3);
}

/// Test range delete (:1,3d)
#[test]
fn test_command_range_delete() {
    let mut test = EditorTest::new("l1\nl2\nl3\nl4\nl5\n");

    test.press(':');
    test.type_text("1,3d");
    test.press_enter();

    let content = test.buffer_content();
    assert!(!content.contains("l1"));
    assert!(!content.contains("l2"));
    assert!(!content.contains("l3"));
    assert!(content.contains("l4"));
}

#[test]
fn test_command_range_delete_undo_redo_macro_flow() {
    editor_flow_test! {
        content "l1\nl2\nl3\nl4\nl5\n";
        step ":1,3d<Enter>" => |test| {
            assert_eq!(test.buffer_content(), "l4\nl5\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "l1\nl2\nl3\nl4\nl5\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "l4\nl5\n");
        }
    }
}

#[test]
fn test_command_sort_undo_redo_macro_flow() {
    editor_flow_test! {
        content "cherry\napple\nbanana\n";
        step ":1,3sort<Enter>" => |test| {
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
fn test_command_copy_undo_redo_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\nthree\n";
        step ":1,2copy 1<Enter>" => |test| {
            assert_eq!(test.buffer_content(), "one\none\ntwo\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\nthree\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "one\none\ntwo\ntwo\nthree\n");
        }
    }
}

#[test]
fn test_command_move_undo_redo_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\nthree\n";
        step ":1,1move 2<Enter>" => |test| {
            assert_eq!(test.buffer_content(), "two\none\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\nthree\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "two\none\nthree\n");
        }
    }
}

/// Test range yank (:1,2y)
#[test]
fn test_command_range_yank() {
    let mut test = EditorTest::new("l1\nl2\nl3\n");

    test.press(':');
    test.type_text("1,2y");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :split command
#[test]
fn test_command_split() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("split");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :vsplit command
#[test]
fn test_command_vsplit() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("vsplit");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :tabnew command
#[test]
fn test_command_tabnew() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("tabnew");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_command_tabnew_nonexistent_file_updates_current_file_register() {
    let mut test = EditorTest::new("test\n");
    test.set_file_path("existing.txt".to_string());

    let file_name = format!("ovim_test_tabnew_nonexistent_{}.txt", unique_test_id());
    let file_path = std::env::temp_dir().join(file_name);
    let file_path = file_path.to_string_lossy().to_string();

    test.press(':');
    test.type_text(&format!("tabnew {}", file_path));
    test.press_enter();

    assert_eq!(test.get_register_content('%'), Some(file_path));
    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :source command
#[test]
fn test_command_source() {
    let mut test = EditorTest::new("test\n");
    let file_name = format!("ovim_test_source_{}.vim", unique_test_id());
    let file_path = std::env::temp_dir().join(file_name);
    let file_path = file_path.to_string_lossy().to_string();

    test.press(':');
    test.type_text(&format!("source {}", file_path));
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test empty command (just pressing Enter)
#[test]
fn test_command_empty() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test command with leading/trailing spaces
#[test]
fn test_command_with_spaces() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("  q  ");
    test.press_enter();

    assert!(test.editor.should_quit());
}

/// Test invalid command
#[test]
fn test_command_invalid() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("invalidcommand");
    test.press_enter();

    // Should return to normal mode
    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :noh command (no highlight)
#[test]
fn test_command_noh() {
    let mut test = EditorTest::new("test\n");

    test.press('/');
    test.type_text("test");
    test.press_enter();

    test.press(':');
    test.type_text("noh");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}

/// Test :reg command (show registers)
#[test]
fn test_command_registers() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("reg");
    test.press_enter();

    // Mode depends on whether there are registers to display:
    // - No registers: "No registers in use" goes to status bar → Normal mode
    // - Has registers (e.g., clipboard content): multi-line output → HoverPreview mode
    let mode = test.editor.mode();
    assert!(
        mode == ovim::mode::Mode::Normal || mode == ovim::mode::Mode::HoverPreview,
        "Expected Normal or HoverPreview mode after :reg, got {:?}",
        mode
    );
}

/// Test :marks command
#[test]
fn test_command_marks() {
    let mut test = EditorTest::new("test\n");

    test.press(':');
    test.type_text("marks");
    test.press_enter();

    test.assert_mode(ovim::mode::Mode::Normal);
}
