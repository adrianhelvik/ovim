mod helpers;
use helpers::EditorTest;
use ovim::mode::Mode;

#[test]
fn test_replace_single_char() {
    let mut test = EditorTest::new("hello");

    // Move to start and replace 'h' with 'x'
    test.keys("rx");

    assert_eq!(test.buffer_content(), "xello\n");
    test.assert_cursor(0, 0);
    assert_eq!(test.editor.mode(), Mode::Normal);
}

#[test]
fn test_replace_multiple_chars() {
    let mut test = EditorTest::new("hello");

    // Replace 3 characters with 'x'
    test.keys("3rx");

    assert_eq!(test.buffer_content(), "xxxlo\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_replace_at_end_of_word() {
    let mut test = EditorTest::new("hello");

    // Move to 'o' and replace with 'x'
    test.keys("4lrx");

    assert_eq!(test.buffer_content(), "hellx\n");
}

#[test]
fn test_replace_respects_line_length() {
    let mut test = EditorTest::new("hi");

    // Try to replace 5 chars when only 2 exist
    test.keys("5rx");

    // Verified with Neovim 0.12.4: an oversized count aborts the command.
    assert_eq!(test.buffer_content(), "hi\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_replace_with_count_and_position() {
    let mut test = EditorTest::new("abcdefgh");

    // Move to position 2 ('c') and replace 3 chars
    test.keys("2l3rz");

    assert_eq!(test.buffer_content(), "abzzzfgh\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_replace_can_undo() {
    let mut test = EditorTest::new("hello");

    // Replace and undo
    test.keys("rx");
    assert_eq!(test.buffer_content(), "xello\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello\n");
}

#[test]
fn test_replace_stays_in_normal_mode() {
    let mut test = EditorTest::new("hello");

    test.keys("ra");

    // Should stay in normal mode, not insert mode
    assert_eq!(test.editor.mode(), Mode::Normal);
    assert_eq!(test.buffer_content(), "aello\n");
}
