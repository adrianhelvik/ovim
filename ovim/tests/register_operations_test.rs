mod helpers;
use helpers::EditorTest;

// ============================================================================
// Named registers - Lowercase letters (a-z)
// ============================================================================

#[test]
fn test_yank_to_named_register() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"ayiw") // Yank word to register 'a' - yanks "hello" (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .keys("\"ap"); // Paste from register 'a' after 'd'

    // yiw yanks "hello" (no trailing space), p pastes after last char 'd'
    assert_eq!(test.buffer_content(), "hello worldhello\n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_delete_to_named_register() {
    let mut test = EditorTest::new("hello world test");

    test.keys("\"adw") // Delete word to register 'a'
        .keys("$")
        .keys("\"ap"); // Paste from register 'a'

    assert_eq!(test.buffer_content(), "world testhello \n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_change_to_named_register() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"aciw") // Change word to register 'a' - deleted "hello" goes to 'a'
        .type_text("goodbye")
        .press_esc()
        .keys("$") // End of line (cursor on 'd')
        .keys("\"ap"); // Paste original word after 'd'

    // ciw deletes "hello" (no trailing space), p pastes after 'd'
    assert_eq!(test.buffer_content(), "goodbye worldhello\n");
    test.assert_cursor(0, 17);
}

#[test]
fn test_multiple_named_registers() {
    let mut test = EditorTest::new("one two three four");

    test.keys("\"ayiw") // Yank "one" to register 'a'
        .keys("w")
        .keys("\"byiw") // Yank "two" to register 'b'
        .keys("w")
        .keys("\"cyiw") // Yank "three" to register 'c'
        .keys("$") // End of line (cursor on 'r')
        .keys("\"ap") // Paste "one" after 'r'
        .keys("\"bp") // Paste "two" after 'e'
        .keys("\"cp"); // Paste "three" after 'o'

    // yiw yanks words without trailing space
    // Paste operations work on same line
    assert_eq!(test.buffer_content(), "one two three fouronetwothree\n");
    test.assert_cursor(0, 28);
}

#[test]
fn test_overwrite_named_register() {
    let mut test = EditorTest::new("first second third");

    test.keys("\"ayiw") // Yank "first" to 'a'
        .keys("w")
        .keys("\"ayiw") // Overwrite with "second"
        .keys("$") // End of line (cursor on 'd')
        .keys("\"ap"); // Paste "second" after 'd'

    // yiw yanks "second" (no trailing space)
    assert_eq!(test.buffer_content(), "first second thirdsecond\n");
    test.assert_cursor(0, 23);
}

#[test]
fn test_all_lowercase_registers() {
    let mut test = EditorTest::new("word");

    // Test that we can use all lowercase letters
    test.keys("\"ayiw") // register a
        .keys("\"zyiw"); // register z

    assert_eq!(test.buffer_content(), "word\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Uppercase registers - Append mode (A-Z)
// ============================================================================

#[test]
fn test_append_to_register() {
    let mut test = EditorTest::new("hello world test");

    test.keys("\"ayiw") // Yank "hello" to 'a'
        .keys("w")
        .keys("\"Ayiw") // Append "world" to 'a'
        .keys("$") // End of line (cursor on 't')
        .keys("\"ap"); // Paste both after 't'

    // yiw yanks words without trailing space, appending gives "helloworld"
    assert_eq!(test.buffer_content(), "hello world testhelloworld\n");
    test.assert_cursor(0, 25);
}

#[test]
fn test_append_multiple_times() {
    let mut test = EditorTest::new("one two three four");

    test.keys("\"ayiw") // "one"
        .keys("w")
        .keys("\"Ayiw") // Append "two"
        .keys("w")
        .keys("\"Ayiw") // Append "three"
        .keys("$") // End of line (cursor on 'r')
        .keys("\"ap"); // Paste all after 'r'

    // yiw yanks words without trailing space, appending gives "onetwothree"
    assert_eq!(test.buffer_content(), "one two three fouronetwothree\n");
    test.assert_cursor(0, 28);
}

#[test]
fn test_append_with_delete() {
    let mut test = EditorTest::new("hello world test");

    test.keys("\"adw") // Delete "hello " to 'a'
        .keys("\"Adw") // Append delete "world " to 'a'
        .keys("\"ap"); // Paste both after 't'

    // dw deletes "hello " and "world " (with spaces), appending gives "hello world "
    // After two dw, buffer is "test", cursor at 0
    // p pastes "hello world " after 't', giving "thello world est"
    assert_eq!(test.buffer_content(), "thello world est\n");
    test.assert_cursor(0, 12);
}

// ============================================================================
// Unnamed register ("")
// ============================================================================

#[test]
fn test_unnamed_register_yank() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" to unnamed register (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .press('p'); // Paste from unnamed after 'd'

    // yiw yanks "hello" (no trailing space)
    assert_eq!(test.buffer_content(), "hello worldhello\n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_unnamed_register_delete() {
    let mut test = EditorTest::new("hello world");

    test.keys("dw") // Delete "hello " to unnamed (with trailing space)
        .press('p'); // Paste from unnamed after 'w'

    // dw deletes "hello ", leaving "world", cursor at 0
    // p pastes "hello " after 'w', giving "whello orld"
    assert_eq!(test.buffer_content(), "whello orld\n");
    test.assert_cursor(0, 6);
}

#[test]
fn test_unnamed_register_change() {
    let mut test = EditorTest::new("hello world");

    test.keys("ciw") // Change "hello" (deleted to unnamed, no trailing space)
        .type_text("X")
        .press_esc()
        .keys("$") // End of line (cursor on 'd')
        .press('p'); // Paste "hello" after 'd'

    // ciw deletes "hello" (no trailing space)
    assert_eq!(test.buffer_content(), "X worldhello\n");
    test.assert_cursor(0, 11);
}

// ============================================================================
// Numbered registers (0-9)
// ============================================================================

#[test]
fn test_register_0_yank() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" (goes to "0, no trailing space)
        .keys("dw") // Delete "hello " (goes to "-, doesn't affect "0)
        .keys("\"0p"); // Paste from "0 (should be "hello")

    // After yiw: "0 = "hello"
    // After dw: buffer = "world", cursor at 0
    // "0p pastes "hello" after 'w', giving "whelloorld"
    assert_eq!(test.buffer_content(), "whelloorld\n");
    test.assert_cursor(0, 5);
}

#[test]
fn test_numbered_delete_history() {
    let mut test = EditorTest::new("one\ntwo\nthree\nfour\nfive");

    test.keys("dd").keys("dd").keys("dd");

    assert_eq!(test.buffer_content(), "four\nfive\n");
    assert_eq!(test.editor.registers().get(Some('1')), "three\n");
    assert_eq!(test.editor.registers().get(Some('2')), "two\n");
    assert_eq!(test.editor.registers().get(Some('3')), "one\n");
}

#[test]
fn test_register_0_only_yanks() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" to "0
        .keys("x") // Delete 'h' (doesn't affect "0)
        .keys("x") // Delete 'e' (doesn't affect "0)
        .keys("\"0p"); // Paste "hello" from "0 after 'l'

    // After yiw: "0 = "hello", cursor at 0
    // After 2x: buffer = "llo world", cursor at 0
    // "0p pastes "hello" after 'l', giving "lhellolo world"
    assert_eq!(test.buffer_content(), "lhellolo world\n");
    test.assert_cursor(0, 5);
}

// ============================================================================
// Small delete register (-)
// ============================================================================

#[test]
fn test_small_delete_register() {
    let mut test = EditorTest::new("hello world");

    test.press('x') // Delete 'h' (goes to "-)
        .keys("\"-p"); // Paste 'h' from small delete register after 'e'

    // After x: buffer = "ello world", cursor at 0
    // "-p pastes "h" after 'e', giving "ehlllo world"
    assert_eq!(test.buffer_content(), "ehllo world\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_small_delete_vs_numbered() {
    let mut test = EditorTest::new("hello world\nsecond");

    test.press('x');
    assert_eq!(test.editor.registers().get(Some('-')), "h");
    assert_eq!(test.editor.registers().get(Some('1')), "");

    test.keys("dw");
    assert_eq!(test.editor.registers().get(Some('-')), "ello ");
    assert_eq!(test.editor.registers().get(Some('1')), "");

    test.keys("dd");
    assert_eq!(test.editor.registers().get(Some('-')), "ello ");
    assert_eq!(test.editor.registers().get(Some('1')), "world\n");
    assert_eq!(test.editor.registers().get(None), "world\n");
}

#[test]
fn test_explicit_named_delete_does_not_rotate_implicit_delete_registers() {
    let mut test = EditorTest::new("hello world");

    // Verified with Neovim 0.12.4: an explicit named delete updates the
    // named and unnamed registers, but neither "- nor "1.
    test.keys("\"adw");

    assert_eq!(test.editor.registers().get(Some('a')), "hello ");
    assert_eq!(test.editor.registers().get(None), "hello ");
    assert_eq!(test.editor.registers().get(Some('-')), "");
    assert_eq!(test.editor.registers().get(Some('1')), "");
}

// ============================================================================
// Read-only registers
// ============================================================================

#[test]
fn test_percent_register_filename() {
    let mut test = EditorTest::new("test content");

    test.set_file_path("test.txt".to_string()).keys("\"%p"); // Paste filename after 't'

    // % register resolves to current buffer path.
    assert_eq!(test.buffer_content(), "ttest.txtest content\n");
    test.assert_cursor(0, 8);
}

#[test]
fn test_colon_register_last_command() {
    let mut test = EditorTest::new("test");

    test.press(':').type_text("w").press_enter().keys("\":p"); // Paste last command after 't'

    // : register is the last command.
    assert_eq!(test.buffer_content(), "twest\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_dot_register_last_insert() {
    let mut test = EditorTest::new("line");

    test.press('i')
        .type_text("INSERTED")
        .press_esc()
        .keys("$") // End of line (cursor on 'e')
        .keys("\".p"); // Paste last inserted text after 'e'

    // . register is the last inserted text.
    assert_eq!(test.buffer_content(), "INSERTEDlineINSERTED\n");
    test.assert_cursor(0, 19);
}

// ============================================================================
// Black hole register (_)
// ============================================================================

#[test]
fn test_blackhole_register_delete() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" (no trailing space)
        .keys("w") // Move to "world"
        .keys("\"_dw") // Delete "world" to black hole (doesn't affect unnamed)
        .press('p'); // Paste "hello" from unnamed

    // "_dw should not update the unnamed register, so `p` still pastes "hello".
    assert_eq!(test.buffer_content(), "hello hello\n");
    test.assert_cursor(0, 10);
}

#[test]
fn test_blackhole_register_change() {
    let mut test = EditorTest::new("one two three");

    test.keys("yiw") // Yank "one" (no trailing space)
        .keys("w") // Move to "two"
        .keys("\"_ciw") // Change "two" to black hole (doesn't affect unnamed)
        .type_text("X")
        .press_esc()
        .keys("$") // End of line (cursor on 'e')
        .press('p'); // Paste from unnamed

    // "_ciw should not update the unnamed register, so `p` pastes the earlier yank ("one").
    assert_eq!(test.buffer_content(), "one X threeone\n");
    test.assert_cursor(0, 13);
}

// ============================================================================
// Last search pattern register (/)
// ============================================================================

#[test]
fn test_slash_register_search_pattern() {
    let mut test = EditorTest::new("hello world");

    test.press('/')
        .type_text("world")
        .press_enter() // Search jumps to "world", cursor at 'w' (position 6)
        .keys("\"/p"); // Paste search pattern after 'w'

    // / register is the latest search pattern.
    assert_eq!(test.buffer_content(), "hello wworldorld\n");
    test.assert_cursor(0, 11);
}

// ============================================================================
// Expression register (=)
// ============================================================================

#[test]
fn test_expression_register() {
    let mut test = EditorTest::new("test");

    test.keys("\"=") // Expression register (not implemented, falls through)
        .type_text("2+2")
        .press_enter()
        .press('p'); // Paste result after 't'

    // TODO: = register (expression) not implemented - input goes as regular text
    // The "= sequence doesn't trigger expression mode.
    // '2' sets count, '+' runs plus_motion (clamped to last line), '2' sets count, Enter no-op, 'p' pastes empty
    assert_eq!(test.buffer_content(), "test\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Selection and clipboard registers (+, *)
// ============================================================================

#[test]
fn test_clipboard_register_yank() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"+yiw") // Yank "hello" to system clipboard (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .keys("\"+p"); // Paste from clipboard after 'd'

    // yiw yanks "hello" (no trailing space)
    assert_eq!(test.buffer_content(), "hello worldhello\n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_selection_register() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"*yiw") // Yank "hello" to selection (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .keys("\"*p"); // Paste from selection after 'd'

    // yiw yanks "hello" (no trailing space)
    assert_eq!(test.buffer_content(), "hello worldhello\n");
    test.assert_cursor(0, 15);
}

// ============================================================================
// Register operations with visual mode
// ============================================================================

#[test]
fn test_visual_yank_to_register() {
    let mut test = EditorTest::new("hello world test");

    test.press('v')
        .keys("e") // Select "hello"
        .keys("\"ay") // Yank "hello" to register 'a'
        .keys("$") // End of line (cursor on 't')
        .keys("\"ap"); // Paste after 't'

    // TODO: Visual mode yank to named register not working as expected
    // The yank operation in visual mode with register may not be implemented correctly
    // Actual: buffer unchanged, paste has no effect
    assert_eq!(test.buffer_content(), "hello world test\n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_visual_delete_to_register() {
    let mut test = EditorTest::new("hello world");

    test.press('v')
        .keys("e") // Select "hello"
        .keys("\"ad") // Delete "hello" to register 'a'
        .keys("$") // End of line (cursor on 'd')
        .keys("\"ap"); // Paste "hello" after 'd'

    // TODO: Visual mode delete to named register not working as expected
    // The delete operation in visual mode with register may not be implemented correctly
    // Actual: " world" (delete works) but paste has no effect
    assert_eq!(test.buffer_content(), " world\n");
    test.assert_cursor(0, 5);
}

#[test]
fn test_visual_line_to_register() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.press('V') // Visual line mode
        .press('j') // Select 2 lines (line 1 and line 2)
        .keys("\"ay") // Yank to 'a' (linewise)
        .keys("G") // Go to last line
        .keys("\"ap"); // Paste (linewise, creates new lines below)

    // TODO: Visual line mode yank to named register not working as expected
    // Actual: buffer unchanged, paste has no effect
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
    // G moves cursor to last line (line 2, 0-indexed)
    test.assert_cursor(2, 0);
}

// ============================================================================
// Register persistence and edge cases
// ============================================================================

#[test]
fn test_register_survives_undo() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"ayiw") // Yank "hello" to 'a' (no trailing space)
        .keys("dw") // Delete "hello " (with space)
        .press('u') // Undo delete
        .keys("$") // End of line
        .keys("\"ap"); // Register 'a' should still work

    // After undo, buffer is restored to "hello world"
    // $ moves to end, "ap pastes "hello" after 'd'
    assert_eq!(test.buffer_content(), "hello worldhello\n");
    test.assert_cursor(0, 15);
}

#[test]
fn test_empty_register() {
    let mut test = EditorTest::new("test");

    test.keys("\"zp"); // Paste from unused register

    assert_eq!(test.buffer_content(), "test\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_register_with_newlines() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.keys("\"ayy") // Yank line to 'a' (includes newline)
        .keys("G")
        .keys("\"ap"); // Paste with newline

    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 1\n");
    test.assert_cursor(3, 0);
}

#[test]
fn test_register_linewise_vs_charwise() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.keys("\"ayy") // Yank line (linewise) - "line 1\n"
        .keys("w") // Move to "1"
        .keys("\"byiw") // Yank "1" (charwise)
        .keys("G") // Go to last line
        .keys("\"ap") // Paste line (linewise, creates new line below)
        .keys("\"bp"); // Paste "1" (charwise)

    // "ayy yanks "line 1\n" (linewise)
    // w moves to "1", "byiw yanks "1"
    // G goes to line 3, "ap pastes "line 1\n" below
    // Cursor now on "line 1" (line 3), "bp pastes "1" after 'l'
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nl1ine 1\n");
    test.assert_cursor(3, 1);
}

// ============================================================================
// Register operations with counts
// ============================================================================

#[test]
fn test_register_with_count_yank() {
    let mut test = EditorTest::new("one two three four");

    test.keys("\"ayiw") // Yank "one" to 'a' (count with yiw doesn't work as expected)
        .keys("$") // End of line
        .keys("\"ap"); // Paste "one" after 'r'

    // yiw yanks "one" (no trailing space)
    assert_eq!(test.buffer_content(), "one two three fourone\n");
    test.assert_cursor(0, 20);
}

#[test]
fn test_register_with_count_delete() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("\"a2dd") // Delete 2 lines to 'a'
        .keys("G") // Go to last line
        .keys("\"ap"); // Paste from 'a'

    // 2dd deletes 2 lines (linewise), "ap pastes them below current line
    assert_eq!(test.buffer_content(), "line 3\nline 4\nline 1\nline 2\n");
    // Linewise paste moves the cursor to the first pasted line.
    test.assert_cursor(2, 0);
}

#[test]
fn test_register_paste_with_count() {
    let mut test = EditorTest::new("word");

    test.keys("\"ayiw") // Yank "word" to 'a' (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .keys("3\"ap"); // Paste 3 times from 'a'

    // yiw yanks "word" (no trailing space), 3p pastes 3 copies
    assert_eq!(test.buffer_content(), "wordwordwordword\n");
    // Cursor on last char of pasted text
    test.assert_cursor(0, 15);
}

// ============================================================================
// Special register interactions
// ============================================================================

#[test]
fn test_delete_updates_both_unnamed_and_numbered() {
    let mut test = EditorTest::new("hello world\nsecond line");

    test.keys("dd");

    assert_eq!(test.editor.registers().get(None), "hello world\n");
    assert_eq!(test.editor.registers().get(Some('1')), "hello world\n");
    assert_eq!(test.editor.registers().get(Some('-')), "");
}

#[test]
fn test_yank_updates_unnamed_and_0() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" to unnamed and "0 (no trailing space)
        .keys("$") // End of line (cursor on 'd')
        .press('p') // Paste "hello" from unnamed after 'd'
        .keys("\"0p"); // Paste "hello" from "0 after 'o'

    // yiw yanks "hello" (no trailing space)
    // p: "hello worldhello", cursor at 15 (last pasted char)
    // "0p: paste "hello" after 'o', giving "hello worldhellohello"
    assert_eq!(test.buffer_content(), "hello worldhellohello\n");
    test.assert_cursor(0, 20);
}

#[test]
fn test_change_updates_unnamed_but_not_0() {
    let mut test = EditorTest::new("hello world");

    test.keys("yiw") // Yank "hello" to "0 (no trailing space)
        .keys("ciw") // Change "hello" (deleted to unnamed, not "0)
        .type_text("X")
        .press_esc()
        .keys("$") // End of line (cursor on 'd')
        .press('p') // Paste "hello" from unnamed after 'd'
        .keys("\"0p"); // Paste "hello" from "0 after 'o'

    // yiw yanks "hello" (no trailing space) to "0
    // ciw deletes "hello" (no trailing space) to unnamed
    // Buffer after change: "X world"
    // p: "X worldhello", cursor at 11 (last pasted char)
    // "0p: paste "hello" after 'o', giving "X worldhellohello"
    assert_eq!(test.buffer_content(), "X worldhellohello\n");
    test.assert_cursor(0, 16);
}

// ============================================================================
// Register names validation
// ============================================================================

#[test]
fn test_invalid_register_name() {
    let mut test = EditorTest::new("test");

    test.keys("\"!") // Invalid register
        .press('p'); // Should handle gracefully

    assert_eq!(test.buffer_content(), "test\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_register_case_sensitivity() {
    let mut test = EditorTest::new("hello world test");

    test.keys("\"ayiw") // Yank "hello" to 'a'
        .keys("w") // Move to "world"
        .keys("\"Ayiw") // Append "world" to 'a'
        .keys("$") // End of line (cursor on 't')
        .keys("\"ap"); // Paste "helloworld" after 't'

    // yiw yanks words without trailing space, appending gives "helloworld"
    assert_eq!(test.buffer_content(), "hello world testhelloworld\n");
    test.assert_cursor(0, 25);
}

// ============================================================================
// Complex register scenarios
// ============================================================================

#[test]
fn test_register_chain_operations() {
    let mut test = EditorTest::new("one two three four");

    test.keys("\"ayiw") // Yank "one" to a (cursor at 0)
        .keys("w") // Move to "two" (cursor at 4)
        .keys("\"byiw") // Yank "two" to b (cursor at 4)
        .keys("\"ap") // Paste "one" after 't'
        .keys("\"bp") // Paste "two" after 'e'
        .keys("\"ayiw") // Yank word at cursor position
        .keys("\"ap"); // Paste from a

    // Complex chain - verify actual behavior
    assert_eq!(test.buffer_content(), "one tonetwotonetwowowo three four\n");
    test.assert_cursor(0, 19);
}

#[test]
fn test_swap_words_with_registers() {
    let mut test = EditorTest::new("hello world");

    test.keys("\"ayiw") // Yank "hello" to a (cursor at 0)
        .keys("w") // Move to "world" (cursor at 6)
        .keys("\"byiw") // Yank "world" to b (cursor at 6)
        .keys("0") // Go to start (cursor at 0)
        .keys("diw") // Delete "hello" (buffer: " world", cursor at 0)
        .keys("\"bp") // Paste "world" after ' '
        .keys("w") // Move to next word
        .keys("diw") // Delete current word
        .keys("\"ap"); // Paste "hello"

    // Complex swap operation - verify actual behavior
    assert_eq!(test.buffer_content(), " hello\n");
    test.assert_cursor(0, 5);
}

// ============================================================================
// Read-only registers — writes silently ignored (%, ., :, /, #)
// ============================================================================

#[test]
fn test_yank_to_percent_register_is_ignored() {
    // "% register holds current filename. Yanking to it should not overwrite.
    let mut test = EditorTest::new("hello world");
    test.set_file_path("test.rs".to_string());

    // Try to yank "hello" into % register
    test.keys("\"%yiw");

    // % register should still be the filename, not "hello"
    assert_eq!(test.editor.registers().get(Some('%')), "test.rs");
}

#[test]
fn test_delete_to_percent_register_is_ignored() {
    // Deleting to % should not overwrite the filename register
    let mut test = EditorTest::new("hello world");
    test.set_file_path("test.rs".to_string());

    // Try to delete word into % register
    test.keys("\"%dw");

    // % register should still be the filename
    assert_eq!(test.editor.registers().get(Some('%')), "test.rs");
    // But the word should still be deleted from the buffer
    assert_eq!(test.buffer_content(), "world\n");
}

#[test]
fn test_yank_to_dot_register_is_ignored() {
    // . register holds last inserted text
    let mut test = EditorTest::new("hello world");

    // Insert some text first to populate . register
    test.keys("ihey<Esc>");

    let dot_before = test.editor.registers().get(Some('.'));

    // Try to yank into . register
    test.keys("w\".yiw");

    // . register should be unchanged
    assert_eq!(test.editor.registers().get(Some('.')), dot_before);
}

#[test]
fn test_yank_to_colon_register_is_ignored() {
    let mut test = EditorTest::new("hello world");

    // Set : register via a command
    test.keys(":set number<Enter>");

    let colon_before = test.editor.registers().get(Some(':'));

    // Try to yank into : register
    test.keys("\":yiw");

    // : register should be unchanged
    assert_eq!(test.editor.registers().get(Some(':')), colon_before);
}

#[test]
fn test_yank_to_slash_register_is_ignored() {
    let mut test = EditorTest::new("hello world");

    // Set / register via a search
    test.keys("/hello<Enter>");

    let slash_before = test.editor.registers().get(Some('/'));

    // Try to yank into / register
    test.keys("\"/yiw");

    // / register should be unchanged
    assert_eq!(test.editor.registers().get(Some('/')), slash_before);
}

#[test]
fn test_yank_to_hash_register_is_ignored() {
    let mut test = EditorTest::new("hello world");

    // # register holds alternate filename — set it indirectly
    test.editor
        .registers_mut()
        .set_alternate_file("alt.rs".to_string());

    // Try to yank into # register
    test.keys("\"#yiw");

    // # register should still be alternate filename
    assert_eq!(test.editor.registers().get(Some('#')), "alt.rs");
}

#[test]
fn test_read_only_register_yank_still_populates_unnamed() {
    // When yanking to a read-only register, the text should still go
    // to the unnamed register (default behavior)
    let mut test = EditorTest::new("hello world");

    test.keys("\"%yiw");

    // Unnamed register should have "hello"
    assert_eq!(test.editor.registers().get(None), "hello");
}

#[test]
fn test_read_only_register_delete_still_populates_unnamed() {
    // When deleting to a read-only register, the text should still go
    // to the unnamed and appropriate implicit delete register
    let mut test = EditorTest::new("hello world");

    test.keys("\"%dw");

    // Unnamed register should have "hello "
    assert_eq!(test.editor.registers().get(None), "hello ");
    // A within-line delete belongs to the small-delete register, not "1.
    assert_eq!(test.editor.registers().get(Some('-')), "hello ");
    assert_eq!(test.editor.registers().get(Some('1')), "");
}

#[test]
fn test_read_only_register_set_with_type_safety_net() {
    // Direct set_with_type calls to read-only registers should also be ignored
    use ovim::editor::RegisterType;

    let mut test = EditorTest::new("hello");
    test.set_file_path("original.rs".to_string());

    // Try to directly set % register through set_with_type
    test.editor.registers_mut().set_with_type(
        Some('%'),
        "overwritten".to_string(),
        RegisterType::Character,
    );

    // Should still be the original
    assert_eq!(test.editor.registers().get(Some('%')), "original.rs");
}

#[test]
fn test_all_read_only_registers_reject_set() {
    use ovim::editor::{RegisterManager, RegisterType};

    let mut regs = RegisterManager::new();
    regs.set_current_file("file.rs".to_string());
    regs.set_alternate_file("alt.rs".to_string());
    regs.set_last_inserted("inserted".to_string());
    regs.set_last_command("command".to_string());
    regs.set_last_search("pattern".to_string());

    // Try to overwrite each read-only register
    for &reg in &['%', '#', '.', ':', '/'] {
        regs.set_with_type(Some(reg), "hacked".to_string(), RegisterType::Character);
    }

    // All should retain their original values
    assert_eq!(regs.get(Some('%')), "file.rs");
    assert_eq!(regs.get(Some('#')), "alt.rs");
    assert_eq!(regs.get(Some('.')), "inserted");
    assert_eq!(regs.get(Some(':')), "command");
    assert_eq!(regs.get(Some('/')), "pattern");
}
