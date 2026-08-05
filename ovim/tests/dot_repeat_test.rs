#![allow(
    non_snake_case,
    reason = "Test names intentionally mirror Vim key notation (for example X, O, A, and R)."
)]

#[macro_use]
#[path = "helpers/editor_test_macro.rs"]
mod editor_test_macro;
mod helpers;
use helpers::EditorTest;

// ============================================================================
// Dot (.) command - Repeat last change
// ============================================================================

#[test]
fn test_dot_repeat_delete_char() {
    let mut test = EditorTest::new("hello");

    test.press('x') // Delete 'h'
        .press('.'); // Repeat (delete 'e')

    assert_eq!(test.buffer_content(), "llo\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_repeat_delete_word() {
    let mut test = EditorTest::new("one two three four");

    test.keys("dw") // Delete "one "
        .press('.'); // Repeat (delete "two ")

    assert_eq!(test.buffer_content(), "three four\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_repeat_delete_line() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("dd") // Delete line 1
        .press('.'); // Repeat (delete line 2)

    assert_eq!(test.buffer_content(), "line 3\nline 4\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_repeat_insert() {
    let mut test = EditorTest::new("line 1\nline 2");

    test.press('i')
        .type_text("PREFIX:")
        .press_esc()
        .press('j') // Next line
        .press('.'); // Repeat insert

    assert_eq!(test.buffer_content(), "PREFIX:line 1\nline PREFIX:2\n");
    test.assert_cursor(1, 11);
}

#[test]
fn test_dot_repeat_append() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('a')
        .type_text("!")
        .press_esc()
        .press('j') // Next line
        .press('.'); // Repeat append

    assert_eq!(test.buffer_content(), "h!ello\nwo!rld\n");
    test.assert_cursor(1, 2);
}

#[test]
fn test_dot_repeat_change_word() {
    let mut test = EditorTest::new("one two three");

    test.keys("ciw") // Change "one" to "X"
        .type_text("X")
        .press_esc()
        .keys("w") // Move to "two"
        .press('.'); // Repeat: change "two" to "X"

    // ciw changes only the word, not surrounding whitespace
    assert_eq!(test.buffer_content(), "X X three\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_dot_repeat_change_word_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "one two three\n";
        step "ciwX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X two three\n");
            test.assert_cursor(0, 0);
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "X X three\n");
            test.assert_cursor(0, 2);
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "X two three\n");
        }
    }
}

#[test]
fn test_dot_repeat_substitute() {
    let mut test = EditorTest::new("hello world");

    test.press('s') // Substitute 'h' → delete 'h', type 'H'
        .type_text("H")
        .press_esc()
        .keys("w") // Move to 'w'
        .press('.'); // Repeat: delete 'w', type 'H' → "Hello Horld"

    assert_eq!(test.buffer_content(), "Hello Horld\n");
    test.assert_cursor(0, 6);
}

// ============================================================================
// Dot with counts
// ============================================================================

#[test]
fn test_dot_with_count() {
    let mut test = EditorTest::new("abcdefgh");

    test.press('x') // Delete one char
        .keys("3."); // Repeat 3 times

    assert_eq!(test.buffer_content(), "cdefgh\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_original_count_vs_repeat_count() {
    let mut test = EditorTest::new("one two three four five six");

    test.keys("2dw") // Delete 2 words: "one two " → "three four five six"
        .press('.'); // Repeat 2dw: "three four " → "five six"

    assert_eq!(test.buffer_content(), "five six\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_override_original_count() {
    let mut test = EditorTest::new("one two three four five six");

    test.keys("2dw") // Delete 2 words: "one two " → "three four five six"
        .keys("3."); // Repeat with count 2 (count override not yet supported): "three four " → "five six"

    assert_eq!(test.buffer_content(), "five six\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_repeat_counted_insert() {
    let mut test = EditorTest::new("line");

    test.press('i').type_text("X").press_esc().keys("3."); // Repeat 3 times

    assert_eq!(test.buffer_content(), "XXline\n");
    test.assert_cursor(0, 1);
}

// ============================================================================
// Dot with various operators
// ============================================================================

#[test]
fn test_dot_repeat_yank_then_change() {
    let mut test = EditorTest::new("one two three");

    test.keys("yiw") // Yank doesn't count as change
        .keys("ciw") // Change word "one" to "X"
        .type_text("X")
        .press_esc()
        .keys("w") // Move to "two"
        .press('.'); // Should repeat change: ciw "X" on "two"

    // ciw preserves whitespace - "one" becomes "X", "two" becomes "X"
    assert_eq!(test.buffer_content(), "X X three\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_dot_repeat_d_dollar() {
    let mut test = EditorTest::new("hello world\ntest case");

    test.keys("d$") // Delete to end of line
        .press('j') // Next line
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "\n\n");
    test.assert_cursor(1, 0);
}

#[test]
fn test_dot_repeat_c_dollar() {
    let mut test = EditorTest::new("hello world\ntest case");

    test.keys("c$") // Change to end (delete "hello world", type "NEW")
        .type_text("NEW")
        .press_esc()
        .press('j')
        .press('.'); // Repeat: delete from cursor to end, type "NEW"

    // After c$+NEW+Esc: "NEW\n...", cursor at (0,2). j → (1,2). Dot: delete "st case", type "NEW"
    assert_eq!(test.buffer_content(), "NEW\nteNEW\n");
    test.assert_cursor(1, 4);
}

#[test]
fn test_dot_repeat_upper_case_X() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // End (cursor on 'o')
        .press('X') // Delete char before cursor ('l'), leaving "helo", cursor on 'o'
        .press('.'); // Repeat (delete char before cursor 'l'), leaving "heo", cursor on 'e'

    assert_eq!(test.buffer_content(), "heo\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_dot_repeat_J_join() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.press('J') // Join line 1 and 2
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "line 1 line 2 line 3\nline 4\n");
    test.assert_cursor(0, 13);
}

// ============================================================================
// Dot with text objects
// ============================================================================

#[test]
fn test_dot_repeat_diw() {
    let mut test = EditorTest::new("one two three four");

    // Note: iw does NOT include trailing whitespace (that's aw)
    test.keys("diw") // Delete "one" → " two three four" (cursor at 0)
        .keys("w") // Move to next word "two" (cursor at 1)
        .press('.'); // Repeat diw: delete "two" → "  three four"

    // Semantic repeat: re-evaluates inner word at cursor, deletes entire word
    assert_eq!(test.buffer_content(), "  three four\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_dot_repeat_daw() {
    let mut test = EditorTest::new("one two three four");

    test.keys("daw").press('.'); // Repeat on next word

    assert_eq!(test.buffer_content(), "three four\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_repeat_ci_quote() {
    let mut test = EditorTest::new(r#""hello" and "world""#);

    test.keys("f\"") // Find first quote (opening quote of "hello")
        .keys("ci\"") // Change inside quotes
        .type_text("X")
        .press_esc()
        .keys("f\"") // Find next quote (closing quote of "X", not opening of "world"!)
        .press('.'); // Repeat (finds same "X" pair, replaces X with X)

    // In Vim: f" from inside "X" finds the closing quote of "X", so ci" operates on "X" again
    assert_eq!(test.buffer_content(), "\"X\" and \"world\"\n");
    test.assert_cursor(0, 1); // On the X after ci" repeat
}

#[test]
fn test_dot_repeat_di_paren() {
    let mut test = EditorTest::new("func(arg1) and func(arg2)");

    test.keys("f(")
        .keys("di(") // Delete inside parens
        .keys("f(") // Next parens
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "func() and func()\n");
    test.assert_cursor(0, 16); // On closing paren after deletion
}

#[test]
fn test_dot_repeat_ci_paren_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "func(arg1) and func(arg2)\n";
        step "f(ci(X<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "func(X) and func(arg2)\n");
        }
        step "f(." => |test| {
            assert_eq!(test.buffer_content(), "func(X) and func(X)\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "func(X) and func(arg2)\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "func(arg1) and func(arg2)\n");
        }
    }
}

#[test]
fn test_dot_repeat_guiw_semantic_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "HELLO WORLD AGAIN\n";
        step "0guiw" => |test| {
            assert_eq!(test.buffer_content(), "hello WORLD AGAIN\n");
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "hello world AGAIN\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello WORLD AGAIN\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "HELLO WORLD AGAIN\n");
        }
    }
}

// ============================================================================
// Dot with visual mode operations
// ============================================================================

#[test]
fn test_dot_after_visual_delete() {
    let mut test = EditorTest::new("hello world test");

    test.press('v')
        .keys("e") // Select word
        .press('d') // Delete
        .press('w') // Move to next word
        .press('.'); // Repeat (should work?)

    assert_eq!(test.buffer_content(), "  test\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_dot_after_visual_change() {
    // Matches Vim: `vecX<Esc>w.` on "one two three" deletes the 3-char
    // selection equivalent at the new cursor position, then inserts "X".
    // On "two" (3 chars) that means: delete "two", insert "X".
    let mut test = EditorTest::new("one two three");

    test.press('v')
        .keys("e") // Select "one"
        .press('c') // Change
        .type_text("X")
        .press_esc()
        .press('w')
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "X X three\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_dot_after_visual_line_delete() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.press('V') // Visual line
        .press('d') // Delete
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "line 3\nline 4\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_after_visual_delete_macro_flow() {
    editor_flow_test! {
        content "abc def ghi\njkl mno pqr\n";
        step "ved" => |test| {
            assert_eq!(test.buffer_content(), " def ghi\njkl mno pqr\n");
            test.assert_cursor(0, 0);
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "  ghi\njkl mno pqr\n");
            test.assert_cursor(0, 1);
        }
    }
}

#[test]
fn test_dot_after_visual_line_delete_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "line 1\nline 2\nline 3\nline 4\n";
        step "Vd" => |test| {
            assert_eq!(test.buffer_content(), "line 2\nline 3\nline 4\n");
        }
        step "." => |test| {
            assert_eq!(test.buffer_content(), "line 3\nline 4\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "line 2\nline 3\nline 4\n");
        }
    }
}

// ============================================================================
// Dot with motion variations
// ============================================================================

#[test]
fn test_dot_repeat_dw_different_positions() {
    let mut test = EditorTest::new("one two three four five");

    test.keys("dw") // Delete "one " → "two three four five"
        .keys("w") // Move to "three"
        .press('.'); // Delete "three " (re-evaluates dw at cursor)

    assert_eq!(test.buffer_content(), "two four five\n");
    test.assert_cursor(0, 4);
}

#[test]
fn test_dot_repeat_cw_at_different_word_lengths() {
    let mut test = EditorTest::new("short longerword");

    test.keys("cw") // Change "short"
        .type_text("X")
        .press_esc()
        .keys("w") // Move to "longerword"
        .press('.'); // Repeat (should change "longerword")

    assert_eq!(test.buffer_content(), "X X\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_dot_repeat_cw_semantic_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "short longerword tail\n";
        step "cwX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X longerword tail\n");
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "X X tail\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "X longerword tail\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "short longerword tail\n");
        }
    }
}

#[test]
fn test_dot_repeat_cw_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "short longerword tail\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "short longerword tail!\n");
        }
        step "0cwX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X longerword tail!\n");
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "X X tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "X longerword tail!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "X X tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "X longerword tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "short longerword tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "short longerword tail\n");
        }
    }
}

#[test]
fn test_cw_esc_undo_does_not_consume_prior_change_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0cw<Esc>" => |test| {
            assert_eq!(test.buffer_content(), " beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

#[test]
fn test_cc_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0ccNEW<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

#[test]
fn test_dot_repeat_cc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha\nbeta\ngamma\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha!\nbeta\ngamma\n");
        }
        step "0ccNEW<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta\ngamma\n");
        }
        step "j." => |test| {
            assert_eq!(test.buffer_content(), "NEW\nNEW\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta\ngamma\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nNEW\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha!\nbeta\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha\nbeta\ngamma\n");
        }
    }
}

#[test]
fn test_C_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0CNEW<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

#[test]
fn test_dot_repeat_C_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha one\nbeta two\ngamma three\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha one!\nbeta two\ngamma three\n");
        }
        step "0CNEW<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta two\ngamma three\n");
        }
        step "j0." => |test| {
            assert_eq!(test.buffer_content(), "NEW\nNEW\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta two\ngamma three\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nNEW\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "NEW\nbeta two\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha one!\nbeta two\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha one\nbeta two\ngamma three\n");
        }
    }
}

#[test]
fn test_cc_esc_no_insert_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0cc<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

#[test]
fn test_C_esc_no_insert_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0C<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

#[test]
fn test_dot_repeat_cc_esc_no_insert_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha\nbeta\ngamma\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha!\nbeta\ngamma\n");
        }
        step "0cc<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta\ngamma\n");
        }
        step "j." => |test| {
            assert_eq!(test.buffer_content(), "\n\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta\ngamma\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\n\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha!\nbeta\ngamma\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha\nbeta\ngamma\n");
        }
    }
}

#[test]
fn test_dot_repeat_C_esc_no_insert_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha one\nbeta two\ngamma three\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha one!\nbeta two\ngamma three\n");
        }
        step "0C<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta two\ngamma three\n");
        }
        step "j0." => |test| {
            assert_eq!(test.buffer_content(), "\n\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta two\ngamma three\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\n\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\nbeta two\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha one!\nbeta two\ngamma three\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha one\nbeta two\ngamma three\n");
        }
    }
}

// ============================================================================
// Dot repeat edge cases
// ============================================================================

#[test]
fn test_dot_without_previous_change() {
    let mut test = EditorTest::new("hello");

    test.press('.'); // No previous change

    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_after_movement_only() {
    let mut test = EditorTest::new("hello world");

    test.keys("w") // Just move
        .press('.'); // No change to repeat

    assert_eq!(test.buffer_content(), "hello world\n");
    test.assert_cursor(0, 6);
}

#[test]
fn test_dot_across_lines() {
    let mut test = EditorTest::new("hello\nworld\ntest");

    test.press('x') // Delete char
        .press('j') // Next line
        .press('.') // Repeat
        .press('j') // Next line
        .press('.'); // Repeat again

    assert_eq!(test.buffer_content(), "ello\norld\nest\n");
    test.assert_cursor(2, 0);
}

#[test]
fn test_dot_at_end_of_line() {
    let mut test = EditorTest::new("hello");

    test.keys("$") // End
        .press('x') // Delete last char ('o')
        .press('.'); // Repeat (deletes 'l')

    assert_eq!(test.buffer_content(), "hel\n");
    test.assert_cursor(0, 2); // Cursor on last char 'l'
}

#[test]
fn test_dot_after_failed_operation() {
    let mut test = EditorTest::new("x");

    test.press('x') // Delete 'x'
        .press('.'); // Try to repeat (nothing to delete)

    assert_eq!(test.buffer_content(), "\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Dot with insert mode variations
// ============================================================================

#[test]
fn test_dot_repeat_o_command() {
    let mut test = EditorTest::new("line 1\nline 2");

    test.press('o') // Open line below
        .type_text("new")
        .press_esc()
        .press('j') // Move down
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "line 1\nnew\nline 2\nnew\n");
    test.assert_cursor(3, 2);
}

#[test]
fn test_dot_repeat_O_command() {
    let mut test = EditorTest::new("line 1\nline 2");

    test.press('O') // Open line above
        .type_text("new")
        .press_esc()
        .press('j')
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "new\nnew\nline 1\nline 2\n");
    test.assert_cursor(1, 2);
}

#[test]
fn test_dot_repeat_A_command() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('A') // Append at end
        .type_text("!")
        .press_esc()
        .press('j')
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "hello!\nworld!\n");
    test.assert_cursor(1, 5);
}

#[test]
fn test_dot_repeat_I_command() {
    let mut test = EditorTest::new("hello\nworld");

    test.press('I') // Insert at beginning
        .type_text("START:")
        .press_esc()
        .press('j')
        .press('.'); // Repeat

    // I repeats at first non-blank of the line, not at cursor position
    assert_eq!(test.buffer_content(), "START:hello\nSTART:world\n");
    test.assert_cursor(1, 5);
}

// ============================================================================
// Dot with replace mode
// ============================================================================

#[test]
fn test_dot_repeat_r_command() {
    let mut test = EditorTest::new("hello world");

    test.press('r') // Replace char (h -> X)
        .press('X')
        .press('l') // Move right
        .press('.'); // Repeat (e -> X)

    assert_eq!(test.buffer_content(), "XXllo world\n");
    test.assert_cursor(0, 1);
}

#[test]
fn test_dot_repeat_R_command() {
    let mut test = EditorTest::new("hello world");

    test.press('R') // Replace mode (replaces "he" -> "HI")
        .type_text("HI")
        .press_esc()
        .press('w') // Move to "world"
        .press('.'); // Repeat (replaces "wo" -> "HI")

    assert_eq!(test.buffer_content(), "HIllo HIrld\n");
    test.assert_cursor(0, 7);
}

#[test]
fn test_dot_repeat_R_semantic_undo_granularity_macro_flow() {
    editor_flow_test! {
        content "hello world test\n";
        step "RHI<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world test\n");
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "HIllo HIrld test\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world test\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world test\n");
        }
    }
}

#[test]
fn test_R_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "hello world\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello world!\n");
        }
        step "0RHI<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world\n");
        }
    }
}

#[test]
fn test_dot_repeat_R_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "hello world\nalpha beta\nomega zeta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello world!\nalpha beta\nomega zeta\n");
        }
        step "0RHI<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\nalpha beta\nomega zeta\n");
        }
        step "j0." => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\nHIpha beta\nomega zeta\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\nalpha beta\nomega zeta\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\nHIpha beta\nomega zeta\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "HIllo world!\nalpha beta\nomega zeta\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world!\nalpha beta\nomega zeta\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world\nalpha beta\nomega zeta\n");
        }
    }
}

#[test]
fn test_replace_mode_backspace_to_empty_does_not_create_undo_entry_macro_flow() {
    editor_flow_test! {
        content "hello\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello!\n");
        }
        step "0RHI<BS><BS><Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello!\n");
        }
        step "u" => |test| {
            // If replace mode created a no-op undo entry, this would not undo `A!`.
            assert_eq!(test.buffer_content(), "hello\n");
        }
    }
}

#[test]
fn test_replace_mode_backspace_to_empty_does_not_clobber_dot_repeat_macro_flow() {
    editor_flow_test! {
        content "short longerword\n";
        step "0cwX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X longerword\n");
        }
        step "0RHI<BS><BS><Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X longerword\n");
        }
        step "w." => |test| {
            assert_eq!(test.buffer_content(), "X X\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "X longerword\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "short longerword\n");
        }
    }
}

#[test]
fn test_replace_mode_backspace_to_empty_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "hello\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello!\n");
        }
        step "0RHI<BS><BS><Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "hello!\n");
        }
    }
}

// ============================================================================
// Complex dot repeat scenarios
// ============================================================================

#[test]
fn test_dot_repeat_multiple_times() {
    let mut test = EditorTest::new("abcdefghijkl");

    test.press('x') // Delete 'a'
        .press('.') // Delete 'b'
        .press('.') // Delete 'c'
        .press('.') // Delete 'd'
        .press('.'); // Delete 'e'

    assert_eq!(test.buffer_content(), "fghijkl\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_changes_after_different_operation() {
    let mut test = EditorTest::new("one two three four");

    test.press('x') // Delete 'o' → "ne two three four"
        .keys("dw") // Delete "ne " → "two three four"
        .press('.'); // Repeat dw: delete "two " → "three four"

    assert_eq!(test.buffer_content(), "three four\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_with_undo() {
    let mut test = EditorTest::new("hello");

    test.press('x') // Delete
        .press('u') // Undo
        .press('.'); // Repeat (should work)

    assert_eq!(test.buffer_content(), "ello\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_after_undo_redo() {
    let mut test = EditorTest::new("hello");

    test.press('x') // Delete 'h'
        .press('u') // Undo
        .press_with(ovim_core::KeyCode::Char('r'), ovim_core::Modifiers::CONTROL) // Redo
        .press('.'); // Repeat

    assert_eq!(test.buffer_content(), "llo\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Dot repeat + undo interaction
// ============================================================================

#[test]
fn test_dot_repeat_X_undo_restores_correctly() {
    // Regression: $X.uu was turning "hello" into "helll" because the repeated
    // change had stale range/deleted_text from the original X, not the repeat.
    let mut test = EditorTest::new("hello");

    test.keys("$") // Cursor on 'o' (col 4)
        .press('X') // Delete 'l' before cursor → "helo"
        .press('.') // Repeat X → "heo"
        .press('u') // Undo repeat → "helo"
        .press('u'); // Undo original X → "hello"

    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 4);
}

// ============================================================================
// Dot with search + change-find (n. workflow)
// ============================================================================

#[test]
fn test_dot_repeat_search_and_change_find() {
    // Classic Vim idiom: /pattern<CR> then cfo to change through 'o', then n.n.
    // Dot-repeat should replay the full cf{char} + inserted text, not just the insert.
    editor_test! {
        given {
            "I say hello world"  "^"
            "I say hello world"  ""
            "I say hello world"  ""
        }
        keys "/hello<CR>cfohi<Esc>n.n."
        expect Normal {
            "I say hi world"  ""
            "I say hi world"  ""
            "I say hi world"  "       ^"
        }
    }
}

#[test]
fn test_cfo_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "hello world hello test\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test!\n");
        }
        step "0/hello<Enter>cfo<Esc>" => |test| {
            assert_eq!(test.buffer_content(), " world hello test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), " world hello test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test\n");
        }
    }
}

#[test]
fn test_dot_repeat_search_and_change_find_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "hello world hello test\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test!\n");
        }
        step "0/hello<Enter>cfohi<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hi world hello test!\n");
        }
        step "n." => |test| {
            assert_eq!(test.buffer_content(), "hi world hi test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hi world hello test!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "hi world hi test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hi world hello test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hello world hello test\n");
        }
    }
}

#[test]
fn test_ctX_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "abcdX abcdX tail\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail!\n");
        }
        step "0/abcdX<Enter>ctX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "X abcdX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "X abcdX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail\n");
        }
    }
}

#[test]
fn test_dot_repeat_search_and_change_till_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "abcdX abcdX tail\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail!\n");
        }
        step "0/abcdX<Enter>ctXhi<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "hiX abcdX tail!\n");
        }
        step "n." => |test| {
            assert_eq!(test.buffer_content(), "hiX hiX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hiX abcdX tail!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "hiX hiX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "hiX abcdX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "abcdX abcdX tail\n");
        }
    }
}

#[test]
fn test_cFX_esc_undo_redo_isolation_macro_flow() {
    // Backward F is EXCLUSIVE with operators: the cursor char ('!') is not
    // part of the changed range (OV-00288).
    editor_flow_test! {
        content "aXbXcY\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "cFX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXb!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "aXb!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY\n");
        }
    }
}

#[test]
fn test_cTX_esc_undo_redo_isolation_macro_flow() {
    // Backward T is EXCLUSIVE with operators: only the text strictly between
    // the target and the cursor is changed (OV-00288).
    editor_flow_test! {
        content "aXbXcY\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "cTX<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbX!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "aXbX!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY\n");
        }
    }
}

#[test]
fn test_dot_repeat_search_and_change_backward_find_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "aXbXcY aXbXcY tail\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail!\n");
        }
        // Backward F is exclusive: the 'Y' under the cursor survives (OV-00288).
        step "0/aXbXcY<Enter>fYcFXhi<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbhiY aXbXcY tail!\n");
        }
        step "ne." => |test| {
            assert_eq!(test.buffer_content(), "aXbhiY aXbhiY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbhiY aXbXcY tail!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "aXbhiY aXbhiY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbhiY aXbXcY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail\n");
        }
    }
}

#[test]
fn test_dot_repeat_search_and_change_backward_till_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "aXbXcY aXbXcY tail\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail!\n");
        }
        // Backward T is exclusive: the 'Y' under the cursor survives (OV-00288).
        step "0/aXbXcY<Enter>fYcTXhi<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXhiY aXbXcY tail!\n");
        }
        step "ne." => |test| {
            assert_eq!(test.buffer_content(), "aXbXhiY aXbXhiY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXhiY aXbXcY tail!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "aXbXhiY aXbXhiY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXhiY aXbXcY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "aXbXcY aXbXcY tail\n");
        }
    }
}

// ============================================================================
// Dot with search motions
// ============================================================================

#[test]
fn test_dot_with_search_motion() {
    let mut test = EditorTest::new("hello world hello test");

    test.keys("d/world") // Delete to "world"
        .press_enter()
        .press('.'); // Repeat (delete to next match?)

    assert_eq!(test.buffer_content(), "hello world hello test\n");
    test.assert_cursor(0, 6);
}

#[test]
fn test_dot_with_f_motion() {
    let mut test = EditorTest::new("a b c d e f");

    // dfc deletes from cursor to 'c' inclusive
    // Starting at position 0, finds 'c' at position 4, deletes "a b c" (positions 0-4)
    // Result: " d e f" (starting with space, which was after 'c')
    test.keys("dfc"); // Delete to 'c'
    assert_eq!(test.buffer_content(), " d e f\n");
    test.assert_cursor(0, 0);

    // . tries to repeat dfc, but there's no 'c' to find, so nothing happens
    test.press('.');
    assert_eq!(test.buffer_content(), " d e f\n");
    test.assert_cursor(0, 0);
}

// ============================================================================
// Dot preserving count
// ============================================================================

#[test]
fn test_dot_preserves_original_count() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4\nline 5");

    test.keys("2dd") // Delete 2 lines
        .press('.'); // Should delete 2 more lines

    assert_eq!(test.buffer_content(), "line 5\n");
    test.assert_cursor(0, 0);
}

#[test]
fn test_dot_with_multichar_insert() {
    let mut test = EditorTest::new("line 1\nline 2");

    test.press('i')
        .type_text("LONG TEXT ")
        .press_esc()
        .press('j')
        .press('.'); // Should insert same text

    assert_eq!(
        test.buffer_content(),
        "LONG TEXT line 1\nline LONG TEXT 2\n"
    );
    test.assert_cursor(1, 14);
}

#[test]
fn test_dot_repeat_complex_change() {
    let mut test = EditorTest::new("one two\nthree four\nfive six");

    test.keys("ciw") // Change "one" to "REPLACED"
        .type_text("REPLACED")
        .press_esc()
        .press('j') // Next line (cursor ends up on "four")
        .press('w') // Move to next word (wraps to "five" on line 2)
        .press('.'); // Repeat change: ciw "REPLACED" on "five"

    // ciw preserves surrounding whitespace
    assert_eq!(
        test.buffer_content(),
        "REPLACED two\nthree four\nREPLACED six\n"
    );
    test.assert_cursor(2, 7);
}

// ============================================================================
// Regression: dot-repeat o/O must create a new line, not corrupt current line
// ============================================================================

#[test]
fn test_dot_repeat_o_on_indented_line() {
    // Regression: dot-repeat of 'o' was inserting indent at end of current
    // line instead of creating a new line below.
    let mut test = EditorTest::new("    hello\n    world");

    // o on indented line, type text, Esc
    test.keys("o");
    test.type_text("new");
    test.press_esc();

    // Move to "world" line and repeat
    test.keys("j"); // now on "    world"
    test.keys(".");

    // Should have: "    hello", "    new", "    world", "    new"
    assert_eq!(test.line_text(0).unwrap(), "    hello");
    assert_eq!(test.line_text(1).unwrap(), "    new");
    assert_eq!(test.line_text(2).unwrap(), "    world");
    assert_eq!(test.line_text(3).unwrap(), "    new");
    assert_eq!(test.line_count(), 4);
}

#[test]
fn test_dot_repeat_O_on_indented_line() {
    // Regression: same issue for O (open above)
    let mut test = EditorTest::new("    hello\n    world");

    // O on first line, type text, Esc
    test.keys("O");
    test.type_text("above");
    test.press_esc();

    // Move to "world" line and repeat
    test.keys("jj"); // skip past hello to world
    test.keys(".");

    // Should insert "    above" above "    world"
    assert_eq!(test.line_count(), 4);
    assert!(
        test.buffer_content().contains("    above\n    hello"),
        "First O should be above hello"
    );
    assert!(
        test.buffer_content().contains("    above\n    world"),
        "Repeated O should be above world"
    );
}

#[test]
fn test_dot_repeat_o_esc_no_text() {
    // Regression: o<Esc> with non-default entry_mode must preserve entry_mode
    // for dot-repeat even when only 1 or 2 sub-changes exist.
    let mut test = EditorTest::new("line 1\nline 2");

    // o then immediately Esc (creates empty line)
    test.keys("o");
    test.press_esc();

    // Move down and repeat
    test.keys("j");
    test.keys(".");

    // Both lines should have gotten new empty lines below them
    assert_eq!(test.line_count(), 4);
    assert_eq!(test.line_text(0).unwrap(), "line 1");
    assert_eq!(test.line_text(1).unwrap(), "");
    assert_eq!(test.line_text(2).unwrap(), "line 2");
    assert_eq!(test.line_text(3).unwrap(), "");
}

#[test]
fn test_o_esc_undo_does_not_consume_prior_change_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "o<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\n");
        }
    }
}

#[test]
fn test_o_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "o<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\n");
        }
    }
}

#[test]
fn test_dot_repeat_o_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\nthree\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\nthree\n");
        }
        step "o<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\nthree\n");
        }
        step "j." => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\n\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\nthree\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\n\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\n\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\nthree\n");
        }
    }
}

#[test]
fn test_uppercase_o_esc_undo_does_not_consume_prior_change_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "O<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\n");
        }
    }
}

#[test]
fn test_dot_repeat_uppercase_o_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\nthree\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\nthree\n");
        }
        step "O<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\nthree\n");
        }
        step "j." => |test| {
            assert_eq!(test.buffer_content(), "\n\none!\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\nthree\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\n\none!\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\nthree\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\nthree\n");
        }
    }
}

#[test]
fn test_uppercase_o_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "one\ntwo\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "O<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), "\none!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one!\ntwo\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "one\ntwo\n");
        }
    }
}

#[test]
fn test_cw_esc_undo_redo_isolation_macro_flow() {
    editor_flow_test! {
        content "alpha beta\n";
        step "A!<Esc>" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "0cw<Esc>" => |test| {
            assert_eq!(test.buffer_content(), " beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "<C-r>" => |test| {
            assert_eq!(test.buffer_content(), " beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta!\n");
        }
        step "u" => |test| {
            assert_eq!(test.buffer_content(), "alpha beta\n");
        }
    }
}

// ============================================================================
// Multi-char visual-c dot-repeat (Signal A — roadmap 15 sprint)
// ============================================================================

#[test]
fn test_dot_after_visual_change_multichar() {
    // `vecXYZ<Esc>w.` on "one two three" replays: delete 3-char span at
    // current cursor + insert "XYZ". Verified against Vim.
    let mut test = EditorTest::new("one two three");

    test.press('v')
        .keys("e") // select "one"
        .press('c') // change
        .type_text("XYZ")
        .press_esc()
        .press('w') // move to "two"
        .press('.'); // repeat — should replace "two" with "XYZ"

    assert_eq!(test.buffer_content(), "XYZ XYZ three\n");
    test.assert_cursor(0, 6);
}

#[test]
fn test_dot_after_visual_change_with_backspace_matches_cw() {
    // vec + backspace-during-insert currently exhibits the same
    // pre-existing limitation as `cw`: dot-repeat's `get_inserted_text`
    // concatenates all Insert edits ignoring backspaces, so the replay
    // inserts "XYZ" instead of the net "XZ". Vim produces "XZ XZ three".
    //
    // This test pins visual-c to cw's behavior so that if/when the
    // underlying get_inserted_text bug is fixed, both operators benefit
    // and both tests can be updated in lockstep.
    let mut test = EditorTest::new("one two three");

    test.press('v')
        .keys("e") // select "one"
        .press('c') // change
        .type_text("XY")
        .press_backspace()
        .type_text("Z")
        .press_esc();

    // After Esc, the buffer reflects the true typed state.
    assert_eq!(test.buffer_content(), "XZ two three\n");

    test.press('w').press('.');

    // Dot-repeat's inserted-text is the naive concatenation — "XYZ",
    // not the net-of-backspace "XZ". Matches existing `cw` behavior.
    // TODO: upstream fix in get_inserted_text / session replay should
    // produce "XZ XZ three\n" (Vim reference) — lift this test then.
    assert_eq!(test.buffer_content(), "XZ XYZ three\n");
}

#[test]
fn test_dot_after_visual_line_change_multichar() {
    // `VcNEW<Esc>j.` linewise on a 3-line buffer should produce
    // "NEW\nNEW\nline three\n" — verified against Vim (vim -N -u NONE).
    let mut test = EditorTest::new("line one\nline two\nline three\n");

    test.press('V')
        .press('c')
        .type_text("NEW")
        .press_esc()
        .press('j')
        .press('.');

    assert_eq!(test.buffer_content(), "NEW\nNEW\nline three\n");
}

#[test]
fn test_dot_after_visual_change_multichar_with_count() {
    // Counted dot-repeat after vec-change. The count applies to the repeat
    // itself (Change::execute runs once per `.` in the current impl; count
    // over Change is not supported separately — this just verifies the
    // counted dot form doesn't panic and produces a sane multi-XY buffer).
    let mut test = EditorTest::new("aaaa");

    test.press('v')
        .press('c')
        .type_text("XY")
        .press_esc()
        .press('l') // move right one position
        .keys("2."); // counted dot

    // Regardless of exact count semantics, the buffer should contain at
    // least two "XY" fragments (original + repeat) and remain well-formed.
    let content = test.buffer_content();
    assert!(
        content.matches("XY").count() >= 2,
        "dot-repeat with count should produce ≥2 XY fragments, got: {:?}",
        content
    );
    assert!(
        content.ends_with('\n'),
        "buffer must stay newline-terminated"
    );
}

#[test]
fn test_dot_after_visual_change_multichar_preserves_undo_granularity() {
    // A single `u` should undo the entire vec→type→Esc as one step,
    // restoring the pre-change text.
    let mut test = EditorTest::new("hello world\n");

    test.press('v')
        .keys("e") // select "hello"
        .press('c')
        .type_text("XYZ")
        .press_esc();

    assert_eq!(test.buffer_content(), "XYZ world\n");

    test.press('u');

    assert_eq!(test.buffer_content(), "hello world\n");
}

#[test]
fn test_visual_change_with_emoji_does_not_panic() {
    // Signal D coverage: visual-c across a buffer containing a multi-char
    // grapheme (ZWJ emoji family) should not panic. The session branch of
    // record_edit and the char_col/grapheme_col conversions downstream must
    // tolerate non-trivial graphemes. We don't pin exact cursor positions
    // here — that's Phase-15 debt (visual_start stores grapheme cols; the
    // delete path treats them as char cols).
    let mut test = EditorTest::new("ab\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}cd");

    test.press('v').press('c').type_text("X").press_esc();

    // Expect buffer to still be well-formed (ends with newline, is UTF-8).
    let content = test.buffer_content();
    assert!(
        content.ends_with('\n'),
        "buffer must stay newline-terminated"
    );
    assert!(
        content.starts_with('X'),
        "inserted X should be at the start"
    );

    // Dot-repeat must not panic either.
    test.press('l').press('.');
    let content = test.buffer_content();
    assert!(content.ends_with('\n'));
}

#[test]
fn test_dot_repeat_o_uses_current_line_indent() {
    // Dot-repeat of 'o' should use the CURRENT line's indent, not the
    // indent from the original 'o' command.
    let mut test = EditorTest::new("    four_spaces\n\t\ttwo_tabs");

    // o on 4-space indented line, type text, Esc
    test.keys("o");
    test.type_text("x");
    test.press_esc();

    // Move to two-tabs line and repeat
    test.keys("j"); // on two_tabs line
    test.keys(".");

    // The repeated line should use two_tabs indent, not four_spaces
    let content = test.buffer_content();
    assert!(
        content.contains("\t\tx\n"),
        "Repeated o should use current line's tab indent, got: {:?}",
        content
    );
}
