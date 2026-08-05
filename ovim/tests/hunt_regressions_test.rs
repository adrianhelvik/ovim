//! Regression tests for the 2026-08 bug hunt (OV-00286..OV-00292).
//! Each test encodes vim's reference behavior for the fixed defect.

mod helpers;

use helpers::EditorTest;

// ── OV-00286: dit/dat off-by-one (exclusive end) ────────────────────────────

#[test]
fn dit_deletes_all_inner_content() {
    let mut test = EditorTest::new("x<b>abc</b>");
    test.keys("fb"); // cursor on 'b' of abc? 'fb' finds the tag's b first
    test.keys("0fa"); // cursor on 'a' of abc
    test.keys("dit");
    assert_eq!(test.buffer_content(), "x<b></b>\n");
}

#[test]
fn dat_deletes_the_whole_tag() {
    let mut test = EditorTest::new("x<b>abc</b>");
    test.keys("0fa");
    test.keys("dat");
    assert_eq!(test.buffer_content(), "x\n");
}

// ── OV-00287: tag at buffer offset 0 ────────────────────────────────────────

#[test]
fn dit_works_when_tag_starts_at_buffer_start() {
    let mut test = EditorTest::new("<b>abc</b>");
    test.keys("0fa");
    test.keys("dit");
    assert_eq!(test.buffer_content(), "<b></b>\n");
}

// ── OV-00288: backward F/T are exclusive with operators ─────────────────────

#[test]
fn d_big_f_excludes_cursor_char() {
    let mut test = EditorTest::new("axbc");
    test.keys("$"); // cursor on 'c'
    test.keys("dFx");
    assert_eq!(test.buffer_content(), "ac\n");
}

#[test]
fn d_big_t_excludes_cursor_char() {
    let mut test = EditorTest::new("axbc");
    test.keys("$");
    test.keys("dTx");
    assert_eq!(test.buffer_content(), "axc\n");
}

#[test]
fn y_big_f_register_excludes_cursor_char() {
    let mut test = EditorTest::new("axbc");
    test.keys("$");
    test.keys("yFx");
    // yF moves the cursor to the target; the register holds "xb" (not
    // "xbc"), so pasting after the 'x' splices it mid-word like vim.
    test.keys("p");
    assert_eq!(test.buffer_content(), "axxbbc\n");
}

// ── OV-00289: d{count}w spans line boundaries ───────────────────────────────

#[test]
fn d2w_crosses_the_newline_like_vim() {
    let mut test = EditorTest::new("one\ntwo three");
    test.keys("gg0");
    test.keys("d2w");
    assert_eq!(test.buffer_content(), "three\n");
}

#[test]
fn dw_still_stops_at_eol_for_last_word_on_line() {
    let mut test = EditorTest::new("one\ntwo");
    test.keys("gg0");
    test.keys("dw");
    assert_eq!(test.buffer_content(), "\ntwo\n");
}

// ── OV-00290: {count}J joins count lines, not count joins ───────────────────

#[test]
fn three_j_joins_three_lines() {
    let mut test = EditorTest::new("a\nb\nc\nd");
    test.keys("gg");
    test.keys("3J");
    assert_eq!(test.buffer_content(), "a b c\nd\n");
}

#[test]
fn plain_j_joins_two_lines() {
    let mut test = EditorTest::new("a\nb\nc");
    test.keys("gg");
    test.keys("J");
    assert_eq!(test.buffer_content(), "a b\nc\n");
}

// ── OV-00291: blockwise paste appends rows past the last line ───────────────

#[test]
fn block_paste_past_last_line_appends_padded_rows() {
    let mut test = EditorTest::new("ab\ncd");
    // Yank block "a"/"c" (col 0, both lines), then paste with cursor on the
    // last line: the second block row must be appended as a new line.
    test.keys("gg0");
    test.press_with(ovim_core::KeyCode::Char('v'), ovim_core::Modifiers::CONTROL);
    test.keys("jy");
    test.keys("jp");
    assert_eq!(test.buffer_content(), "ab\ncad\n c\n");
}

// ── OV-00292: y'a keeps line boundaries in the register ─────────────────────

#[test]
fn linewise_yank_to_mark_preserves_newlines() {
    let mut test = EditorTest::new("aaa\nbbb\nccc\nddd");
    test.keys("gg");
    test.keys("ma");
    test.keys("jj");
    test.keys("y'a");
    test.keys("G");
    test.keys("p");
    assert_eq!(test.buffer_content(), "aaa\nbbb\nccc\nddd\naaa\nbbb\nccc\n");
}

// ── OV-00293: d}/d{ exclusive-motion adjustments (vim :help exclusive) ──────

#[test]
fn d_paragraph_forward_midline_is_charwise_and_keeps_blank_line() {
    // vim: "foo bar\n\nbaz", cursor (0,4), d} → "foo \n\nbaz"; register
    // holds charwise "bar", so p splices inline.
    let mut test = EditorTest::new("foo bar\n\nbaz");
    test.keys("gg04l");
    test.keys("d}");
    assert_eq!(test.buffer_content(), "foo \n\nbaz\n");
    test.keys("p");
    assert_eq!(test.buffer_content(), "foo bar\n\nbaz\n");
}

#[test]
fn d_paragraph_forward_from_line_start_is_linewise() {
    // vim: d} at col 0 deletes the paragraph's lines wholesale.
    let mut test = EditorTest::new("foo bar\n\nbaz");
    test.keys("gg0");
    test.keys("d}");
    assert_eq!(test.buffer_content(), "\nbaz\n");
}

#[test]
fn d_paragraph_backward_excludes_cursor_char() {
    // vim: "foo\n\nbar baz", cursor on 'b' of baz (2,4), d{ deletes from
    // the blank line up to (not including) the cursor → "foo\nbaz".
    let mut test = EditorTest::new("foo\n\nbar baz");
    test.keys("gg");
    test.keys("2j4l");
    test.keys("d{");
    assert_eq!(test.buffer_content(), "foo\nbaz\n");
}

#[test]
fn y_paragraph_forward_midline_register_is_charwise() {
    // vim: y} from (0,4) yanks "bar" charwise; p pastes inline after cursor.
    let mut test = EditorTest::new("foo bar\n\nbaz");
    test.keys("gg04l");
    test.keys("y}");
    assert_eq!(test.buffer_content(), "foo bar\n\nbaz\n");
    test.keys("p");
    assert_eq!(test.buffer_content(), "foo bbarar\n\nbaz\n");
}
