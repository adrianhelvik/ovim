#![allow(
    non_snake_case,
    reason = "Test names intentionally mirror Vim key notation (for example G motion)."
)]

#[macro_use]
#[path = "helpers/editor_test_macro.rs"]
mod editor_test_macro;
mod helpers;
use helpers::EditorTest;

// ============================================================================
// '>' command - Indent operator
// ============================================================================

#[test]
fn test_shift_right_line() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.keys(">>"); // Indent current line

    assert_eq!(test.buffer_content(), "    line 1\nline 2\nline 3\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_shift_right_with_count() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("3>>"); // Indent 3 lines

    assert_eq!(
        test.buffer_content(),
        "    line 1\n    line 2\n    line 3\nline 4\n"
    );
    test.assert_cursor(0, 4);
}

#[test]
fn test_shift_right_already_indented() {
    let mut test = EditorTest::new("    already indented\nplain line");

    test.keys(">>"); // Indent more

    assert_eq!(
        test.buffer_content(),
        "        already indented\nplain line\n"
    );
    test.assert_cursor(0, 8);
}

// ============================================================================
// '>' with motions
// ============================================================================

#[test]
fn test_shift_right_j() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys(">j"); // Indent current and next line

    assert_eq!(
        test.buffer_content(),
        "    line 1\n    line 2\nline 3\nline 4\n"
    );
    test.assert_cursor(0, 4);
}

#[test]
fn test_shift_right_4j() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");

    test.keys(">4j"); // Indent 5 lines (current + 4 down)

    assert_eq!(
        test.buffer_content(),
        "    a\n    b\n    c\n    d\n    e\nf\n"
    );
    test.assert_cursor(0, 4);
}

#[test]
fn test_shift_right_k() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("jj") // Move to line 2
        .keys(">k"); // Indent line 2 and line 1

    assert_eq!(
        test.buffer_content(),
        "line 1\n    line 2\n    line 3\nline 4\n"
    );
    test.assert_cursor(1, 4);
}

#[test]
fn test_shift_right_2k() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne");

    test.keys("G") // Go to last line
        .keys(">2k"); // Indent 3 lines (current + 2 up)

    assert_eq!(test.buffer_content(), "a\nb\n    c\n    d\n    e\n");
    test.assert_cursor(2, 4); // Cursor goes to start line (min of current and target)
}

#[test]
fn test_shift_right_G() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4\nline 5");

    test.keys("jj") // Line 2 (0-indexed, so this is the 3rd line "line 3")
        .keys(">G"); // Indent from current line to end

    // After jj we're on line 3 (0-indexed line 2), then >G indents from there to end
    assert_eq!(
        test.buffer_content(),
        "line 1\nline 2\n    line 3\n    line 4\n    line 5\n"
    );
    test.assert_cursor(2, 4);
}

#[test]
fn test_shift_right_gg() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("G") // Last line
        .keys(">gg"); // Indent from last to first (all lines)

    assert_eq!(
        test.buffer_content(),
        "    line 1\n    line 2\n    line 3\n    line 4\n"
    );
    test.assert_cursor(0, 4); // Cursor goes to start line
}

// ============================================================================
// '<' command - Dedent operator
// ============================================================================

#[test]
fn test_shift_left_line() {
    let mut test = EditorTest::new("    indented line\n    another");

    test.keys("<<"); // Dedent current line

    assert_eq!(test.buffer_content(), "indented line\n    another\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_shift_left_with_count() {
    let mut test = EditorTest::new("    line 1\n    line 2\n    line 3");

    test.keys("3<<"); // Dedent 3 lines

    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_shift_left_no_indent() {
    let mut test = EditorTest::new("no indent");

    test.keys("<<"); // Should do nothing

    assert_eq!(test.buffer_content(), "no indent\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_shift_left_partial_indent() {
    let mut test = EditorTest::new("  two spaces");

    test.keys("<<"); // Remove indent (might go to 0 or stay at some minimum)

    assert_eq!(test.buffer_content(), "two spaces\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// '<' with motions
// ============================================================================

#[test]
fn test_shift_left_j() {
    let mut test = EditorTest::new("    line 1\n    line 2\n    line 3");

    test.keys("<j"); // Dedent current and next

    assert_eq!(test.buffer_content(), "line 1\nline 2\n    line 3\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_shift_left_3j() {
    let mut test = EditorTest::new("    a\n    b\n    c\n    d\n    e");

    test.keys("<3j"); // Dedent 4 lines (current + 3 down)

    assert_eq!(test.buffer_content(), "a\nb\nc\nd\n    e\n");
    test.assert_cursor(0, 0); // Cursor on first line after dedent
}

#[test]
fn test_shift_left_k() {
    let mut test = EditorTest::new("    line 1\n    line 2\n    line 3");

    test.keys("j").keys("<k"); // Dedent current and previous

    assert_eq!(test.buffer_content(), "line 1\nline 2\n    line 3\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_shift_left_G() {
    let mut test = EditorTest::new("    a\n    b\n    c\n    d");

    test.keys("j").keys("<G"); // Dedent from line 1 to end

    assert_eq!(test.buffer_content(), "    a\nb\nc\nd\n");
    test.assert_cursor(1, 0);
}

// ============================================================================
// Visual mode indenting
// ============================================================================

#[test]
fn test_visual_line_indent() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.press('V') // Visual line
        .keys("jj") // Select 3 lines
        .press('>'); // Indent

    assert_eq!(
        test.buffer_content(),
        "    line 1\n    line 2\n    line 3\n"
    );
    test.assert_cursor(0, 4);
}

#[test]
fn test_visual_line_dedent() {
    let mut test = EditorTest::new("    line 1\n    line 2\n    line 3");

    test.press('V').keys("j").press('<'); // Dedent

    assert_eq!(test.buffer_content(), "line 1\nline 2\n    line 3\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_visual_char_indent() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press('v') // Character visual
        .keys("jj")
        .press('>'); // Should indent affected lines

    assert_eq!(test.buffer_content(), "    hello\n    world\n    test\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_visual_reselect_indent() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.press('V')
        .keys("j")
        .press('>') // Indent once (lines 1 and 2)
        .keys("gv") // Reselect
        .press('>'); // Indent again (lines 1 and 2 again)

    // After first >, cursor goes to start line (0, 4). gv reselects using saved visual marks,
    // but since cursor moved to line 0, gv selects only line 0. Second > indents only line 0.
    assert_eq!(
        test.buffer_content(),
        "        line 1\n    line 2\nline 3\n"
    );
    test.assert_cursor(0, 8);
}

// ============================================================================
// Multiple indents
// ============================================================================

#[test]
fn test_multiple_indent() {
    let mut test = EditorTest::new("line 1");

    test.keys(">>") // Indent
        .keys(">>") // Indent again
        .keys(">>"); // And again

    assert_eq!(test.buffer_content(), "            line 1\n");
    test.assert_cursor(0, 12);
}

#[test]
fn test_indent_then_dedent() {
    let mut test = EditorTest::new("line 1");

    test.keys(">>") // Indent
        .keys("<<"); // Dedent

    assert_eq!(test.buffer_content(), "line 1\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_indent_dedent_cycle() {
    let mut test = EditorTest::new("    indented");

    test.keys("<<") // Dedent
        .keys(">>") // Indent
        .keys(">>") // Indent more
        .keys("<<"); // Dedent

    assert_eq!(test.buffer_content(), "    indented\n");
    test.assert_cursor(0, 4);
}

// ============================================================================
// Indent with text objects (if supported)
// ============================================================================

#[test]
fn test_indent_paragraph() {
    let mut test = EditorTest::new("para line 1\npara line 2\n\nnext para");

    test.keys(">ip"); // Indent inner paragraph

    assert_eq!(
        test.buffer_content(),
        "para line 1\npara line 2\n\nnext para\n"
    );
    test.assert_cursor(0, 0);
}

#[test]
fn test_indent_around_paragraph() {
    let mut test = EditorTest::new("para 1\npara 1 cont\n\npara 2");

    test.keys(">ap"); // Indent around paragraph

    assert_eq!(test.buffer_content(), "para 1\npara 1 cont\n\npara 2\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Indent and undo/redo
// ============================================================================

#[test]
fn test_indent_and_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.keys(">>") // Indent
        .press('u'); // Undo

    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_multiple_indent_undo() {
    let mut test = EditorTest::new("line 1");

    test.keys(">>")
        .keys(">>")
        .keys(">>")
        .press('u') // Undo last indent
        .press('u') // Undo another
        .press('u'); // Undo first

    assert_eq!(test.buffer_content(), "line 1\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_indent_undo_redo() {
    let mut test = EditorTest::new("line 1");

    test.keys(">>")
        .press('u') // Undo
        .press_with(ovim_core::KeyCode::Char('r'), ovim_core::Modifiers::CONTROL); // Redo

    assert_eq!(test.buffer_content(), "    line 1\n");
    test.assert_cursor(0, 4);
}

// ============================================================================
// Indent with different content
// ============================================================================

#[test]
fn test_indent_mixed_content() {
    let mut test = EditorTest::new("    indented\nno indent\n        more indent");

    test.keys(">G"); // Indent all from current to end

    assert_eq!(
        test.buffer_content(),
        "        indented\n    no indent\n            more indent\n"
    );
    test.assert_cursor(0, 8);
}

#[test]
fn test_indent_empty_line() {
    let mut test = EditorTest::new("line 1\n\nline 3");

    test.keys("j") // Move to empty line
        .keys(">>"); // Indent empty line

    assert_eq!(test.buffer_content(), "line 1\n\nline 3\n");
    test.assert_cursor(1, 0);
}

#[test]
fn test_indent_whitespace_only() {
    let mut test = EditorTest::new("    ");

    test.keys(">>"); // Indent whitespace

    assert_eq!(test.buffer_content(), "    \n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_indent_single_line_file() {
    let mut test = EditorTest::new("only line");

    test.keys(">>");

    assert_eq!(test.buffer_content(), "    only line\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_indent_at_eof() {
    let mut test = EditorTest::new("line 1\nline 2");

    test.keys("G") // Last line
        .keys(">>")
        .keys(">j"); // Try to indent beyond EOF (no effect, already at last line)

    assert_eq!(test.buffer_content(), "line 1\n        line 2\n");
    test.assert_cursor(1, 8);
}

#[test]
fn test_dedent_beyond_zero() {
    let mut test = EditorTest::new("no indent");

    test.keys("<<") // Dedent
        .keys("<<") // Dedent again (should stay at 0)
        .keys("<<"); // And again

    assert_eq!(test.buffer_content(), "no indent\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Indent with counts and motions combined
// ============================================================================

#[test]
fn test_2_shift_right_3j() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");

    test.keys("2>3j"); // Count 2, indent, motion 3j - indents all 6 lines

    assert_eq!(
        test.buffer_content(),
        "    a\n    b\n    c\n    d\n    e\n    f\n"
    );
    test.assert_cursor(0, 4);
}

#[test]
fn test_3_shift_right_2k() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne");

    test.keys("G").keys("3>2k"); // Count 3, indent, motion 2k - all lines get indented

    assert_eq!(test.buffer_content(), "    a\n    b\n    c\n    d\n    e\n");
    test.assert_cursor(0, 4); // Count 3 multiplies motion 2k to 6k; from line 4, goes to line 0
}

// ============================================================================
// Auto-indent behavior
// ============================================================================

#[test]
fn test_equal_equal_auto_indent() {
    let mut test = EditorTest::new("if true {\ncode\n}");

    test.keys("j") // Move to "code"
        .keys("=="); // Auto-indent line

    assert_eq!(test.buffer_content(), "if true {\n    code\n}\n");
    test.assert_cursor(1, 4);
}

#[test]
fn test_equal_motion() {
    let mut test = EditorTest::new("{\nline1\nline2\n}");

    test.keys("j").keys("=j"); // Auto-indent current and next

    assert_eq!(test.buffer_content(), "{\n    line1\n    line2\n}\n");
    test.assert_cursor(2, 4);
}

#[test]
fn test_equal_G() {
    let mut test = EditorTest::new("{\nline1\nline2\nline3\n}");

    test.keys("j").keys("=G"); // Auto-indent to end

    assert_eq!(
        test.buffer_content(),
        "{\n    line1\n    line2\n    line3\n}\n"
    );
    test.assert_cursor(4, 0); // Cursor ends at the target line for G (the closing brace).
}

// ============================================================================
// Auto-indent across delimiter contexts
// ============================================================================

#[test]
fn test_equal_equal_indents_inside_braces() {
    let mut test = EditorTest::new("fn main() {\nlet x = 1;\n}\n");

    test.keys("j==");

    assert_eq!(test.buffer_content(), "fn main() {\n    let x = 1;\n}\n");
    test.assert_cursor(1, 4);
}

#[test]
fn test_equal_equal_dedents_closing_brace() {
    let mut test = EditorTest::new("fn main() {\n    let x = 1;\n    }\n");

    test.keys("jj==");

    assert_eq!(test.buffer_content(), "fn main() {\n    let x = 1;\n}\n");
    test.assert_cursor(2, 0);
}

#[test]
fn test_equal_equal_indents_line_between_braces_editor_test_dsl() {
    editor_test! {
        given {
            "{"
            ""
            "foo"
            "^"
            "}"
            ""
        }
        keys "=="
        expect Normal {
            "{"
            ""
            "    foo"
            "    ^"
            "}"
            ""
        }
    }
}

#[test]
fn test_equal_G_indents_square_brackets() {
    let mut test = EditorTest::new("let xs = [\n1,\n2,\n];\n");

    test.keys("=G");

    assert_eq!(test.buffer_content(), "let xs = [\n    1,\n    2,\n];\n");
}

#[test]
fn test_equal_G_indents_parentheses() {
    let mut test = EditorTest::new("call(\na,\nb,\n)\n");

    test.keys("=G");

    assert_eq!(test.buffer_content(), "call(\n    a,\n    b,\n)\n");
}

#[test]
fn test_equal_G_indents_nested_delimiters() {
    let mut test = EditorTest::new("{\n[\n(\nx\n)\n]\n}\n");

    test.keys("=G");

    assert_eq!(
        test.buffer_content(),
        "{\n    [\n        (\n            x\n        )\n    ]\n}\n"
    );
}

#[test]
fn test_equal_G_handles_trailing_whitespace_after_opening_delimiter() {
    let mut test = EditorTest::new("{   \nline\n}\n");

    test.keys("=G");

    assert_eq!(test.buffer_content(), "{   \n    line\n}\n");
}

#[test]
fn test_equal_equal_undo_restores_indentation() {
    let mut test = EditorTest::new("if true {\ncode\n}\n");

    test.keys("j==u");

    assert_eq!(test.buffer_content(), "if true {\ncode\n}\n");
}

// ============================================================================
// Indent with tabs vs spaces
// ============================================================================

#[test]
fn test_indent_creates_spaces() {
    let mut test = EditorTest::new("line");

    test.keys(">>"); // Should create spaces (or tabs based on settings)

    assert_eq!(test.buffer_content(), "    line\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_indent_with_existing_tabs() {
    let mut test = EditorTest::new("\tline");

    test.keys(">>"); // Indent tabbed line

    assert_eq!(test.buffer_content(), "        line\n");
    test.assert_cursor(0, 8);
}

#[test]
fn test_dedent_tabs() {
    let mut test = EditorTest::new("\t\tline");

    test.keys("<<"); // Dedent tabs

    assert_eq!(test.buffer_content(), "    line\n");
    test.assert_cursor(0, 4);
}

// ============================================================================
// Dot repeat with indent
// ============================================================================

#[test]
fn test_indent_dot_repeat() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");

    test.keys(">>") // Indent line 1
        .keys("j") // Move down
        .press('.') // Repeat indent on line 2
        .keys("j")
        .press('.'); // Repeat indent on line 3

    // Bug fix: dot-repeat now correctly indents at col 0 (like >>)
    // instead of inserting spaces at cursor position
    assert_eq!(
        test.buffer_content(),
        "    line 1\n    line 2\n    line 3\n"
    );
    test.assert_cursor(2, 4);
}

#[test]
fn test_dedent_dot_repeat() {
    let mut test = EditorTest::new("    line 1\n    line 2\n    line 3");

    test.keys("<<").keys("j").press('.').keys("j").press('.');

    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
    test.assert_cursor(2, 0);
}

// ============================================================================
// Indent in insert mode (if Ctrl-T/Ctrl-D supported)
// ============================================================================

#[test]
fn test_ctrl_t_indent_in_insert() {
    let mut test = EditorTest::new("line");

    test.press('i')
        .press_with(ovim_core::KeyCode::Char('t'), ovim_core::Modifiers::CONTROL) // Indent in insert mode
        .press_esc();

    // Ctrl-T in insert mode adds one level of indentation (4 spaces by default)
    assert_eq!(test.buffer_content(), "    line\n");
    // After Esc from insert mode, cursor moves back one position, so from 4 to 3
    test.assert_cursor(0, 3);
}

#[test]
fn test_ctrl_d_dedent_in_insert() {
    let mut test = EditorTest::new("    line");

    test.press('i')
        .press_with(ovim_core::KeyCode::Char('d'), ovim_core::Modifiers::CONTROL) // Dedent in insert mode
        .press_esc();

    // Ctrl-D in insert mode removes one level of indentation (4 spaces)
    assert_eq!(test.buffer_content(), "line\n");
    test.assert_cursor(0, 0);
}
