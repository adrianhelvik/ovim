//! Regression tests: x / X / r must operate on whole grapheme clusters, not
//! individual Unicode scalars. A base char followed by a combining mark is a
//! single grapheme; editing it scalar-by-scalar corrupts the cluster.

#![allow(non_snake_case)]

mod helpers;
use helpers::EditorTest;

// "é" written as base 'e' + U+0301 combining acute = 2 scalars, 1 grapheme.
const E_ACUTE: &str = "e\u{0301}";

#[test]
fn test_x_deletes_whole_grapheme_cluster() {
    let mut test = EditorTest::new(&format!("{E_ACUTE}x"));
    // cursor on the é grapheme (grapheme col 0)
    test.press('x');
    assert_eq!(
        test.buffer_content(),
        "x\n",
        "x should delete the whole é grapheme"
    );
}

#[test]
fn test_X_deletes_whole_grapheme_cluster() {
    let mut test = EditorTest::new(&format!("{E_ACUTE}x"));
    // move right one grapheme onto 'x', then X deletes the é before it
    test.press('l').press('X');
    assert_eq!(
        test.buffer_content(),
        "x\n",
        "X should delete the whole é grapheme before cursor"
    );
}

#[test]
fn test_r_replaces_whole_grapheme_cluster() {
    let mut test = EditorTest::new(&format!("{E_ACUTE}x"));
    test.press('r').press('z');
    assert_eq!(
        test.buffer_content(),
        "zx\n",
        "r should replace the whole é grapheme with a single char, not split the cluster"
    );
}

#[test]
fn test_replace_mode_replaces_whole_grapheme_cluster() {
    let mut test = EditorTest::new(&format!("{E_ACUTE}x"));
    test.keys("RZ<Esc>");

    assert_eq!(
        test.buffer_content(),
        "Zx\n",
        "R should replace the whole decomposed grapheme"
    );
}

#[test]
fn test_replace_mode_backspace_restores_whole_grapheme_cluster() {
    let original = format!("{E_ACUTE}x");
    let mut test = EditorTest::new(&original);
    test.keys("RZ<BS><Esc>");

    assert_eq!(test.buffer_content(), format!("{original}\n"));
}

#[test]
fn test_replace_mode_backspace_distinguishes_overwrite_from_eol_insert() {
    let mut test = EditorTest::new("a");
    test.keys("RXY<BS><Esc>");

    assert_eq!(test.buffer_content(), "X\n");
    test.press('u');
    assert_eq!(test.buffer_content(), "a\n");
}

#[test]
fn test_replace_mode_dot_repeat_replaces_target_graphemes() {
    let mut test = EditorTest::new(&format!("{E_ACUTE}x\n{E_ACUTE}y"));
    test.keys("RZ<Esc>j0.");

    assert_eq!(test.buffer_content(), "Zx\nZy\n");
}

#[test]
fn test_replace_mode_replaces_whole_zwj_emoji() {
    let mut test = EditorTest::new("👨‍👩‍👧‍👦x");
    test.keys("RZ<Esc>");

    assert_eq!(test.buffer_content(), "Zx\n");
}

#[test]
fn test_x_with_count_deletes_multiple_graphemes() {
    // Two é graphemes then 'x'
    let mut test = EditorTest::new(&format!("{E_ACUTE}{E_ACUTE}x"));
    test.press('2').press('x');
    assert_eq!(
        test.buffer_content(),
        "x\n",
        "2x should delete two whole graphemes"
    );
}

#[test]
fn test_x_on_flag_emoji_grapheme() {
    // Regional indicator pair 🇳🇴 (Norway flag) = one grapheme, two scalars.
    let mut test = EditorTest::new("🇳🇴!");
    test.press('x');
    assert_eq!(
        test.buffer_content(),
        "!\n",
        "x should delete the whole flag grapheme"
    );
}

#[test]
fn test_x_still_works_on_plain_ascii() {
    let mut test = EditorTest::new("abc");
    test.press('x');
    assert_eq!(test.buffer_content(), "bc\n");
    test.assert_cursor(0, 0);
}
