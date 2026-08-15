/// Tests for grapheme/char column correctness in insert mode operations.
///
/// cursor.col() returns a grapheme cluster index, but buffer operations
/// (insert_text_at, delete_range) work with char indices. These tests
/// verify that insert_char, insert_newline, insert_tab, and
/// delete_char_before_cursor properly convert between the two.
mod helpers;
use helpers::EditorTest;

// ---------------------------------------------------------------------------
// insert_char: position must use char col, cursor result must be grapheme col
// ---------------------------------------------------------------------------

#[test]
fn test_insert_char_after_emoji() {
    // "a👨‍👩‍👧‍👦b" — 3 graphemes (a, family emoji, b), but 9 chars
    // Place cursor after emoji (grapheme col 2) and insert 'X'.
    // The 'X' should appear between the emoji and 'b', not mid-emoji.
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.keys("i"); // insert mode at col 0
                    // Move right twice: col 0 -> 1 -> 2 (past 'a', past emoji, onto 'b')
    test.press_key(ovim_core::KeyCode::Right);
    test.press_key(ovim_core::KeyCode::Right);
    assert_eq!(test.cursor(), (0, 2), "cursor should be at grapheme col 2");

    test.type_text("X");
    assert_eq!(
        test.cursor(),
        (0, 3),
        "cursor should advance to grapheme col 3 after inserting 'X'"
    );

    let content = test.buffer_content();
    assert_eq!(content, "a👨‍👩‍👧‍👦Xb\n", "'X' should be between emoji and 'b'");
}

#[test]
fn test_insert_char_before_emoji() {
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.keys("i"); // insert at col 0
    test.press_key(ovim_core::KeyCode::Right); // col 1 (between 'a' and emoji)
    test.type_text("X");

    let content = test.buffer_content();
    assert_eq!(content, "aX👨‍👩‍👧‍👦b\n", "'X' should be between 'a' and emoji");
    assert_eq!(test.cursor(), (0, 2));
}

// ---------------------------------------------------------------------------
// insert_newline: at_eof check and text_before_cursor must use char col
// ---------------------------------------------------------------------------

#[test]
fn test_newline_after_emoji() {
    // Press Enter after emoji — newline should split correctly
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right); // past 'a'
    test.press_key(ovim_core::KeyCode::Right); // past emoji
    test.press_enter();

    let content = test.buffer_content();
    assert_eq!(
        content, "a👨‍👩‍👧‍👦\nb\n",
        "newline should split after emoji, not mid-emoji"
    );
    assert_eq!(test.cursor(), (1, 0), "cursor on new line at col 0");
}

#[test]
fn test_newline_before_emoji() {
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right); // past 'a'
    test.press_enter();

    let content = test.buffer_content();
    assert_eq!(content, "a\n👨‍👩‍👧‍👦b\n");
    assert_eq!(test.cursor(), (1, 0));
}

#[test]
fn test_newline_with_indent_after_emoji() {
    // Indented line with emoji — auto-indent should work correctly
    let mut test = EditorTest::new("    a👨‍👩‍👧‍👦b\n");
    test.keys("A"); // append at end of line
    test.press_enter();

    let content = test.buffer_content();
    assert!(
        content.starts_with("    a👨‍👩‍👧‍👦b\n    \n"),
        "should preserve indent on new line, got: {:?}",
        content
    );
    assert_eq!(test.cursor(), (1, 4), "cursor at end of indent");
}

// ---------------------------------------------------------------------------
// delete_char_before_cursor: must delete the correct grapheme
// ---------------------------------------------------------------------------

#[test]
fn test_backspace_after_emoji() {
    // Cursor after emoji, backspace should delete the char before cursor
    // (which is the last codepoint of the emoji when using char-level delete,
    // but the position calculation should at least be correct)
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right); // past 'a'
    test.press_key(ovim_core::KeyCode::Right); // past emoji
    test.press_key(ovim_core::KeyCode::Right); // past 'b'
                                               // Cursor at grapheme col 3 (end of "a👨‍👩‍👧‍👦b")
    test.press_backspace();

    let content = test.buffer_content();
    assert_eq!(content, "a👨‍👩‍👧‍👦\n", "'b' should be deleted");
    assert_eq!(test.cursor(), (0, 2));
}

// ---------------------------------------------------------------------------
// insert_tab: position must use char col
// ---------------------------------------------------------------------------

#[test]
fn test_tab_after_emoji() {
    let mut test = EditorTest::new("a👨‍👩‍👧‍👦b\n");
    test.editor.options.expand_tab = true;
    test.editor.options.shift_width = 4;
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right); // past 'a'
    test.press_key(ovim_core::KeyCode::Right); // past emoji
    test.press_key(ovim_core::KeyCode::Tab);

    let content = test.buffer_content();
    assert_eq!(
        content, "a👨‍👩‍👧‍👦 b\n",
        "Tab should advance from display column 3 to soft stop 4"
    );
}

// ---------------------------------------------------------------------------
// Bracket detection in insert_newline with multi-byte chars before bracket
// ---------------------------------------------------------------------------

#[test]
fn test_newline_bracket_detection_after_emoji() {
    // The bracket detection uses text_before_cursor which was using grapheme col
    // as char count — verify it works correctly with multi-byte chars
    let mut test = EditorTest::new("👨‍👩‍👧‍👦 {\n");
    test.editor.options.expand_tab = true;
    test.editor.options.shift_width = 4;
    test.keys("A"); // end of line
    test.press_enter();

    let content = test.buffer_content();
    // Should detect the '{' and add extra indent
    assert!(
        content.contains("\n    \n"),
        "should add extra indent after '{{', got: {:?}",
        content
    );
}

// ---------------------------------------------------------------------------
// ASCII baseline — ensure no regression for the common case
// ---------------------------------------------------------------------------

#[test]
fn test_insert_char_ascii_still_works() {
    let mut test = EditorTest::new("hello\n");
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right);
    test.press_key(ovim_core::KeyCode::Right);
    test.type_text("X");

    assert_eq!(test.buffer_content(), "heXllo\n");
    assert_eq!(test.cursor(), (0, 3));
}

#[test]
fn test_newline_ascii_still_works() {
    let mut test = EditorTest::new("    hello\n");
    test.keys("A");
    test.press_enter();
    assert_eq!(test.buffer_content(), "    hello\n    \n");
    assert_eq!(test.cursor(), (1, 4));
}

#[test]
fn test_backspace_ascii_still_works() {
    let mut test = EditorTest::new("hello\n");
    test.keys("i");
    test.press_key(ovim_core::KeyCode::Right);
    test.press_key(ovim_core::KeyCode::Right);
    test.press_backspace();

    assert_eq!(test.buffer_content(), "hllo\n");
    assert_eq!(test.cursor(), (0, 1));
}
