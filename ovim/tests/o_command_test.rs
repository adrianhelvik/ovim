use ovim::editor::{Editor, InputHandler};
use ovim::unicode::GraphemeCol;
use ovim_core::{KeyCode, KeyEvent, Modifiers};

/// Helper function to create a KeyEvent
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, Modifiers::NONE)
}

/// Helper function to handle a key press
fn press(editor: &mut Editor, code: KeyCode) {
    InputHandler::handle_key_event(editor, key(code)).unwrap();
}

/// Helper to press a character key
fn press_char(editor: &mut Editor, c: char) {
    press(editor, KeyCode::Char(c));
}

/// Helper to get full buffer content as string
fn buffer_content(editor: &Editor) -> String {
    let mut content = String::new();
    for i in 0..editor.buffer().line_count() {
        if let Some(line) = editor.buffer().line_text(i) {
            content.push_str(&line);
        }
    }
    content
}

#[test]
fn test_o_middle_of_file() {
    // Test: 'o' on a line in the middle of file
    // with_content adds trailing newline, so "line 1\nline 2\nline 3" becomes
    // "line 1\nline 2\nline 3\n" which is 3 lines in Vim-style counting
    // (Ropey counts 4 including phantom empty line, but line_count() adjusts)
    let mut editor = Editor::with_content("line 1\nline 2\nline 3");

    // Press 'o' on line 0
    press_char(&mut editor, 'o');

    // Should be in insert mode
    assert_eq!(editor.mode(), ovim::mode::Mode::Insert);

    // Should have 4 lines now (3 original + 1 new)
    assert_eq!(editor.buffer().line_count(), 4);

    // Cursor should be on line 1 (the new line)
    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol::ZERO);

    // Lines should be correct
    assert_eq!(editor.buffer().line_text(0).unwrap(), "line 1");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "");
    assert_eq!(editor.buffer().line_text(2).unwrap(), "line 2");
    assert_eq!(editor.buffer().line_text(3).unwrap(), "line 3");
}

#[test]
fn test_o_last_line_no_newline() {
    // Test: 'o' on the last line
    // with_content adds trailing newline, so "line 1\nline 2" becomes
    // "line 1\nline 2\n" which is 2 lines in Vim-style counting
    let mut editor = Editor::with_content("line 1\nline 2");

    // Move to line 1 (the last actual line)
    press_char(&mut editor, 'j');
    assert_eq!(editor.buffer().cursor().line(), 1);

    // Press 'o'
    press_char(&mut editor, 'o');

    // Should be in insert mode
    assert_eq!(editor.mode(), ovim::mode::Mode::Insert);

    // Should have 3 lines (2 original + 1 new)
    assert_eq!(editor.buffer().line_count(), 3);

    // Cursor should be on line 2 (the new line)
    assert_eq!(editor.buffer().cursor().line(), 2);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol::ZERO);

    // Verify content
    assert_eq!(editor.buffer().line_text(0).unwrap(), "line 1");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "line 2");
    assert_eq!(editor.buffer().line_text(2).unwrap(), "");
}

#[test]
fn test_o_with_indentation() {
    // Test: 'o' on an indented line copies indentation
    let mut editor = Editor::with_content("line 1\n    indented line\nline 3");

    // Move to indented line
    press_char(&mut editor, 'j');
    assert_eq!(editor.buffer().cursor().line(), 1);

    // Press 'o'
    press_char(&mut editor, 'o');

    // Should be in insert mode
    assert_eq!(editor.mode(), ovim::mode::Mode::Insert);

    // Cursor should be at column 4 (after the indentation)
    assert_eq!(editor.buffer().cursor().line(), 2);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));

    // New line should have indentation
    assert_eq!(editor.buffer().line_text(2).unwrap(), "    ");
}

#[test]
fn test_o_type_text() {
    // Test: 'o' followed by typing text
    // with_content adds trailing newline, so "line 1\nline 2" becomes
    // "line 1\nline 2\n" which is 2 lines in Vim-style counting
    let mut editor = Editor::with_content("line 1\nline 2");

    // Press 'o'
    press_char(&mut editor, 'o');

    // Type some text
    press_char(&mut editor, 'n');
    press_char(&mut editor, 'e');
    press_char(&mut editor, 'w');

    // Exit insert mode
    press(&mut editor, KeyCode::Esc);

    // Check content
    let content = buffer_content(&editor);
    assert!(
        content.contains("new"),
        "Content should contain 'new': {}",
        content
    );

    // Should have 3 lines (2 original + 1 new)
    assert_eq!(editor.buffer().line_count(), 3);
    assert_eq!(editor.buffer().line_text(0).unwrap(), "line 1");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "new");
    assert_eq!(editor.buffer().line_text(2).unwrap(), "line 2");
}

#[test]
fn test_o_and_undo() {
    // Test: 'o' followed by typing and undo
    // with_content adds a trailing newline, so "line 1\nline 2\nline 3" becomes
    // "line 1\nline 2\nline 3\n" which is 3 lines in Vim-style counting
    let mut editor = Editor::with_content("line 1\nline 2\nline 3");

    // Press 'o'
    press_char(&mut editor, 'o');

    // Type text
    press_char(&mut editor, 't');
    press_char(&mut editor, 'e');
    press_char(&mut editor, 's');
    press_char(&mut editor, 't');

    // Exit insert mode
    press(&mut editor, KeyCode::Esc);

    // Should have 4 lines (3 original + 1 new)
    assert_eq!(editor.buffer().line_count(), 4);
    assert_eq!(editor.buffer().line_text(1).unwrap(), "test");

    // Undo
    press_char(&mut editor, 'u');

    // Should be back to original 3 lines
    assert_eq!(editor.buffer().line_count(), 3);
    assert_eq!(editor.buffer().line_text(0).unwrap(), "line 1");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "line 2");
    assert_eq!(editor.buffer().line_text(2).unwrap(), "line 3");
}

#[test]
fn test_o_on_empty_file() {
    // Test: 'o' on an empty file
    // Empty content ("") becomes "\n" (1 line with just newline in Vim-style counting)
    // After 'o', we add a new line, making it 2 lines
    let mut editor = Editor::with_content("");

    // Press 'o'
    press_char(&mut editor, 'o');

    // Should be in insert mode
    assert_eq!(editor.mode(), ovim::mode::Mode::Insert);

    // Should have 2 lines (1 original empty line + 1 new)
    assert_eq!(editor.buffer().line_count(), 2);

    // Cursor should be on line 1 (the new line)
    assert_eq!(editor.buffer().cursor().line(), 1);
}

#[test]
fn test_o_single_line_no_newline() {
    // Test: 'o' on a single line
    // with_content("hello") -> "hello\n" which is 1 line in Vim-style counting
    let mut editor = Editor::with_content("hello");

    // Press 'o'
    press_char(&mut editor, 'o');

    // Should be in insert mode
    assert_eq!(editor.mode(), ovim::mode::Mode::Insert);

    // Should have 2 lines (1 original + 1 new)
    assert_eq!(editor.buffer().line_count(), 2);

    // Cursor should be on line 1 (the new line)
    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol::ZERO);

    // Line content
    assert_eq!(editor.buffer().line_text(0).unwrap(), "hello");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "");
}

#[test]
fn test_o_multiple_times() {
    // Test: pressing 'o' multiple times
    // with_content("start") -> "start\n" which is 1 line in Vim-style counting
    let mut editor = Editor::with_content("start");

    // Press 'o' and add text
    press_char(&mut editor, 'o');
    press_char(&mut editor, 'l');
    press_char(&mut editor, '1');
    press(&mut editor, KeyCode::Esc);

    // Press 'o' again and add more text
    press_char(&mut editor, 'o');
    press_char(&mut editor, 'l');
    press_char(&mut editor, '2');
    press(&mut editor, KeyCode::Esc);

    // Should have 3 lines (1 original + 2 new)
    assert_eq!(editor.buffer().line_count(), 3);
    assert_eq!(editor.buffer().line_text(0).unwrap(), "start");
    assert_eq!(editor.buffer().line_text(1).unwrap(), "l1");
    assert_eq!(editor.buffer().line_text(2).unwrap(), "l2");
}

#[test]
fn test_o_preserves_tab_indentation_width() {
    // Default expandtab normalizes a hard tab while preserving its width.
    let mut editor = Editor::with_content("line 1\n\ttabbed\nline 3");

    // Move to tabbed line
    press_char(&mut editor, 'j');

    // Press 'o'
    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().line_text(2).unwrap(), "    ");
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
}

#[test]
fn test_o_mixed_indentation() {
    // Mixed indentation is normalized by default while preserving visual width.
    let mut editor = Editor::with_content("start\n  \tmixed\nend");

    // Move to mixed indentation line
    press_char(&mut editor, 'j');

    // Press 'o'
    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().line_text(2).unwrap(), "    ");
}

#[test]
fn test_o_copyindent_preserves_mixed_indentation() {
    let mut editor = Editor::with_content("start\n  \tmixed\nend");
    editor.options.copy_indent = true;

    press_char(&mut editor, 'j');
    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().line_text(2).unwrap(), "  \t");
}

// ============================================================================
// Trailing newline: o should not create a phantom extra line
// ============================================================================

#[test]
fn test_o_then_dd_no_phantom_line() {
    // Regression: 'o' on the last line created a visible phantom line because
    // the renderer used raw_line_count (which includes Ropey's phantom empty
    // line after a trailing newline). This test verifies the buffer state is
    // correct after: insert text -> o -> Esc -> G -> dd -> dd
    //
    // Sequence: o<Esc>Gdddd on a 5-line buffer
    // Expected: 4 lines, cursor on line 4 (the empty line)
    let mut editor = Editor::with_content("Hello world\nFoo bar\nBar baz\n\nHello");

    assert_eq!(editor.buffer().line_count(), 5);

    // Move to last line
    press_char(&mut editor, 'G');

    // o - open line below (creates line 6), then Esc
    press_char(&mut editor, 'o');
    assert_eq!(editor.buffer().line_count(), 6);
    press(&mut editor, KeyCode::Esc);

    // G - go to last line
    press_char(&mut editor, 'G');
    assert_eq!(
        editor.buffer().cursor().line(),
        5,
        "G should go to last real line (0-indexed: 5)"
    );

    // dd - delete line 6 (the empty line from 'o')
    press_char(&mut editor, 'd');
    press_char(&mut editor, 'd');
    assert_eq!(editor.buffer().line_count(), 5);
    // Cursor should now be on line 5 (0-indexed: 4) = "Hello"
    assert_eq!(editor.buffer().cursor().line(), 4);
    assert!(editor.buffer().line_text(4).unwrap().starts_with("Hello"));

    // dd - delete "Hello"
    press_char(&mut editor, 'd');
    press_char(&mut editor, 'd');
    assert_eq!(editor.buffer().line_count(), 4);
    // Cursor should be on line 4 (0-indexed: 3) = the empty line
    assert_eq!(
        editor.buffer().cursor().line(),
        3,
        "After two dd's from end, cursor should be on line 4 (0-indexed: 3)"
    );
    assert_eq!(editor.buffer().line_text(3).unwrap(), "");
}

// ============================================================================
// Redo cursor validation after o
// ============================================================================

#[test]
fn test_o_undo_redo_cursor_valid() {
    // Regression: redo of 'o' could place cursor past end of line because
    // cursor_after was captured in insert mode (one past last char).
    let mut editor = Editor::with_content("hello\nworld");

    // o, type text, Esc
    press_char(&mut editor, 'o');
    press_char(&mut editor, 'a');
    press_char(&mut editor, 'b');
    press_char(&mut editor, 'c');
    press(&mut editor, KeyCode::Esc);

    // Undo
    press_char(&mut editor, 'u');
    assert_eq!(editor.buffer().line_count(), 2);

    // Redo (Ctrl-R)
    InputHandler::handle_key_event(
        &mut editor,
        KeyEvent::new(KeyCode::Char('r'), Modifiers::CONTROL),
    )
    .unwrap();

    // After redo, cursor should be within valid line bounds (normal mode)
    let cursor_line = editor.buffer().cursor().line();
    let cursor_col = editor.buffer().cursor().col().0;
    let line = editor.buffer().line_text(cursor_line).unwrap();
    let line_len = line.trim_end_matches('\n').chars().count();
    assert!(
        cursor_col < line_len || line_len == 0,
        "Cursor col {} should be < line_len {} after redo (line: {:?})",
        cursor_col,
        line_len,
        line
    );
}

// ============================================================================
// Count not leaked through mode transitions
// ============================================================================

#[test]
fn test_count_not_leaked_through_o() {
    // Regression: typing '5o' would leak count=5 to next normal mode command.
    let mut editor = Editor::with_content("line 1\nline 2\nline 3\nline 4\nline 5");

    // Type 5, then o — count should be consumed/cleared
    press_char(&mut editor, '5');
    press_char(&mut editor, 'o');
    press(&mut editor, KeyCode::Esc);

    // Now type 'j' — should move 1 line, not 5
    let before_line = editor.buffer().cursor().line();
    press_char(&mut editor, 'j');
    let after_line = editor.buffer().cursor().line();

    assert_eq!(
        after_line - before_line,
        1,
        "j after o should move 1 line, not 5 (count leaked)"
    );
}

// ============================================================================
// Smart indent after opening brackets
// ============================================================================

#[test]
fn test_o_after_opening_brace_adds_indent() {
    let mut editor = Editor::with_content("fn main() {\n}");

    // Cursor on line 0: "fn main() {"
    press_char(&mut editor, 'o');

    // Should add shiftwidth (4 spaces) indent
    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_o_after_opening_paren_adds_indent() {
    let mut editor = Editor::with_content("foo(\n)");

    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_o_after_opening_bracket_adds_indent() {
    let mut editor = Editor::with_content("let x = [\n]");

    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_o_after_brace_with_trailing_spaces() {
    // Trailing spaces after { should still trigger extra indent
    let mut editor = Editor::with_content("fn main() {  \n}");

    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_o_does_not_indent_after_delimiter_in_comment() {
    let mut editor = Editor::with_content("// {\nnext");
    editor.set_file_path("/tmp/indent.rs".to_string());

    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().cursor().col(), GraphemeCol::ZERO);
    assert_eq!(editor.buffer().line_text(1).unwrap(), "");
}

#[test]
fn test_o_indents_after_opener_before_trailing_comment() {
    let mut editor = Editor::with_content("fn main() { // body\n}");
    editor.set_file_path("/tmp/indent.rs".to_string());

    press_char(&mut editor, 'o');

    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_o_no_extra_indent_on_normal_line() {
    let mut editor = Editor::with_content("    let x = 1;\n");

    press_char(&mut editor, 'o');

    // Should copy indent (4 spaces) but not add extra
    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}

#[test]
fn test_big_o_on_line_after_brace_no_extra_indent() {
    // O on "    body" should copy its indent, not add extra
    let mut editor = Editor::with_content("fn main() {\n    body\n}");

    // Move to line 1 ("    body")
    press_char(&mut editor, 'j');
    assert_eq!(editor.buffer().cursor().line(), 1);

    // Press O (shift+o = capital O)
    press_char(&mut editor, 'O');

    // Should copy indent from "    body" (4 spaces), no extra
    assert_eq!(editor.buffer().cursor().line(), 1);
    assert_eq!(editor.buffer().cursor().col(), GraphemeCol(4));
    assert_eq!(editor.buffer().line_text(1).unwrap(), "    ");
}
