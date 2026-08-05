#![allow(non_snake_case)]

mod helpers;
use helpers::EditorTest;
use ovim_core::{KeyCode, Modifiers};

// ============================================================================
// Ctrl-V command - Visual Block mode (CRITICAL for Neovim-like experience)
// ============================================================================

#[test]
fn test_ctrl_v_basic_selection() {
    let mut test = EditorTest::new("hello world\ntest line\nfoo bar");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL) // Enter visual block mode
        .keys("jj") // Down 2 lines
        .keys("lll"); // Right 3 chars

    assert_eq!(test.buffer_content(), "hello world\ntest line\nfoo bar\n");
    test.assert_cursor(2, 3);
}

#[test]
fn test_ctrl_v_delete_block() {
    let mut test = EditorTest::new("hello world\ntest line\nfoo bar");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Select 3 lines
        .keys("llll") // Select 5 columns
        .press('d'); // Delete block

    assert_eq!(test.buffer_content(), " world\nline\nar\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_v_yank_paste_block() {
    let mut test = EditorTest::new("hello world\ntest line\nfoo bar");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("j") // Down 1 line
        .keys("ll") // Right 2 chars
        .press('y') // Yank block
        .press_esc()
        .keys("G$") // Go to end of last line
        .press('p'); // Paste block

    // Block rows past the last buffer line are appended as new space-padded
    // lines, like vim (OV-00291).
    assert_eq!(
        test.buffer_content(),
        "hello world\ntest line\nfoo barhel\n       tes\n"
    );
    // Cursor on last char of the final pasted row: "tes" padded to col 7,
    // last char at col 9
    test.assert_cursor(3, 9);
}

#[test]
fn test_ctrl_v_change_block() {
    let mut test = EditorTest::new("hello world\ntest line\nfoo bar");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("j")
        .keys("ll")
        .press('c') // Change block
        .type_text("X")
        .press_esc();

    // 'll' selects columns 0,1,2 (3 chars total) - visual mode is inclusive
    assert_eq!(test.buffer_content(), "Xlo world\nXt line\nfoo bar\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_v_insert_block() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Select 3 lines
        .press('I') // Insert at beginning of block
        .type_text(">> ")
        .press_esc();

    assert_eq!(test.buffer_content(), ">> hello\n>> world\n>> test\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_append_block() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.keys("$") // End of first line
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Select 3 lines
        .press('A') // Append at end of block
        .type_text("!")
        .press_esc();

    assert_eq!(test.buffer_content(), "hello!\nworld!\ntest!\n");
    test.assert_cursor(2, 4);
}

#[test]
fn test_ctrl_v_ragged_right_edge() {
    let mut test = EditorTest::new("short\nthis is longer\nmedium");

    test.keys("lll") // Move to column 3
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Down 2 lines
        .keys("$") // Extend to end of longest line
        .press('d'); // Delete

    assert_eq!(test.buffer_content(), "sho\nthi\nmed\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_ctrl_v_empty_lines() {
    let mut test = EditorTest::new("hello\n\nworld");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Include empty line
        .keys("ll")
        .press('d');

    // Visual block deletes same columns on all lines - 'll' selects cols 0,1,2 (3 chars)
    assert_eq!(test.buffer_content(), "lo\n\nld\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_v_single_column() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.keys("ll") // Column 2
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Down 2 lines (single column)
        .press('r') // Replace
        .press('X'); // With 'X'

    assert_eq!(test.buffer_content(), "heXlo\nwoXld\nteXt\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_replace_escape_cancels_pending_char_macro_flow() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.keys("ll")
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("j")
        .press('r')
        .press_esc();

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_mode(ovim::mode::Mode::VisualBlock);

    // A second Esc exits visual mode cleanly.
    test.press_esc();
    test.assert_mode(ovim::mode::Mode::Normal);
}

#[test]
fn test_ctrl_v_replace_after_cancel_still_replaces_block_macro_flow() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.keys("ll")
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("j")
        .press('r')
        .press_esc() // cancel pending replace char
        .press('r')
        .press('X');

    assert_eq!(test.buffer_content(), "heXlo\nwoXld\ntest\n");
    test.assert_mode(ovim::mode::Mode::Normal);
}

#[test]
fn test_ctrl_v_o_flip_corners() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj") // Select down
        .keys("ll") // Select right
        .press('o'); // Flip to opposite corner

    // Cursor should be at opposite corner of block
    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_ctrl_v_O_flip_horizontal() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('O'); // Flip horizontally

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(2, 0);
}

#[test]
fn test_ctrl_v_c_replace_block() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('c')
        .type_text("NEW")
        .press_esc();

    // Each line should have "NEW" replacing the first 3 chars
    assert_eq!(test.buffer_content(), "NEWlo\nNEWld\nNEWt\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_ctrl_v_with_dollar() {
    let mut test = EditorTest::new("short\nvery long line\nmedium");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("$") // Extend to end (should work on longest line)
        .press('d');

    assert_eq!(test.buffer_content(), "\n\n\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_v_undo() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('c')
        .type_text("X")
        .press_esc()
        .press('u'); // Undo block change

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(0, 0); // Cursor at start of visual block
}

#[test]
fn test_ctrl_v_delete_dot_repeat_macro_flow() {
    editor_flow_test! {
        content "abcde\nvwxyz\n12345\n67890\n";
        step "<C-v>jlld" => |test| {
            assert_eq!(test.buffer_content(), "de\nyz\n12345\n67890\n");
            test.assert_cursor(0, 0);
        }
        step "jj." => |test| {
            assert_eq!(test.buffer_content(), "de\nyz\n45\n90\n");
            test.assert_cursor(2, 0);
        }
    }
}

#[test]
fn test_ctrl_v_change_dot_repeat_macro_flow() {
    editor_flow_test! {
        content "hello\nworld\ntest\nmore\nlines\n";
        step "<C-v>jlcXX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "XXllo\nXXrld\ntest\nmore\nlines\n");
            test.assert_cursor(0, 1);
        }
        step "jj0." => |test| {
            assert_eq!(test.buffer_content(), "XXllo\nXXrld\nXXst\nXXre\nlines\n");
            test.assert_cursor(2, 1);
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "XXllo\nXXrld\ntest\nmore\nlines\n");
        }
    }
}

#[test]
fn test_ctrl_v_change_undo_does_not_consume_prior_change_macro_flow() {
    editor_flow_test! {
        content "abcde\nfghij\nklmno\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "abcde!\nfghij\nklmno\n");
        }
        step "0<C-v>jjlcX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "Xcde!\nXhij\nXmno\n");
        }
        step "u" => |test| {
            // Undo should only revert the visual-block change.
            assert_eq!(test.buffer_content(), "abcde!\nfghij\nklmno\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcde\nfghij\nklmno\n");
        }
    }
}

#[test]
fn test_ctrl_v_insert_undo_does_not_consume_prior_change_macro_flow() {
    editor_flow_test! {
        content "a\nb\nc\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "a!\nb\nc\n");
        }
        step "0<C-v>jjI><Esc>" => |test| {
            assert_eq!(test.buffer_content(), ">a!\n>b\n>c\n");
        }
        step "u" => |test| {
            // Undo should only revert the visual-block insert.
            assert_eq!(test.buffer_content(), "a!\nb\nc\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "a\nb\nc\n");
        }
    }
}

#[test]
fn test_ctrl_v_escape_cancels() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press_esc(); // Cancel selection

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_yank_uppercase() {
    let mut test = EditorTest::new("HELLO\nWORLD\nTEST");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("j")
        .keys("ll")
        .press('y')
        .press_esc()
        .keys("G")
        .press('p');

    // Pastes first line of block ("HEL") at line 2, col 1 -> "THELEST".
    // Second block line ("WOR") is appended as a new padded line, like vim
    // (OV-00291).
    assert_eq!(test.buffer_content(), "HELLO\nWORLD\nTHELEST\n WOR\n");
    test.assert_cursor(3, 3); // Cursor on last char of final pasted row
}

#[test]
fn test_ctrl_v_with_tabs() {
    let mut test = EditorTest::new("\thello\n\tworld\n\ttest");

    test.keys("l") // Move past tab
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('d');

    assert_eq!(test.buffer_content(), "\tlo\n\tld\n\tt\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_ctrl_v_switch_to_v() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .press('v'); // Switch to character-wise visual

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(2, 0);
}

#[test]
fn test_ctrl_v_switch_to_V() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .press('V'); // Switch to line-wise visual

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(2, 0);
}

#[test]
fn test_ctrl_v_indent() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('>'); // Indent block

    assert_eq!(test.buffer_content(), "    hello\n    world\n    test\n");
    test.assert_cursor(2, 6);
}

#[test]
fn test_ctrl_v_dedent() {
    let mut test = EditorTest::new("    hello\n    world\n    test");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('<'); // Dedent block

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_ctrl_v_J_join_lines() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .press('J'); // Join lines (should it work in block mode?)

    // Behavior may vary - document what happens
    assert_eq!(test.buffer_content(), "hello world test\n");
    test.assert_cursor(0, 11);
}

#[test]
fn test_ctrl_v_gv_reselect() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press_esc()
        .keys("gv"); // Reselect last visual block

    assert_eq!(test.buffer_content(), "hello\nworld\ntest\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_multiple_char_insert() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .press('I')
        .type_text("PREFIX: ")
        .press_esc();

    assert_eq!(
        test.buffer_content(),
        "PREFIX: hello\nPREFIX: world\nPREFIX: test\n"
    );
    test.assert_cursor(2, 7);
}

#[test]
fn test_ctrl_v_at_eof() {
    let mut test = EditorTest::new("hello\nworld");

    test.keys("G") // Last line
        .press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jjjj") // Try to go past EOF
        .keys("ll")
        .press('d');

    // 'll' selects columns 0,1,2 (3 chars total) - visual mode is inclusive
    assert_eq!(test.buffer_content(), "hello\nld\n");
    test.assert_cursor(1, 0);
}

// ============================================================================
// Visual block mode with operators
// ============================================================================

#[test]
fn test_ctrl_v_tilde_case_toggle() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('~'); // Toggle case

    assert_eq!(test.buffer_content(), "HELlo\nWORld\nTESt\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_uppercase_U() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('U'); // Uppercase

    assert_eq!(test.buffer_content(), "HELlo\nWORld\nTESt\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_lowercase_u() {
    let mut test = EditorTest::new("HELLO\nWORLD\nTEST");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('u'); // Lowercase

    assert_eq!(test.buffer_content(), "helLO\nworLD\ntesT\n");
    test.assert_cursor(2, 2);
}

#[test]
fn test_ctrl_v_replace_r() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press_with(KeyCode::Char('v'), Modifiers::CONTROL)
        .keys("jj")
        .keys("ll")
        .press('r')
        .press('X'); // Replace all selected chars with X

    assert_eq!(test.buffer_content(), "XXXlo\nXXXld\nXXXt\n");
    test.assert_cursor(2, 2);
}
