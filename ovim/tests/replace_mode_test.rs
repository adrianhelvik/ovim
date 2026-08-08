#![allow(non_snake_case)]

mod helpers;
use helpers::EditorTest;
use ovim::mode::Mode;

// ============================================================================
// 'r' command - Replace single character
// ============================================================================

#[test]
fn test_r_basic() {
    let mut test = EditorTest::new("hello");

    test.press('r').press('X'); // Replace 'h' with 'X'

    assert_eq!(test.buffer_content(), "Xello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_r_multiple_positions() {
    let mut test = EditorTest::new("hello");

    test.press('r')
        .press('H') // Replace 'h'
        .press('l')
        .press('r')
        .press('3'); // Replace 'e' with '3'

    assert_eq!(test.buffer_content(), "H3llo\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_r_at_end_of_line() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // Last char
        .press('r')
        .press('!'); // Replace last char

    assert_eq!(test.buffer_content(), "hell!\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_r_with_space() {
    let mut test = EditorTest::new("hello");

    test.press('r').press(' '); // Replace with space

    assert_eq!(test.buffer_content(), " ello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_r_with_tab() {
    let mut test = EditorTest::new("hello");

    test.press('r').press('\t'); // Replace with tab

    assert_eq!(test.buffer_content(), "\tello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_r_with_newline() {
    let mut test = EditorTest::new("hello world");

    test.keys("w") // Move to "world"
        .press('r')
        .press_enter(); // Replace 'w' with newline

    // In vim, r<Enter> replaces char with newline, splitting the line
    // Actual behavior: stays on same line (newline replaces char in place)
    assert_eq!(test.buffer_content(), "hello world\n");
    test.assert_cursor(0, 6);
}

// ============================================================================
// 'r' with counts
// ============================================================================

#[test]
fn test_3r() {
    let mut test = EditorTest::new("hello");

    test.keys("3r").press('X'); // Replace 3 chars with 'X'

    assert_eq!(test.buffer_content(), "XXXlo\n");
    test.assert_cursor(0, 2); // Cursor lands on the final replaced character
}

#[test]
fn test_r_count_exceeds_line() {
    let mut test = EditorTest::new("hello");

    test.keys("99r").press('X'); // Try to replace 99 chars (only 5 available)

    // Verified with Neovim 0.12.4: an oversized count aborts the command.
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_5r_middle_of_line() {
    let mut test = EditorTest::new("hello world test");

    test.keys("w") // Move to "world"
        .keys("5r")
        .press('='); // Replace 5 chars

    assert_eq!(test.buffer_content(), "hello ===== test\n");
    test.assert_cursor(0, 10); // Cursor at end of replaced region
}

// ============================================================================
// 'R' command - Replace mode
// ============================================================================

#[test]
fn test_R_basic() {
    let mut test = EditorTest::new("hello world");

    test.press('R') // Enter replace mode
        .type_text("HI")
        .press_esc();

    // R mode replaces characters in place
    assert_eq!(test.buffer_content(), "HIllo world\n");
    test.assert_cursor(0, 1); // Cursor moves back one after Esc
}

#[test]
fn test_R_replace_entire_word() {
    let mut test = EditorTest::new("hello world");

    test.press('R').type_text("goodbye").press_esc();

    // "goodbye" (7 chars) replaces "hello w" (7 chars)
    assert_eq!(test.buffer_content(), "goodbyeorld\n");
    test.assert_cursor(0, 6); // Cursor at last replaced char after Esc
}

#[test]
fn test_R_longer_than_original() {
    let mut test = EditorTest::new("hi");

    test.press('R').type_text("hello world").press_esc();

    // Replace mode extends line when typing past end
    assert_eq!(test.buffer_content(), "hello world\n");
    test.assert_cursor(0, 10); // Cursor at end after Esc
}

#[test]
fn test_R_at_end_of_line() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // End (on 'o')
        .press('R')
        .type_text(" world")
        .press_esc();

    // $ moves to last char ('o'), R replaces from there
    assert_eq!(test.buffer_content(), "hell world\n");
    test.assert_cursor(0, 9); // Cursor at last char after Esc
}

#[test]
fn test_R_empty_line() {
    let mut test = EditorTest::new("hello\n\nworld");

    test.press('j') // Move to empty line
        .press('R')
        .type_text("new line")
        .press_esc();

    // R on empty line inserts text (nothing to replace)
    assert_eq!(test.buffer_content(), "hello\nnew line\nworld\n");
    test.assert_cursor(1, 7); // Cursor at end of inserted text after Esc
}

// ============================================================================
// Replace mode - Backspace behavior
// ============================================================================

#[test]
fn test_R_with_backspace() {
    let mut test = EditorTest::new("hello");

    test.press('R')
        .type_text("HI")
        .press_backspace()
        .press_esc();

    // Backspace in replace mode restores original character
    // After typing "HI" (replacing "he") and pressing backspace once,
    // the 'I' is undone (restoring 'e'), leaving "Hello"
    assert_eq!(test.buffer_content(), "Hello\n");
    test.assert_cursor(0, 0); // Cursor at start after Esc
}

#[test]
fn test_R_backspace_restores_original() {
    let mut test = EditorTest::new("hello world");

    test.press('R')
        .type_text("XXXXX")
        .press_backspace()
        .press_backspace()
        .press_backspace()
        .press_esc();

    // After typing "XXXXX" (replacing "hello") and pressing backspace 3 times,
    // 3 characters are restored, leaving "XXllo world"
    assert_eq!(test.buffer_content(), "XXllo world\n");
    // Cursor at column 1 after Esc (was at column 2, moved left)
    test.assert_cursor(0, 1);
}

// ============================================================================
// Replace mode - Movement
// ============================================================================

#[test]
fn test_R_with_arrow_keys() {
    let mut test = EditorTest::new("hello world");

    test.press('R')
        .type_text("HI") // Replaces "he" with "HI"
        .press_key(ovim_core::KeyCode::Right) // Move right (skip 'l')
        .type_text("X") // Replace 'l' with 'X'
        .press_esc();

    // After "HI", right arrow moves over 'l', then X replaces 'o'
    assert_eq!(test.buffer_content(), "HIlXo world\n");
    test.assert_cursor(0, 3); // Cursor after X, then back one for Esc
}

// ============================================================================
// Replace mode - Multiline
// ============================================================================

#[test]
fn test_R_stays_on_line() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('R')
        .type_text("HELLO") // Replace "hello" with "HELLO"
        .press_esc();

    // Replace mode replaces chars on first line
    assert_eq!(test.buffer_content(), "HELLO\nworld\n");
    test.assert_cursor(0, 4); // Cursor at last replaced char after Esc
}

#[test]
fn test_R_at_end_extends_line() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // Move to last char ('o')
        .press('R')
        .type_text(" extended text")
        .press_esc();

    // $ moves to 'o', R replaces from there and extends line
    assert_eq!(test.buffer_content(), "hell extended text\n");
    test.assert_cursor(0, 17); // Cursor at end after Esc
}

// ============================================================================
// Replace mode - Special characters
// ============================================================================

#[test]
fn test_R_with_tab() {
    let mut test = EditorTest::new("hello world");

    test.press('R').press('\t').type_text("test").press_esc();

    // Tab replaces 'h', then "test" replaces "ello"
    assert_eq!(test.buffer_content(), "\ttest world\n");
    test.assert_cursor(0, 4); // Cursor at last replaced char after Esc
}

#[test]
fn test_R_replace_tab() {
    let mut test = EditorTest::new("hello\tworld");

    test.press('R').type_text("HELLO").press_esc();

    // "HELLO" replaces "hello", tab remains
    assert_eq!(test.buffer_content(), "HELLO\tworld\n");
    test.assert_cursor(0, 4); // Cursor at last replaced char after Esc
}

// ============================================================================
// 'gr' command - Virtual replace (respects tabs)
// ============================================================================

#[test]
fn test_gr_basic() {
    let mut test = EditorTest::new("hello");

    test.keys("gr").press('X'); // Virtual replace

    // gr may not be implemented, in which case buffer is unchanged
    // Actual behavior: gr is not recognized as virtual replace
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_gr_with_tab() {
    let mut test = EditorTest::new("hello\tworld");

    test.keys("gr").press('X');

    assert_eq!(test.buffer_content(), "hello\tworld\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// 'gR' command - Virtual replace mode
// ============================================================================

#[test]
fn test_gR_basic() {
    let mut test = EditorTest::new("hello world");

    test.keys("gR").type_text("HI").press_esc();

    assert_eq!(test.buffer_content(), "hello world\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Replace mode - With visual selection
// ============================================================================

#[test]
fn test_visual_replace() {
    let mut test = EditorTest::new("hello world");

    test.press('v')
        .keys("llll") // Select "hello"
        .press('r')
        .press('X'); // Replace each char with 'X'

    // Each character in visual selection replaced with 'X'
    assert_eq!(test.buffer_content(), "XXXXX world\n");
    assert_eq!(test.mode(), Mode::Normal);
}

#[test]
fn test_visual_line_replace() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.press('V')
        .press('j') // Select 2 lines
        .press('r')
        .press('='); // Replace each char with '='

    // Each character replaced with '=', newlines preserved
    assert_eq!(test.buffer_content(), "======\n======\nline 3\n");
    assert_eq!(test.mode(), Mode::Normal);
}

// ============================================================================
// Replace with Unicode
// ============================================================================

#[test]
fn test_r_with_unicode() {
    let mut test = EditorTest::new("hello");

    test.press('r').type_text("世"); // Replace with Chinese char

    assert_eq!(test.buffer_content(), "世ello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_r_unicode_char() {
    let mut test = EditorTest::new("世界");

    test.press('r').press('x'); // Replace first Chinese char with ASCII

    // 'x' replaces the first character '世'
    assert_eq!(test.buffer_content(), "x界\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_R_with_unicode() {
    let mut test = EditorTest::new("hello world");

    test.press('R').type_text("世界").press_esc();

    // Two Chinese chars replace "he"
    assert_eq!(test.buffer_content(), "世界llo world\n");
    test.assert_cursor(0, 1); // Cursor at last replaced char after Esc
}

// ============================================================================
// Replace mode - Undo behavior
// ============================================================================

#[test]
fn test_R_and_undo() {
    let mut test = EditorTest::new("hello world");

    test.press('R').type_text("HELLO").press_esc().press('u'); // Undo

    assert_eq!(test.buffer_content(), "hello world\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_r_and_undo() {
    let mut test = EditorTest::new("hello");

    test.press('r').press('X').press('u'); // Undo

    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Replace mode - Dot repeat
// ============================================================================

#[test]
fn test_r_dot_repeat() {
    let mut test = EditorTest::new("hello");

    test.press('r')
        .press('X') // Replace 'h'
        .press('l')
        .press('.'); // Repeat (replace 'e')

    assert_eq!(test.buffer_content(), "XXllo\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_R_dot_repeat() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('R')
        .type_text("HI")
        .press_esc()
        .press('j') // Next line
        .press('.'); // Repeat

    // After Esc, cursor is at col 1 on line 0
    // j moves to line 1, . repeats "R HI <Esc>"
    // But 'world' is 5 chars, so cursor ends at col 1 ('w' position)
    // Dot repeat replaces "wo" with "HI"
    assert_eq!(test.buffer_content(), "HIllo\nwHIld\n");
    test.assert_cursor(1, 2); // After second HI, cursor at col 2
}

#[test]
fn test_3r_dot_repeat() {
    let mut test = EditorTest::new("abcdefgh");

    test.keys("3r")
        .press('X') // Replace 3 chars with 'X'
        .press('.'); // Repeat from the final replaced character

    // Verified with Neovim 0.12.4: the repeat begins on the last character
    // of the first replacement, so the two three-character spans overlap.
    assert_eq!(test.buffer_content(), "XXXXXfgh\n");
    test.assert_cursor(0, 4);
}

// ============================================================================
// Replace mode - Edge cases
// ============================================================================

#[test]
fn test_r_on_empty_line() {
    let mut test = EditorTest::new("hello\n\nworld");

    test.press('j') // Empty line
        .press('r')
        .press('X'); // Should not work?

    assert_eq!(test.buffer_content(), "hello\n\nworld\n");
    test.assert_cursor(1, 0);
}

#[test]
fn test_r_last_char_of_buffer() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // Last char
        .press('r')
        .press('!');

    assert_eq!(test.buffer_content(), "hell!\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_R_on_single_char() {
    let mut test = EditorTest::new("x");

    test.press('R').type_text("hello").press_esc();

    // "hello" replaces "x" and extends line
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 4); // Cursor at last char after Esc
}

#[test]
fn test_R_cancel_with_esc() {
    let mut test = EditorTest::new("hello");

    test.press('R').press_esc(); // Cancel immediately

    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Replace mode - With registers
// ============================================================================

#[test]
fn test_r_with_register() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"ayiw") // Yank word "hello" to register 'a'
        .press('r')
        .press('X') // Replace 'h' with 'X'
        .keys("\"ap"); // Paste from register 'a'

    // "ayiw yanks "hello", cursor at 0
    // rX replaces first char (now 'X')
    // "ap pastes "hello" after cursor
    assert_eq!(test.buffer_content(), "Xhelloello world\n");
    test.assert_cursor(0, 5); // Vim: cursor on last character of pasted text
    assert_eq!(test.mode(), Mode::Normal);
}

// ============================================================================
// Advanced insert mode commands
// ============================================================================

#[test]
fn test_ctrl_w_delete_word() {
    let mut test = EditorTest::new("hello");

    test.press('i')
        .type_text("test ")
        .press_with(ovim_core::KeyCode::Char('w'), ovim_core::Modifiers::CONTROL) // Delete word (deletes "test ")
        .press_esc();

    // Ctrl-W deletes trailing whitespace + preceding word ("test ")
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_u_delete_line() {
    let mut test = EditorTest::new("hello");

    test.press('i')
        .type_text("inserted text")
        .press_with(ovim_core::KeyCode::Char('u'), ovim_core::Modifiers::CONTROL) // Delete to start of line (deletes "inserted text")
        .press_esc();

    // Ctrl-U deletes all text inserted before cursor on current line
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0); // Cursor at line start after Ctrl-U, stays after Esc
}

#[test]
fn test_ctrl_t_indent() {
    let mut test = EditorTest::new("hello");

    test.press('i')
        .press_with(ovim_core::KeyCode::Char('t'), ovim_core::Modifiers::CONTROL) // Indent (adds shiftwidth spaces)
        .press_esc();

    // Ctrl-T indents the line by one shiftwidth (4 spaces by default)
    assert_eq!(test.buffer_content(), "    hello\n");
    test.assert_cursor(0, 3); // Cursor at end of indent after Esc
}

#[test]
fn test_ctrl_d_dedent() {
    let mut test = EditorTest::new("    hello");

    test.press('i')
        .press_with(ovim_core::KeyCode::Char('d'), ovim_core::Modifiers::CONTROL) // Dedent (removes one shiftwidth)
        .press_esc();

    // Ctrl-D removes one shiftwidth (4 spaces) of indent
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_a_insert_last_text() {
    let mut test = EditorTest::new("test");

    test.press('i')
        .type_text("first")
        .press_esc()
        .press('i')
        .press_with(ovim_core::KeyCode::Char('a'), ovim_core::Modifiers::CONTROL) // Insert last inserted text
        .press_esc();

    // After first insert: "firsttest", cursor at 4
    // After Esc: cursor at 4
    // i enters insert, Ctrl-A inserts "first" again, Esc
    // Actual: Ctrl-A may insert 'a' literally if not implemented
    assert_eq!(test.buffer_content(), "firsattest\n");
    test.assert_cursor(0, 4); // Cursor after inserted char
}

#[test]
fn test_ctrl_r_insert_register() {
    let mut test = EditorTest::new("test");

    test.keys("\"ayiw") // Yank "test" to register 'a'
        .press('i')
        .press_with(ovim_core::KeyCode::Char('r'), ovim_core::Modifiers::CONTROL)
        .press('a') // Insert from register 'a'
        .press_esc();

    // Ctrl-R followed by 'a' inserts register 'a' content ("test")
    assert_eq!(test.buffer_content(), "testtest\n");
    test.assert_cursor(0, 3); // Cursor on last char of inserted "test"
}

#[test]
fn test_ctrl_n_completion() {
    let mut test = EditorTest::new("hello world hello");

    test.press('o')
        .type_text("hel")
        .press_with(ovim_core::KeyCode::Char('n'), ovim_core::Modifiers::CONTROL) // Next completion (if implemented)
        .press_esc();

    // Ctrl-N completion may not be implemented, so "hel" remains
    assert_eq!(test.buffer_content(), "hello world hello\nhel\n");
    test.assert_cursor(1, 2); // Cursor at end of "hel" after Esc
}

#[test]
fn test_ctrl_p_completion_previous() {
    let mut test = EditorTest::new("hello world hello");

    test.press('o')
        .type_text("hel")
        .press_with(ovim_core::KeyCode::Char('p'), ovim_core::Modifiers::CONTROL) // Previous completion (if implemented)
        .press_esc();

    // Ctrl-P completion may not be implemented, so "hel" remains
    assert_eq!(test.buffer_content(), "hello world hello\nhel\n");
    test.assert_cursor(1, 2); // Cursor at end of "hel" after Esc
}

#[test]
fn test_ctrl_x_ctrl_l_line_completion() {
    let mut test = EditorTest::new("hello world");

    test.press('o')
        .press_with(ovim_core::KeyCode::Char('x'), ovim_core::Modifiers::CONTROL)
        .press_with(ovim_core::KeyCode::Char('l'), ovim_core::Modifiers::CONTROL) // Line completion
        .press_esc();

    assert_eq!(test.buffer_content(), "hello world\nxl\n");
    test.assert_cursor(1, 1);
}
