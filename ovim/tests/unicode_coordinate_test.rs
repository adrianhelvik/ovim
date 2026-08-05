//! Regression tests for grapheme/char coordinate conversions (OV-00299).
//!
//! `é` written as `e\u{301}` (e + combining acute) is ONE grapheme but TWO
//! chars — any code path that feeds a grapheme col into a char-indexed
//! operation drifts by one char per such cluster before the position.

mod helpers;

use helpers::EditorTest;

// One grapheme, two chars: e + combining acute accent.
const E_ACUTE: &str = "e\u{301}";

// ── visual charwise selection (yank + delete) ───────────────────────────────

#[test]
fn visual_yank_after_combining_grapheme_takes_whole_selection() {
    // Line: é a b  → graphemes [é, a, b], chars [e, ́, a, b].
    // Select "ab" (graphemes 1-2) and yank: must not shift left into é's
    // combining mark.
    let content = format!("{}ab", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0l"); // cursor on 'a' (grapheme 1)
    test.keys("vly"); // select a..b, yank
    test.keys("$p"); // paste at end
    assert_eq!(test.buffer_content(), format!("{}abab\n", E_ACUTE));
}

#[test]
fn visual_delete_after_combining_grapheme_removes_whole_selection() {
    let content = format!("{}ab", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0l");
    test.keys("vld"); // delete "ab"
    assert_eq!(test.buffer_content(), format!("{}\n", E_ACUTE));
}

#[test]
fn visual_delete_of_combining_grapheme_deletes_whole_cluster() {
    // Selecting é itself must delete BOTH chars, not just the base 'e'.
    let content = format!("a{}b", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0l"); // cursor on é
    test.keys("vd");
    assert_eq!(test.buffer_content(), "ab\n");
}

// ── charwise operator ranges (f/t with operators) ───────────────────────────

#[test]
fn dfx_across_combining_grapheme_deletes_correct_range() {
    // a é b x c → dfx from 'a' must delete a..x inclusive (chars a,e,́,b,x).
    let content = format!("a{}bxc", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0");
    test.keys("dfx");
    assert_eq!(test.buffer_content(), "c\n");
}

#[test]
fn yank_f_after_combining_grapheme_register_is_correct() {
    let content = format!("{}abc", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0");
    test.keys("yfb"); // yank é a b
    test.keys("$p");
    assert_eq!(
        test.buffer_content(),
        format!("{}abc{}ab\n", E_ACUTE, E_ACUTE)
    );
}

// ── y0 with combining grapheme before the cursor ────────────────────────────

#[test]
fn y0_with_combining_grapheme_yanks_up_to_cursor() {
    // é a b, cursor on b (grapheme 2): y0 yanks é a (all chars before b).
    let content = format!("{}ab", E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("$"); // cursor on 'b'
    test.keys("y0");
    test.keys("$p"); // paste after b
    assert_eq!(
        test.buffer_content(),
        format!("{}ab{}a\n", E_ACUTE, E_ACUTE)
    );
}

// ── blockwise paste with combining graphemes before the paste column ────────

#[test]
fn block_paste_after_combining_grapheme_lands_on_grapheme_column() {
    // Two lines starting with é; block-yank col 1 (x/y), paste after col 1.
    let content = format!("{}xq\n{}yr", E_ACUTE, E_ACUTE);
    let mut test = EditorTest::new(&content);
    test.keys("0l"); // cursor on x (grapheme 1)
    test.press_with(ovim_core::KeyCode::Char('v'), ovim_core::Modifiers::CONTROL);
    test.keys("jy"); // block yank x/y
    test.keys("gg0l");
    test.keys("p"); // paste after grapheme 1 on both lines
    assert_eq!(
        test.buffer_content(),
        format!("{}xxq\n{}yyr\n", E_ACUTE, E_ACUTE)
    );
}
