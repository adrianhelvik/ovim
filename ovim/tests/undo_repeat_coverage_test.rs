#![allow(non_snake_case)]

mod helpers;
use helpers::EditorTest;

// ============================================================================
// Tests for operations migrated from Pattern A to Pattern B.
// Each test verifies: operation works, undo restores, redo re-applies,
// and dot-repeat re-evaluates at cursor.
// ============================================================================

// ============================================================================
// D (delete to end of line)
// ============================================================================

#[test]
fn test_D_undo_redo() {
    let mut test = EditorTest::new("hello world");
    test.keys("wD");
    assert_eq!(test.buffer_content(), "hello \n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "hello \n");
}

#[test]
fn test_D_dot_repeat() {
    let mut test = EditorTest::new("one two three\nfour five six");
    test.keys("wD");
    assert_eq!(test.buffer_content(), "one \nfour five six\n");

    test.keys("jw.");
    assert_eq!(test.buffer_content(), "one \nfour \n");
}

#[test]
fn test_D_dot_repeat_undo() {
    let mut test = EditorTest::new("abc def\nghi jkl");
    test.keys("wD");
    test.keys("jw.");
    assert_eq!(test.buffer_content(), "abc \nghi \n");

    // Undo the repeat
    test.keys("u");
    assert_eq!(test.buffer_content(), "abc \nghi jkl\n");

    // Undo the original
    test.keys("u");
    assert_eq!(test.buffer_content(), "abc def\nghi jkl\n");
}

// ============================================================================
// dd (delete line)
// ============================================================================

#[test]
fn test_dd_undo_redo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");
    test.keys("jdd");
    assert_eq!(test.buffer_content(), "line 1\nline 3\n");
    test.assert_cursor(1, 0);

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "line 1\nline 3\n");
}

#[test]
fn test_dd_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd");
    test.keys("dd");
    assert_eq!(test.buffer_content(), "b\nc\nd\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "c\nd\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "d\n");
}

#[test]
fn test_2dd_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");
    test.keys("2dd");
    assert_eq!(test.buffer_content(), "c\nd\ne\nf\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "e\nf\n");
}

#[test]
fn test_dd_dot_repeat_undo_chain() {
    let mut test = EditorTest::new("a\nb\nc\nd");
    test.keys("dd..");
    assert_eq!(test.buffer_content(), "d\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "c\nd\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "b\nc\nd\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "a\nb\nc\nd\n");
}

// ============================================================================
// dw (delete word)
// ============================================================================

#[test]
fn test_dw_undo_redo() {
    let mut test = EditorTest::new("one two three");
    test.keys("dw");
    assert_eq!(test.buffer_content(), "two three\n");
    test.assert_cursor(0, 0);

    test.keys("u");
    assert_eq!(test.buffer_content(), "one two three\n");
    test.assert_cursor(0, 0);

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "two three\n");
}

#[test]
fn test_dw_dot_repeat_different_word_lengths() {
    let mut test = EditorTest::new("short longword x");
    test.keys("dw");
    assert_eq!(test.buffer_content(), "longword x\n");

    // Dot repeat should delete "longword " (re-evaluating word at cursor)
    test.keys(".");
    assert_eq!(test.buffer_content(), "x\n");
}

#[test]
fn test_2dw_dot_repeat() {
    let mut test = EditorTest::new("a b c d e f");
    test.keys("2dw");
    assert_eq!(test.buffer_content(), "c d e f\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "e f\n");
}

// ============================================================================
// dj (delete line down)
// ============================================================================

#[test]
fn test_dj_undo_redo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");
    test.keys("dj");
    assert_eq!(test.buffer_content(), "line 3\nline 4\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "line 3\nline 4\n");
}

#[test]
fn test_dj_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");
    test.keys("dj");
    assert_eq!(test.buffer_content(), "c\nd\ne\nf\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "e\nf\n");
}

// ============================================================================
// dk (delete line up)
// ============================================================================

#[test]
fn test_dk_undo_redo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");
    test.keys("jjdk");
    assert_eq!(test.buffer_content(), "line 1\nline 4\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "line 1\nline 4\n");
}

#[test]
fn test_dk_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne\nf");
    // Go to line 5 (index 4), dk deletes lines 4 and 3
    test.keys("4jdk");
    assert_eq!(test.buffer_content(), "a\nb\nc\nf\n");

    // Now on line 3 (f), dot-repeat: dk deletes lines 3 and 2
    test.keys(".");
    assert_eq!(test.buffer_content(), "a\nb\n");
}

// ============================================================================
// d} / d{ (delete paragraph forward/backward)
// ============================================================================

#[test]
fn test_d_paragraph_forward_undo_redo() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc\nddd");
    test.keys("d}");
    assert_eq!(test.buffer_content(), "\nccc\nddd\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n\nccc\nddd\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "\nccc\nddd\n");
}

#[test]
fn test_d_paragraph_forward_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc\nddd\n\neee");
    test.keys("d}");
    assert_eq!(test.buffer_content(), "\nccc\nddd\n\neee\n");

    test.keys("j"); // Move into the paragraph
    test.keys("d}");
    assert_eq!(test.buffer_content(), "\n\neee\n");
}

#[test]
fn test_d_paragraph_backward_undo_redo() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc\nddd");
    // d{ from "ddd" deletes backward to paragraph boundary (blank line),
    // removing both the blank line and "ccc"
    test.keys("Gd{");
    assert_eq!(test.buffer_content(), "aaa\nbbb\nddd\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n\nccc\nddd\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aaa\nbbb\nddd\n");
}

// ============================================================================
// dl (delete char right — same as x but via d+l)
// ============================================================================

#[test]
fn test_dl_undo_redo() {
    let mut test = EditorTest::new("hello");
    test.keys("dl");
    assert_eq!(test.buffer_content(), "ello\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "ello\n");
}

#[test]
fn test_dl_dot_repeat() {
    let mut test = EditorTest::new("abcde");
    test.keys("dl..");
    assert_eq!(test.buffer_content(), "de\n");
}

// ============================================================================
// Text object deletes — dip, dap, dis, das, di", da", di(, da(
// ============================================================================

#[test]
fn test_dip_undo_redo() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc");
    test.keys("dip");
    assert_eq!(test.buffer_content(), "\nccc\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n\nccc\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "\nccc\n");
}

#[test]
fn test_dip_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc\nddd\n\neee");
    test.keys("dip");
    // Deletes first paragraph (aaa, bbb)
    assert_eq!(test.buffer_content(), "\nccc\nddd\n\neee\n");

    // Move into next paragraph and repeat
    test.keys("j");
    test.keys(".");
    assert_eq!(test.buffer_content(), "\n\neee\n");
}

#[test]
fn test_dap_undo_redo() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc");
    test.keys("dap");
    assert_eq!(test.buffer_content(), "ccc\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n\nccc\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "ccc\n");
}

#[test]
fn test_dis_undo_redo() {
    let mut test = EditorTest::new("First sentence. Second sentence.");
    test.keys("dis");

    let after = test.buffer_content();
    // Should have deleted the first sentence
    test.keys("u");
    assert_eq!(test.buffer_content(), "First sentence. Second sentence.\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), after);
}

#[test]
fn test_di_quote_undo_redo() {
    let mut test = EditorTest::new("say \"hello world\" here");
    test.keys("f\"ldi\"");
    assert_eq!(test.buffer_content(), "say \"\" here\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "say \"hello world\" here\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "say \"\" here\n");
}

#[test]
fn test_di_quote_dot_repeat() {
    let mut test = EditorTest::new("a \"one\" b \"two\" c");
    test.keys("f\"ldi\"");
    assert_eq!(test.buffer_content(), "a \"\" b \"two\" c\n");

    // Move to next quoted string and repeat
    test.keys("f\"l.");
    assert_eq!(test.buffer_content(), "a \"\" b \"\" c\n");
}

#[test]
fn test_da_quote_undo_redo() {
    let mut test = EditorTest::new("say \"hello\" here");
    test.keys("f\"da\"");
    assert_eq!(test.buffer_content(), "say  here\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "say \"hello\" here\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "say  here\n");
}

#[test]
fn test_di_paren_undo_redo() {
    let mut test = EditorTest::new("fn(a, b, c)");
    test.keys("f(ldi(");
    assert_eq!(test.buffer_content(), "fn()\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "fn(a, b, c)\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "fn()\n");
}

#[test]
fn test_di_paren_dot_repeat() {
    let mut test = EditorTest::new("fn(abc) + gn(xyz)");
    test.keys("f(ldi(");
    assert_eq!(test.buffer_content(), "fn() + gn(xyz)\n");

    test.keys("f(l.");
    assert_eq!(test.buffer_content(), "fn() + gn()\n");
}

#[test]
fn test_da_paren_undo_redo() {
    let mut test = EditorTest::new("fn(a, b) end");
    test.keys("f(da(");
    assert_eq!(test.buffer_content(), "fn end\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "fn(a, b) end\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "fn end\n");
}

#[test]
fn test_di_bracket_undo_redo() {
    let mut test = EditorTest::new("[1, 2, 3]");
    test.keys("ldi[");
    assert_eq!(test.buffer_content(), "[]\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "[1, 2, 3]\n");
}

#[test]
fn test_di_curly_undo_redo() {
    let mut test = EditorTest::new("{ foo: bar }");
    test.keys("ldi{");
    assert_eq!(test.buffer_content(), "{}\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "{ foo: bar }\n");
}

// ============================================================================
// Paste operations — p, P (char and line modes)
// ============================================================================

#[test]
fn test_paste_after_line_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");
    // Yank first line, paste after
    test.keys("yyp");
    assert_eq!(test.buffer_content(), "aaa\naaa\nbbb\nccc\n");

    // Dot-repeat should paste again
    test.keys(".");
    assert_eq!(test.buffer_content(), "aaa\naaa\naaa\nbbb\nccc\n");
}

#[test]
fn test_paste_before_line_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");
    test.keys("yyP");
    assert_eq!(test.buffer_content(), "aaa\naaa\nbbb\nccc\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "aaa\naaa\naaa\nbbb\nccc\n");
}

#[test]
fn test_paste_after_char_undo_redo() {
    let mut test = EditorTest::new("hello world");
    // Yank "hello", move to end of "world", paste after
    test.keys("yiw$p");
    let after = test.buffer_content();
    assert!(after.contains("worldhello"), "got: {}", after);

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), after);
}

#[test]
fn test_paste_before_char_undo() {
    let mut test = EditorTest::new("hello world");
    test.keys("yiwwP");
    let after = test.buffer_content();
    assert!(after.contains("helloworld"), "got: {}", after);

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");
}

#[test]
fn test_paste_line_undo_redo_cycle() {
    let mut test = EditorTest::new("aaa\nbbb");
    test.keys("yyp");
    assert_eq!(test.buffer_content(), "aaa\naaa\nbbb\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aaa\naaa\nbbb\n");

    // Undo again
    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n");
}

// ============================================================================
// cc (change line) — semantic change repeat coverage
// ============================================================================

#[test]
fn test_cc_undo_redo() {
    let mut test = EditorTest::new("hello world\nsecond line");
    test.keys("ccnew text<Esc>");
    assert_eq!(test.buffer_content(), "new text\nsecond line\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\nsecond line\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "new text\nsecond line\n");
}

#[test]
fn test_cc_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");
    test.keys("ccXXX<Esc>");
    assert_eq!(test.buffer_content(), "XXX\nbbb\nccc\n");

    test.keys("j.");
    assert_eq!(test.buffer_content(), "XXX\nXXX\nccc\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "XXX\nbbb\nccc\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "XXX\nXXX\nccc\n");
}

// ============================================================================
// C (change to end of line) — semantic change repeat coverage
// ============================================================================

#[test]
fn test_big_c_undo_redo() {
    let mut test = EditorTest::new("hello world");
    test.keys("wCnew<Esc>");
    assert_eq!(test.buffer_content(), "hello new\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "hello new\n");
}

// ============================================================================
// x, X dot-repeat undo chain
// ============================================================================

#[test]
fn test_x_dot_repeat_undo_chain() {
    let mut test = EditorTest::new("abcde");
    test.keys("x.."); // delete a, b, c
    assert_eq!(test.buffer_content(), "de\n");

    test.keys("u"); // undo last dot
    assert_eq!(test.buffer_content(), "cde\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "bcde\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "abcde\n");
}

// ============================================================================
// df, dt (char motion deletes) — semantic repeat coverage
// ============================================================================

#[test]
fn test_df_undo_redo() {
    let mut test = EditorTest::new("hello, world!");
    test.keys("df,");
    assert_eq!(test.buffer_content(), " world!\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello, world!\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), " world!\n");
}

#[test]
fn test_dt_undo_redo() {
    let mut test = EditorTest::new("hello, world!");
    test.keys("dt,");
    assert_eq!(test.buffer_content(), ", world!\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello, world!\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), ", world!\n");
}

#[test]
fn test_dF_undo_redo() {
    // Backward F is exclusive: the 'Y' under the cursor survives (OV-00288).
    let mut test = EditorTest::new("aXbXcY");
    test.keys("$dFX");
    assert_eq!(test.buffer_content(), "aXbY\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aXbXcY\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aXbY\n");
}

#[test]
fn test_dT_undo_redo() {
    // Backward T is exclusive: the 'Y' under the cursor survives (OV-00288).
    let mut test = EditorTest::new("aXbXcY");
    test.keys("$dTX");
    assert_eq!(test.buffer_content(), "aXbXY\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "aXbXcY\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aXbXY\n");
}

#[test]
fn test_df_dot_repeat() {
    let mut test = EditorTest::new("a,b,c,d");
    test.keys("df,");
    assert_eq!(test.buffer_content(), "b,c,d\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "c,d\n");

    test.keys(".");
    assert_eq!(test.buffer_content(), "d\n");
}

#[test]
fn test_df_dot_repeat_undo_redo_chain() {
    let mut test = EditorTest::new("a,b,c,d");
    test.keys("df,.");
    assert_eq!(test.buffer_content(), "c,d\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "b,c,d\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "a,b,c,d\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "b,c,d\n");

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "c,d\n");
}

// ============================================================================
// Multi-step integration: delete, dot-repeat, undo all, redo all
// ============================================================================

#[test]
fn test_dw_full_roundtrip() {
    let mut test = EditorTest::new("one two three four");

    // Delete first word
    test.keys("dw");
    assert_eq!(test.buffer_content(), "two three four\n");

    // Dot repeat
    test.keys(".");
    assert_eq!(test.buffer_content(), "three four\n");

    // Dot repeat again
    test.keys(".");
    assert_eq!(test.buffer_content(), "four\n");

    // Undo all three
    test.keys("uuu");
    assert_eq!(test.buffer_content(), "one two three four\n");

    // Redo all three
    test.keys("<C-r><C-r><C-r>");
    assert_eq!(test.buffer_content(), "four\n");
}

#[test]
fn test_mixed_operations_undo_isolation() {
    // Verify that different operations create independent undo entries
    let mut test = EditorTest::new("hello world\nfoo bar");

    test.keys("x"); // delete 'h'
    test.keys("jdd"); // delete "foo bar"
    assert_eq!(test.buffer_content(), "ello world\n");

    test.keys("u"); // undo dd
    assert_eq!(test.buffer_content(), "ello world\nfoo bar\n");

    test.keys("u"); // undo x
    assert_eq!(test.buffer_content(), "hello world\nfoo bar\n");
}

#[test]
fn test_diw_then_dw_independent_undo() {
    let mut test = EditorTest::new("alpha bravo charlie");

    test.keys("diw"); // delete "alpha"
    test.keys("dw"); // delete " bravo" (or "bravo " depending on cursor)

    // Two undos should restore everything
    test.keys("uu");
    assert_eq!(test.buffer_content(), "alpha bravo charlie\n");
}

// ============================================================================
// dG (delete to last line)
// ============================================================================

#[test]
fn test_dG_undo_redo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("dG");
    assert_eq!(test.buffer_content(), "\n");

    // Single undo restores all lines
    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");

    // Redo re-applies
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "\n");
}

#[test]
fn test_dG_from_middle_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("j"); // Move to line 2
    test.keys("dG");
    assert_eq!(test.buffer_content(), "line 1\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");
}

// ============================================================================
// dgg (delete to first line)
// ============================================================================

#[test]
fn test_dgg_undo_redo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("G"); // Go to last line
    test.keys("dgg");
    assert_eq!(test.buffer_content(), "\n");

    // Single undo restores all lines
    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");

    // Redo re-applies
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "\n");
}

#[test]
fn test_dgg_from_middle_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");

    test.keys("jj"); // Move to line 3
    test.keys("dgg");
    assert_eq!(test.buffer_content(), "line 4\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");
}

#[test]
fn test_dG_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc\nddd\neee");

    test.keys("jj"); // Move to line 3 (ccc)
    test.keys("dG"); // Delete lines 3-5
    assert_eq!(test.buffer_content(), "aaa\nbbb\n");

    // Dot-repeat at new position: cursor is on line 1 (bbb)
    // dG from line 1 should delete lines 1-end
    test.keys(".");
    assert_eq!(test.buffer_content(), "aaa\n");
}

#[test]
fn test_dgg_dot_repeat() {
    let mut test = EditorTest::new("aaa\nbbb\nccc\nddd\neee");

    test.keys("jj"); // Move to line 3 (ccc)
    test.keys("dgg"); // Delete lines 1-3
    assert_eq!(test.buffer_content(), "ddd\neee\n");

    // Dot-repeat: cursor is on line 0 (ddd)
    // dgg from line 0 targets line 0 (itself + lines above, so just line 0)
    test.keys("j"); // Move to line 1 (eee)
    test.keys("."); // dgg from line 1 deletes lines 0-1
    assert_eq!(test.buffer_content(), "\n");
}

// ============================================================================
// d% (delete to matching bracket)
// ============================================================================

#[test]
fn test_d_percent_undo_redo() {
    let mut test = EditorTest::new("(hello)");
    test.keys("d%");
    assert_eq!(test.buffer_content(), "\n");

    // Single undo restores
    test.keys("u");
    assert_eq!(test.buffer_content(), "(hello)\n");

    // Redo re-applies
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "\n");
}

#[test]
fn test_d_percent_from_closing_bracket() {
    let mut test = EditorTest::new("(hello) world");
    test.keys("f)"); // Move to closing paren
    test.keys("d%");
    assert_eq!(test.buffer_content(), " world\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "(hello) world\n");
}

#[test]
fn test_d_percent_dot_repeat() {
    let mut test = EditorTest::new("[abc] [xyz]");
    test.keys("d%"); // Delete [abc]
    assert_eq!(test.buffer_content(), " [xyz]\n");

    test.keys("l."); // Move to [ and repeat
    assert_eq!(test.buffer_content(), " \n");
}

#[test]
fn test_d_percent_no_bracket() {
    let mut test = EditorTest::new("hello");
    test.keys("d%");
    // No bracket at cursor — no-op
    assert_eq!(test.buffer_content(), "hello\n");
}

// ============================================================================
// r (replace character)
// ============================================================================

#[test]
fn test_r_undo_redo() {
    let mut test = EditorTest::new("hello");
    test.keys("ra");
    assert_eq!(test.buffer_content(), "aello\n");

    // Single undo restores
    test.keys("u");
    assert_eq!(test.buffer_content(), "hello\n");

    // Redo re-applies
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aello\n");
}

#[test]
fn test_3r_undo_redo() {
    let mut test = EditorTest::new("hello");
    test.keys("3ra");
    assert_eq!(test.buffer_content(), "aaalo\n");
    test.assert_cursor(0, 2);

    // Single undo restores all 3 chars
    test.keys("u");
    assert_eq!(test.buffer_content(), "hello\n");
    test.assert_cursor(0, 0);

    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), "aaalo\n");
    test.assert_cursor(0, 2);
}

#[test]
fn test_r_dot_repeat() {
    let mut test = EditorTest::new("hello world");
    test.keys("rx"); // Replace 'h' with 'x'
    assert_eq!(test.buffer_content(), "xello world\n");

    test.keys("w."); // Move to 'w', repeat
    assert_eq!(test.buffer_content(), "xello xorld\n");
}

// ============================================================================
// Change operators — RepeatAction::Change (semantic dot-repeat)
// ============================================================================

// ----------------------------------------------------------------------------
// cc (change line)
// ----------------------------------------------------------------------------

#[test]
fn test_cc_undo() {
    let mut test = EditorTest::new("hello world\nsecond line");
    test.keys("cc").type_text("NEW").press_esc();
    assert_eq!(test.buffer_content(), "NEW\nsecond line\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\nsecond line\n");
}

#[test]
fn test_cc_semantic_dot_repeat() {
    let mut test = EditorTest::new("line one\nline two\nline three");
    test.keys("cc").type_text("changed").press_esc();
    assert_eq!(test.buffer_content(), "changed\nline two\nline three\n");

    test.keys("j."); // Move to line two, repeat
    assert_eq!(test.buffer_content(), "changed\nchanged\nline three\n");
}

#[test]
fn test_cc_semantic_dot_repeat_undo() {
    let mut test = EditorTest::new("aaa\nbbb\nccc");
    test.keys("cc").type_text("X").press_esc();
    test.keys("j.");
    assert_eq!(test.buffer_content(), "X\nX\nccc\n");

    // Single undo reverts the repeat
    test.keys("u");
    assert_eq!(test.buffer_content(), "X\nbbb\nccc\n");

    // Second undo reverts the original
    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\nccc\n");
}

#[test]
fn test_3cc_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3\nline 4");
    test.keys("3cc").type_text("merged").press_esc();
    assert_eq!(test.buffer_content(), "merged\nline 4\n");

    // Single undo restores all 3 lines
    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\nline 4\n");
}

#[test]
fn test_cc_preserves_indent() {
    let mut test = EditorTest::new("    indented\nnot indented");
    test.keys("cc").type_text("new").press_esc();
    assert_eq!(test.buffer_content(), "    new\nnot indented\n");
}

// ----------------------------------------------------------------------------
// C (change to end of line)
// ----------------------------------------------------------------------------

#[test]
fn test_C_undo() {
    let mut test = EditorTest::new("hello world");
    test.keys("w").press('C').type_text("NEW").press_esc();
    assert_eq!(test.buffer_content(), "hello NEW\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");
}

#[test]
fn test_C_dot_repeat() {
    let mut test = EditorTest::new("aaa bbb\nccc ddd");
    test.keys("w").press('C').type_text("END").press_esc();
    assert_eq!(test.buffer_content(), "aaa END\nccc ddd\n");

    test.keys("j0w."); // Move to second line, word motion, repeat
    assert_eq!(test.buffer_content(), "aaa END\nccc END\n");
}

// ----------------------------------------------------------------------------
// c$ (change to end of line via operator+motion)
// ----------------------------------------------------------------------------

#[test]
fn test_c_dollar_undo() {
    let mut test = EditorTest::new("hello world");
    test.keys("wc$").type_text("NEW").press_esc();
    assert_eq!(test.buffer_content(), "hello NEW\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");
}

#[test]
fn test_c_dollar_dot_repeat() {
    let mut test = EditorTest::new("aaa bbb\nccc ddd");
    test.keys("wc$").type_text("END").press_esc();
    assert_eq!(test.buffer_content(), "aaa END\nccc ddd\n");

    // Move to second line, word motion, then repeat
    test.keys("j0w.");
    assert_eq!(test.buffer_content(), "aaa END\nccc END\n");
}

// ----------------------------------------------------------------------------
// s (substitute character)
// ----------------------------------------------------------------------------

#[test]
fn test_s_undo() {
    let mut test = EditorTest::new("hello");
    test.press('s').type_text("H").press_esc();
    assert_eq!(test.buffer_content(), "Hello\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello\n");
}

#[test]
fn test_s_dot_repeat() {
    let mut test = EditorTest::new("abc");
    test.press('s').type_text("X").press_esc();
    assert_eq!(test.buffer_content(), "Xbc\n");

    test.keys("l.");
    assert_eq!(test.buffer_content(), "XXc\n");
}

// ----------------------------------------------------------------------------
// S (substitute line)
// ----------------------------------------------------------------------------

#[test]
fn test_S_undo() {
    let mut test = EditorTest::new("hello world\nsecond");
    test.press('S').type_text("NEW").press_esc();
    assert_eq!(test.buffer_content(), "NEW\nsecond\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\nsecond\n");
}

#[test]
fn test_S_dot_repeat() {
    let mut test = EditorTest::new("line one\nline two\nline three");
    test.press('S').type_text("X").press_esc();
    assert_eq!(test.buffer_content(), "X\nline two\nline three\n");

    test.keys("j.");
    assert_eq!(test.buffer_content(), "X\nX\nline three\n");
}

// ----------------------------------------------------------------------------
// cj (change line + below)
// ----------------------------------------------------------------------------

#[test]
fn test_cj_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");
    test.keys("cj").type_text("merged").press_esc();
    assert_eq!(test.buffer_content(), "merged\nline 3\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
}

#[test]
fn test_cj_dot_repeat() {
    let mut test = EditorTest::new("a\nb\nc\nd\ne");
    test.keys("cj").type_text("X").press_esc();
    assert_eq!(test.buffer_content(), "X\nc\nd\ne\n");

    test.keys("j.");
    assert_eq!(test.buffer_content(), "X\nX\ne\n");
}

// ----------------------------------------------------------------------------
// ck (change line + above)
// ----------------------------------------------------------------------------

#[test]
fn test_ck_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");
    test.keys("jck").type_text("merged").press_esc();
    assert_eq!(test.buffer_content(), "merged\nline 3\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
}

// ----------------------------------------------------------------------------
// cG (change to last line)
// ----------------------------------------------------------------------------

#[test]
fn test_cG_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");
    test.keys("jcG").type_text("rest").press_esc();
    assert_eq!(test.buffer_content(), "line 1\nrest\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
}

// ----------------------------------------------------------------------------
// cgg (change to first line)
// ----------------------------------------------------------------------------

#[test]
fn test_cgg_undo() {
    let mut test = EditorTest::new("line 1\nline 2\nline 3");
    test.keys("Gcgg").type_text("all").press_esc();
    assert_eq!(test.buffer_content(), "all\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "line 1\nline 2\nline 3\n");
}

// ----------------------------------------------------------------------------
// c} (change to paragraph forward)
// ----------------------------------------------------------------------------

#[test]
fn test_c_brace_forward_undo() {
    let mut test = EditorTest::new("aaa\nbbb\n\nccc");
    test.keys("c}").type_text("X").press_esc();

    // Verify the change, then undo
    let after_change = test.buffer_content();
    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\nbbb\n\nccc\n");

    // Redo should restore the changed state
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), after_change);
}

// ----------------------------------------------------------------------------
// c{ (change to paragraph backward)
// ----------------------------------------------------------------------------

#[test]
fn test_c_brace_backward_undo() {
    let mut test = EditorTest::new("aaa\n\nbbb\nccc");
    test.keys("Gc{").type_text("X").press_esc();

    // Verify the change, then undo
    let after_change = test.buffer_content();
    test.keys("u");
    assert_eq!(test.buffer_content(), "aaa\n\nbbb\nccc\n");

    // Redo should restore
    test.keys("<C-r>");
    assert_eq!(test.buffer_content(), after_change);
}
