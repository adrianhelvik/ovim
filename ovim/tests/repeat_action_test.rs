mod helpers;
use helpers::EditorTest;

// ============================================================================
// Toggle case (~) dot-repeat
// ============================================================================

#[test]
fn test_tilde_dot_repeat_basic() {
    let mut test = EditorTest::new("hello world");

    test.press('~') // Toggle 'h' → 'H', cursor moves to 'e'
        .keys("l") // Move to 'l'
        .press('.'); // Toggle 'l' → 'L'

    assert_eq!(test.buffer_content(), "HeLlo world\n");
}

#[test]
fn test_tilde_dot_repeat_sequential() {
    let mut test = EditorTest::new("abcdef");

    test.press('~'); // 'a' → 'A', cursor at 'b'
    test.press('.'); // 'b' → 'B', cursor at 'c'
    test.press('.'); // 'c' → 'C', cursor at 'd'

    assert_eq!(test.buffer_content(), "ABCdef\n");
    test.assert_cursor(0, 3);
}

#[test]
fn test_tilde_with_count_dot_repeat() {
    let mut test = EditorTest::new("hello\nworld");

    test.keys("3~"); // Toggle 'hel' → 'HEL', cursor at 'l'
    test.keys("j0"); // Move to start of next line
    test.press('.'); // Repeat: toggle 'wor' → 'WOR'

    assert_eq!(test.buffer_content(), "HELlo\nWORld\n");
}

#[test]
fn test_tilde_dot_repeat_on_different_line() {
    let mut test = EditorTest::new("abc\nxyz");

    test.press('~'); // 'a' → 'A'
    test.keys("j0"); // Move to next line start
    test.press('.'); // Toggle 'x' → 'X'

    assert_eq!(test.buffer_content(), "Abc\nXyz\n");
}

#[test]
fn test_tilde_dot_repeat_undo() {
    let mut test = EditorTest::new("hello");

    test.press('~'); // 'h' → 'H'
    test.press('.'); // 'e' → 'E'
    test.press('u'); // Undo the dot-repeat

    assert_eq!(test.buffer_content(), "Hello\n");
}

#[test]
fn test_tilde_dot_repeat_undo_both() {
    let mut test = EditorTest::new("hello");

    test.press('~'); // 'h' → 'H'
    test.press('.'); // 'e' → 'E'
    test.press('u'); // Undo dot-repeat → 'Hello'
    test.press('u'); // Undo original ~ → 'hello'

    assert_eq!(test.buffer_content(), "hello\n");
}

#[test]
fn test_tilde_dot_repeat_undo_redo() {
    let mut test = EditorTest::new("hello");

    test.press('~'); // 'h' → 'H'
    test.press('.'); // 'e' → 'E'
    test.press('u'); // Undo → 'Hello'
    test.keys("<C-r>"); // Redo → 'HEllo'

    assert_eq!(test.buffer_content(), "HEllo\n");
}

// ============================================================================
// Join lines (J / gJ) dot-repeat
// ============================================================================

#[test]
fn test_join_with_count_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");

    // Vim's [count]J joins count LINES (count-1 joins) — OV-00290.
    test.keys("3J"); // Join 3 lines: "a b c"
    test.press('.'); // Repeat: join 3 lines again: "a b c d e"

    assert_eq!(test.buffer_content(), "a b c d e\nf\n");
}

#[test]
fn test_join_dot_repeat_multiple() {
    let mut test = EditorTest::new("line1\nline2\nline3\nline4");

    test.press('J'); // "line1 line2\nline3\nline4"
    test.press('.'); // "line1 line2 line3\nline4"
    test.press('.'); // "line1 line2 line3 line4"

    assert_eq!(test.buffer_content(), "line1 line2 line3 line4\n");
}

#[test]
fn test_gj_dot_repeat_multiple() {
    let mut test = EditorTest::new("aa\nbb\ncc\ndd");

    test.keys("gJ"); // "aabb\ncc\ndd"
    test.press('.'); // "aabbcc\ndd"
    test.press('.'); // "aabbccdd"

    assert_eq!(test.buffer_content(), "aabbccdd\n");
}

#[test]
fn test_join_dot_repeat_undo() {
    let mut test = EditorTest::new("a\nb\nc\nd");

    test.press('J'); // "a b\nc\nd"
    test.press('.'); // "a b c\nd"
    test.press('u'); // Undo dot-repeat → "a b\nc\nd"

    assert_eq!(test.buffer_content(), "a b\nc\nd\n");
}

#[test]
fn test_join_dot_repeat_at_last_line() {
    let mut test = EditorTest::new("only line");

    test.press('J'); // Nothing to join
    test.press('.'); // Still nothing

    assert_eq!(test.buffer_content(), "only line\n");
}

#[test]
fn test_join_dot_repeat_two_lines_left() {
    let mut test = EditorTest::new("aa\nbb");

    test.press('J'); // "aa bb"
    test.press('.'); // Nothing more to join

    assert_eq!(test.buffer_content(), "aa bb\n");
}

// ============================================================================
// Indent (>>) dot-repeat
// ============================================================================

#[test]
fn test_indent_dot_repeat_preserves_line_count() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys(">>");
    test.keys("j");
    test.press('.');
    test.keys("j");
    test.press('.');

    // All 3 lines should be indented independently
    assert_eq!(test.buffer_content(), "    aaa\n    bbb\n    ccc\n");
}

#[test]
fn test_indent_multiline_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");

    test.keys(">j"); // Indent lines 0-1 (2 lines)
    test.keys("jj"); // Move to line 2 (c)
                     // NOTE: the cursor is on line 3 now (after jj from line 1)
    test.press('.'); // Repeat: indent 2 lines starting here

    // Lines 0-1 indented by first operation, lines 3-4 by repeat
    let content = test.buffer_content();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    assert!(lines[0].starts_with("    ")); // "    a"
    assert!(lines[1].starts_with("    ")); // "    b"
}

#[test]
fn test_indent_dot_repeat_at_eof() {
    let mut test = EditorTest::new("line1\nline2");

    test.keys(">>"); // Indent line 0
    test.keys("j"); // Move to last line
    test.press('.'); // Repeat indent on last line

    assert_eq!(test.buffer_content(), "    line1\n    line2\n");
}

#[test]
fn test_indent_dot_repeat_undo() {
    let mut test = EditorTest::new("aaa\nbbb");

    test.keys(">>"); // Indent line 0
    test.keys("j");
    test.press('.'); // Indent line 1
    test.press('u'); // Undo the dot-repeat only

    assert_eq!(test.buffer_content(), "    aaa\nbbb\n");
}

#[test]
fn test_indent_dot_repeat_undo_redo() {
    let mut test = EditorTest::new("aaa\nbbb");

    test.keys(">>");
    test.keys("j");
    test.press('.');
    test.press('u'); // Undo dot-repeat
    test.keys("<C-r>"); // Redo

    assert_eq!(test.buffer_content(), "    aaa\n    bbb\n");
}

// ============================================================================
// Dedent (<<) dot-repeat
// ============================================================================

#[test]
fn test_dedent_dot_repeat_sequential() {
    let mut test = EditorTest::new("    aaa\n    bbb\n    ccc");

    test.keys("<<");
    test.keys("j");
    test.press('.');
    test.keys("j");
    test.press('.');

    assert_eq!(test.buffer_content(), "aaa\nbbb\nccc\n");
}

#[test]
fn test_dedent_multiline_dot_repeat() {
    let mut test = EditorTest::new("    a\n    b\n    c\n    d");

    test.keys("<j"); // Dedent 2 lines (a, b)
    test.keys("jj"); // Move down
    test.press('.'); // Repeat dedent on 2 more lines

    let content = test.buffer_content();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    assert_eq!(lines[0], "a");
    assert_eq!(lines[1], "b");
}

#[test]
fn test_dedent_dot_repeat_on_unindented_line() {
    let mut test = EditorTest::new("    indented\nnot indented");

    test.keys("<<"); // Dedent first line
    test.keys("j");
    test.press('.'); // Repeat on already-unindented line — no-op

    assert_eq!(test.buffer_content(), "indented\nnot indented\n");
}

#[test]
fn test_dedent_dot_repeat_undo() {
    let mut test = EditorTest::new("    aaa\n    bbb");

    test.keys("<<");
    test.keys("j");
    test.press('.');
    test.press('u');

    assert_eq!(test.buffer_content(), "aaa\n    bbb\n");
}

// ============================================================================
// Mutual exclusion: RepeatAction vs Change-based repeat
// ============================================================================

#[test]
fn test_change_overrides_repeat_action() {
    // After a RepeatAction operation (>>), a Change-based operation (dd) should
    // become the new dot-repeat target.
    let mut test = EditorTest::new("aaa\nbbb\nccc\nddd");

    test.keys(">>"); // RepeatAction::IndentLines
    test.keys("j");
    test.keys("dd"); // Change-based delete line
    test.keys("j"); // Now on "ddd"
    test.press('.'); // Should repeat dd (not >>)

    // Line 0: "    aaa", line 1 deleted (bbb), line 2 (ddd) deleted
    assert_eq!(test.buffer_content(), "    aaa\nccc\n");
}

#[test]
fn test_repeat_action_overrides_change() {
    // After a Change-based operation (dd), a RepeatAction operation (>>) should
    // become the new dot-repeat target.
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys("dd"); // Delete "aaa"
    test.keys(">>"); // Indent "bbb" (now line 0)
    test.keys("j"); // Move to "ccc"
    test.press('.'); // Should repeat >> (not dd)

    assert_eq!(test.buffer_content(), "    bbb\n    ccc\n");
}

#[test]
fn test_tilde_overrides_insert_repeat() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('i').type_text("X").press_esc(); // Insert "X" at start
    test.keys("j");
    test.press('~'); // Toggle 'w' → 'W'
    test.keys("j");
    // Not enough lines, so test at end of "World"
    // Just verify the tilde is the repeat target, not the insert
    // by checking that ~ set the repeat action:
    // Go back to first character of line 1
    test.keys("0");
    test.press('.'); // Should toggle case (not insert "X")

    // "World" → "world" (toggled 'W' back)
    assert_eq!(test.buffer_content(), "Xhello\nworld\n");
}

// ============================================================================
// g; after RepeatAction operations
// ============================================================================

#[test]
fn test_g_semicolon_after_indent() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys(">>"); // Indent line 0
    test.keys("G"); // Go to last line
    test.keys("g;"); // Jump to last edit position

    test.assert_cursor(0, 0);
}

#[test]
fn test_g_semicolon_after_join() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.press('J'); // Join lines 0+1
    test.keys("G"); // Go to last line
    test.keys("g;"); // Jump back

    test.assert_cursor(0, 0);
}

#[test]
fn test_g_semicolon_after_tilde() {
    let mut test = EditorTest::new("hello\nworld");

    test.keys("w"); // Move to next word (doesn't exist, stays or goes to next line)
    test.keys("j"); // Go to line 1
    test.press('~'); // Toggle 'w' → 'W'
    test.keys("gg"); // Go to top
    test.keys("g;"); // Jump to last edit

    test.assert_cursor(1, 0);
}

#[test]
fn test_g_semicolon_after_dedent() {
    let mut test = EditorTest::new("    indented\nother");

    test.keys("<<"); // Dedent
    test.keys("j"); // Move to next line
    test.keys("g;"); // Jump to last edit

    test.assert_cursor(0, 0);
}

#[test]
fn test_g_semicolon_after_repeat_action_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys(">>"); // Original edit at line 0
    test.keys("j."); // Dot-repeat on line 1
    test.keys("Gg;"); // Move to older changelist entry
    test.keys("g,"); // Move forward to newer changelist entry

    assert_eq!(test.cursor().0, 1, "g, should reach the dot-repeat line");
}

#[test]
fn test_g_semicolon_after_change_dot_repeat() {
    let mut test = EditorTest::new("one two\nthree four\nfive six");

    test.keys("dw"); // Original edit at line 0
    test.keys("j."); // Dot-repeat at line 1
    test.keys("Gg;"); // Move to older changelist entry
    test.keys("g,"); // Move forward to newer changelist entry

    assert_eq!(test.cursor().0, 1, "g, should reach the dot-repeat line");
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_tilde_dot_repeat_at_end_of_short_line() {
    let mut test = EditorTest::new("a\nbcdef");

    test.keys("3~"); // Toggle on line with only 1 char
    test.keys("j0"); // Move to next line start
    test.press('.'); // Repeat 3~ on longer line

    // First line: 'a' → 'A' (only 1 char so count 3 only gets 1)
    // Second line: 'bcd' → 'BCD'
    let content = test.buffer_content();
    assert!(content.starts_with("A\n"));
    assert!(content.contains("BCDef"));
}

#[test]
fn test_indent_dot_repeat_on_empty_line() {
    let mut test = EditorTest::new("bar\n\nfoo");

    test.keys(">>"); // Indent "bar" (sets repeat action)
    test.keys("jj"); // Move past empty line to "foo"
    test.press('.'); // Repeat indent on "foo"

    let content = test.buffer_content();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    assert_eq!(lines[0], "    bar");
    assert_eq!(lines[1], ""); // Empty line untouched
    assert_eq!(lines[2], "    foo");
}

#[test]
fn test_dedent_dot_repeat_already_at_col0() {
    let mut test = EditorTest::new("abc\ndef");

    test.keys("<<"); // Dedent unindented line — no-op
    test.keys("j");
    test.press('.'); // Repeat dedent — still no-op

    assert_eq!(test.buffer_content(), "abc\ndef\n");
}

#[test]
fn test_join_dot_repeat_with_trailing_whitespace() {
    let mut test = EditorTest::new("hello   \n   world\nfoo");

    test.press('J'); // Join: "hello    world\nfoo" (Vim trims and adds single space)
    test.press('.'); // Repeat join

    let content = test.buffer_content();
    // Should be a single line after two joins
    assert_eq!(content.lines().count(), 1);
}

#[test]
fn test_indent_double_dot_repeat() {
    // >> then . twice = three levels of indent should NOT stack on same line
    // but should work on sequential lines
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys(">>");
    test.keys("j");
    test.press('.');
    test.keys("j");
    test.press('.');

    assert_eq!(test.buffer_content(), "    aaa\n    bbb\n    ccc\n");
}

#[test]
fn test_dedent_double_indent_dot_repeat() {
    // Indent a line twice, then dedent and repeat
    let mut test = EditorTest::new("        aaa\n        bbb");

    test.keys("<<"); // Remove 4 spaces → "    aaa"
    test.keys("j");
    test.press('.'); // Remove 4 spaces → "    bbb"

    assert_eq!(test.buffer_content(), "    aaa\n    bbb\n");
}

// ============================================================================
// Combined operations: undo across RepeatAction and Change boundaries
// ============================================================================

#[test]
fn test_undo_across_repeat_action_and_change() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");

    test.keys(">>"); // Indent (RepeatAction undo entry)
    test.keys("j");
    test.keys("dd"); // Delete line (Change undo entry)
    test.press('u'); // Undo dd → "bbb" restored
    test.press('u'); // Undo >> → "aaa" unindented

    assert_eq!(test.buffer_content(), "aaa\nbbb\nccc\n");
}

#[test]
fn test_redo_repeat_action_operations() {
    let mut test = EditorTest::new("aaa\nbbb");

    test.keys(">>"); // Indent line 0
    test.press('u'); // Undo
    test.keys("<C-r>"); // Redo

    assert_eq!(test.buffer_content(), "    aaa\nbbb\n");
}

#[test]
fn test_undo_tilde_dot_repeat_is_atomic() {
    // Dot-repeat of ~ should undo as a single unit
    let mut test = EditorTest::new("abcde");

    test.keys("3~"); // Toggle 'abc' → 'ABC'
    test.press('.'); // Toggle 'de' (only 2 chars left at pos 3)

    // After dot-repeat: "ABCDe" or "ABCDE" depending on count behavior
    // Undo should restore to state after first 3~
    test.press('u');

    assert_eq!(test.buffer_content(), "ABCde\n");
}

// ============================================================================
// Bug fix: ~ with count at end of line stops instead of re-toggling
// ============================================================================

#[test]
fn test_tilde_count_stops_at_eol_single_char() {
    // 2~ on "a" should toggle once and stop (only 1 char)
    let mut test = EditorTest::new("a");
    test.keys("2~");
    assert_eq!(test.buffer_content(), "A\n");
}

#[test]
fn test_tilde_count_stops_at_eol_two_chars() {
    // 4~ on "ab" should toggle both chars and stop
    let mut test = EditorTest::new("ab");
    test.keys("4~");
    assert_eq!(test.buffer_content(), "AB\n");
}

#[test]
fn test_tilde_count_at_last_char() {
    // 3~ at last char of "hello" should toggle just 'o'
    let mut test = EditorTest::new("hello");
    test.keys("$3~");
    assert_eq!(test.buffer_content(), "hellO\n");
}

#[test]
fn test_tilde_count_dot_repeat_stops_at_eol() {
    // Repeat of 2~ should also stop at end of line
    let mut test = EditorTest::new("ab\ncd");

    test.keys("2~"); // Toggle 'ab' → 'AB'
    test.keys("j0"); // Move to 'c'
    test.press('.'); // Repeat 2~: toggle 'cd' → 'CD'

    assert_eq!(test.buffer_content(), "AB\nCD\n");
}

// ============================================================================
// Bug #5: Multi-byte character toggle case (ß → SS)
// ============================================================================

#[test]
fn test_tilde_on_eszett() {
    // German ß uppercases to SS (1 char → 2 chars)
    let mut test = EditorTest::new("straße");
    test.keys("$"); // Move to 'e' (last char)
                    // Move back to ß
    test.keys("h"); // Now on 'ß'
    test.press('~'); // Toggle ß → SS (or ss?)

    // ß.is_lowercase() is true → uppercase to "SS"
    assert_eq!(test.buffer_content(), "straSSe\n");
}

#[test]
fn test_tilde_on_uppercase_after_eszett_expansion() {
    // ß → SS: cursor should advance past both new chars
    let mut test = EditorTest::new("aß");
    test.keys("l"); // Move to ß
    test.press('~'); // Toggle ß → SS

    assert_eq!(test.buffer_content(), "aSS\n");
}

#[test]
fn test_tilde_count_with_multibyte() {
    // 3~ starting at 'a' in "aßc" — a→A, ß→SS, c→C
    let mut test = EditorTest::new("abc");
    test.keys("3~");
    assert_eq!(test.buffer_content(), "ABC\n");

    // Now with a non-ASCII char: ß uppercases to SS (expands line)
    let mut test2 = EditorTest::new("aßc");
    test2.keys("3~");
    assert_eq!(test2.buffer_content(), "ASSC\n");
}

// ============================================================================
// Bug #9: Cursor position after dedent
// ============================================================================

#[test]
fn test_dedent_cursor_beyond_new_line_length() {
    // Cursor at col 7, dedent removes 4 spaces — cursor should be clamped
    let mut test = EditorTest::new("    content");
    test.keys("$"); // Move to end of "    content" (col 10)
    test.keys("<<"); // Dedent: "content" (7 chars)
    let after = test.cursor();

    assert_eq!(test.buffer_content(), "content\n");
    // Cursor should be valid (within line bounds)
    let line_len = "content".len();
    assert!(
        after.1 <= line_len,
        "Cursor col {} exceeds line length {}",
        after.1,
        line_len
    );
}

#[test]
fn test_dedent_dot_repeat_cursor_position() {
    // After dedent + dot-repeat, cursor should be at valid positions
    let mut test = EditorTest::new("        deep\n        indent");
    test.keys("$"); // Go to end of line
    test.keys("<<"); // Dedent first line
    test.keys("j$"); // Go to end of second line
    test.press('.'); // Repeat dedent

    let content = test.buffer_content();
    assert_eq!(content, "    deep\n    indent\n");
    // Cursor should be within "    indent" (col <= 9)
    let (_, col) = test.cursor();
    assert!(col <= 9, "Cursor col {} beyond line length", col);
}

// ============================================================================
// Bug #3: Dedent tab handling
// ============================================================================

#[test]
fn test_dedent_with_tab_indentation() {
    let mut test = EditorTest::new("\tcontent");
    test.keys("<<");

    assert_eq!(test.buffer_content(), "content\n");
}

#[test]
fn test_dedent_mixed_tabs_and_spaces() {
    // Line starts with a tab then spaces
    let mut test = EditorTest::new("\t  content");
    test.keys("<<");

    // Six visual columns move left by shiftwidth=4, then normalize to spaces.
    assert_eq!(test.buffer_content(), "  content\n");
}

#[test]
fn test_dedent_dot_repeat_with_tabs() {
    let mut test = EditorTest::new("\t\tcontent\n\t\tother");
    test.keys("<<"); // Remove one tab
    test.keys("j");
    test.press('.'); // Repeat: remove one tab from second line

    assert_eq!(test.buffer_content(), "    content\n    other\n");
}

#[test]
fn test_dedent_spaces_then_tab() {
    // Spaces followed by tab — should remove spaces, then hit tab
    let mut test = EditorTest::new("  \tcontent");
    test.keys("<<");

    // The mixed prefix occupies four visual columns, so one dedent removes it.
    assert_eq!(test.buffer_content(), "content\n");
}

// ============================================================================
// Bug #2: Indent multiline count near EOF
// ============================================================================

#[test]
fn test_indent_multiline_near_eof() {
    // >2j on the second-to-last line: should indent what's available
    let mut test = EditorTest::new("aaa\nbbb\nccc");
    test.keys("jj"); // Move to last line (ccc)
    test.keys(">j"); // >j on last line — only 1 line to indent

    let content = test.buffer_content();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    assert_eq!(lines[0], "aaa");
    assert_eq!(lines[1], "bbb");
    assert!(
        lines[2].starts_with("    "),
        "Last line should be indented: {:?}",
        lines[2]
    );
}

#[test]
fn test_indent_multiline_dot_repeat_near_eof() {
    // >j indents 2 lines, then repeat near EOF where fewer lines exist
    let mut test = EditorTest::new("aaa\nbbb\nccc\nddd");
    test.keys(">j"); // Indent lines 0-1 (2 lines)
    test.keys("jjj"); // Move to last line (ddd, now line 3)
    test.press('.'); // Repeat >j — only 1 line available (line 3)

    let content = test.buffer_content();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    // Lines 0-1 indented by original, line 3 indented by repeat
    assert!(lines[0].starts_with("    "));
    assert!(lines[1].starts_with("    "));
    assert!(
        !lines[2].starts_with("    "),
        "Line 2 should not be indented"
    );
    assert!(
        lines[3].starts_with("    "),
        "Line 3 should be indented by repeat"
    );
}
