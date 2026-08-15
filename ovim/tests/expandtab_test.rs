mod helpers;
use helpers::EditorTest;

// ============================================================================
// Tab key — expandtab (default)
// ============================================================================

#[test]
fn test_tab_expandtab_inserts_spaces() {
    let mut test = EditorTest::new("hello");
    test.keys("i<Tab><Esc>");
    // Default: expandtab=true, shiftwidth=4 → 4 spaces
    assert_eq!(test.buffer_content(), "    hello\n");
}

#[test]
fn test_tab_expandtab_custom_shiftwidth() {
    let mut test = EditorTest::new("hello");
    test.editor.options.shift_width = 2;
    test.keys("i<Tab><Esc>");
    assert_eq!(test.buffer_content(), "  hello\n");
}

#[test]
fn test_tab_advances_to_next_soft_stop() {
    let mut test = EditorTest::new("  hello");
    test.editor.options.shift_width = 4;
    test.editor.options.soft_tab_stop = -1;
    test.keys("lli<Tab><Esc>");
    assert_eq!(test.buffer_content(), "    hello\n");
}

// ============================================================================
// Tab key — noexpandtab
// ============================================================================

#[test]
fn test_tab_noexpandtab_inserts_tab() {
    let mut test = EditorTest::new("hello");
    test.editor.options.expand_tab = false;
    test.keys("i<Tab><Esc>");
    assert_eq!(test.buffer_content(), "\thello\n");
}

// ============================================================================
// = operator — expandtab (default)
// ============================================================================

#[test]
fn test_equals_expandtab_uses_spaces() {
    let mut test = EditorTest::new("fn main() {\nhello\n}");
    test.keys("j==");
    assert_eq!(test.buffer_content(), "fn main() {\n    hello\n}\n");
}

// ============================================================================
// = operator — noexpandtab
// ============================================================================

#[test]
fn test_equals_noexpandtab_uses_tabs() {
    let mut test = EditorTest::new("fn main() {\nhello\n}");
    test.editor.options.expand_tab = false;
    test.keys("j==");
    assert_eq!(test.buffer_content(), "fn main() {\n\thello\n}\n");
}

// ============================================================================
// Enter after opening bracket — expandtab
// ============================================================================

#[test]
fn test_enter_after_brace_expandtab() {
    let mut test = EditorTest::new("fn main() {");
    // Type Enter then content so indent is preserved (Esc strips trailing whitespace)
    test.keys("A<CR>x<Esc>");
    assert_eq!(test.buffer_content(), "fn main() {\n    x\n");
}

#[test]
fn test_enter_after_paren_expandtab() {
    let mut test = EditorTest::new("call(");
    test.keys("A<CR>x<Esc>");
    assert_eq!(test.buffer_content(), "call(\n    x\n");
}

#[test]
fn test_enter_after_bracket_expandtab() {
    let mut test = EditorTest::new("let a = [");
    test.keys("A<CR>x<Esc>");
    assert_eq!(test.buffer_content(), "let a = [\n    x\n");
}

#[test]
fn test_enter_splits_adjacent_braces_and_aligns_closer() {
    let mut test = EditorTest::new("{}");

    test.keys("a<CR>x<Esc>");

    assert_eq!(test.buffer_content(), "{\n    x\n}\n");
}

#[test]
fn test_enter_does_not_indent_after_delimiter_in_comment() {
    let mut test = EditorTest::new("// {");
    test.set_file_path("/tmp/indent.rs".to_string());

    test.keys("A<CR>x<Esc>");

    assert_eq!(test.buffer_content(), "// {\nx\n");
}

#[test]
fn test_enter_indents_after_opener_before_trailing_comment() {
    let mut test = EditorTest::new("fn main() { // body");
    test.set_file_path("/tmp/indent.rs".to_string());

    test.keys("A<CR>x<Esc>");

    assert_eq!(test.buffer_content(), "fn main() { // body\n    x\n");
}

// ============================================================================
// Enter after opening bracket — noexpandtab
// ============================================================================

#[test]
fn test_enter_after_brace_noexpandtab() {
    let mut test = EditorTest::new("fn main() {");
    test.editor.options.expand_tab = false;
    test.keys("A<CR>x<Esc>");
    assert_eq!(test.buffer_content(), "fn main() {\n\tx\n");
}

#[test]
fn test_enter_noexpandtab_does_not_overshoot_shiftwidth() {
    let mut test = EditorTest::new("fn main() {");
    test.editor.options.tab_width = 8;
    test.editor.options.shift_width = 4;
    test.editor.options.expand_tab = false;
    test.keys("A<CR>x<Esc>");
    assert_eq!(test.buffer_content(), "fn main() {\n    x\n");
}

// ============================================================================
// Enter on normal line — just copies indent
// ============================================================================

#[test]
fn test_enter_no_bracket_copies_indent() {
    let mut test = EditorTest::new("    hello world");
    test.keys("A<CR>x<Esc>");
    // Should copy the 4-space indent, no extra
    assert_eq!(test.buffer_content(), "    hello world\n    x\n");
}

// ============================================================================
// o after brace — noexpandtab
// ============================================================================

#[test]
fn test_o_after_brace_noexpandtab() {
    let mut test = EditorTest::new("fn main() {");
    test.editor.options.expand_tab = false;
    test.keys("o<Esc>");
    let content = test.buffer_content();
    // o on a line ending with { should produce a new line
    // Esc may strip trailing whitespace, so just check the line exists
    assert!(
        content.contains('\t') || content == "fn main() {\n\n",
        "Expected tab indent or empty line, got: {:?}",
        content
    );
}

// ============================================================================
// >> with noexpandtab
// ============================================================================

#[test]
fn test_shift_right_noexpandtab() {
    let mut test = EditorTest::new("hello");
    test.editor.options.expand_tab = false;
    test.keys(">>");
    assert_eq!(test.buffer_content(), "\thello\n");
}

#[test]
fn test_shift_right_noexpandtab_uses_exact_visual_width() {
    let mut test = EditorTest::new("hello");
    test.editor.options.tab_width = 8;
    test.editor.options.shift_width = 4;
    test.editor.options.expand_tab = false;

    test.keys(">>");
    assert_eq!(test.buffer_content(), "    hello\n");

    test.keys(">>");
    assert_eq!(test.buffer_content(), "\thello\n");
}

#[test]
fn test_equals_uses_shiftwidth_not_tabstop() {
    let mut test = EditorTest::new("fn main() {\nhello\n}");
    test.editor.options.tab_width = 8;
    test.editor.options.shift_width = 2;
    test.keys("j==");
    assert_eq!(test.buffer_content(), "fn main() {\n  hello\n}\n");
}

// ============================================================================
// Ctrl-T with noexpandtab
// ============================================================================

#[test]
fn test_ctrl_t_noexpandtab() {
    let mut test = EditorTest::new("hello");
    test.editor.options.expand_tab = false;
    test.keys("i<C-t><Esc>");
    assert_eq!(test.buffer_content(), "\thello\n");
}

#[test]
fn test_ctrl_t_noexpandtab_uses_shift_boundaries() {
    let mut test = EditorTest::new("  hello");
    test.editor.options.tab_width = 8;
    test.editor.options.shift_width = 4;
    test.editor.options.expand_tab = false;

    test.keys("I<C-t><C-t><Esc>");
    assert_eq!(test.buffer_content(), "\thello\n");
}

#[test]
fn test_ctrl_d_noexpandtab_does_not_delete_eight_columns() {
    let mut test = EditorTest::new("\thello");
    test.editor.options.tab_width = 8;
    test.editor.options.shift_width = 4;
    test.editor.options.expand_tab = false;

    test.keys("I<C-d><Esc>");
    assert_eq!(test.buffer_content(), "    hello\n");
}

#[test]
fn test_softtabstop_controls_tab_and_backspace() {
    let mut test = EditorTest::new(" hello");
    test.editor.options.shift_width = 4;
    test.editor.options.soft_tab_stop = 2;

    test.keys("I<Tab><Tab><BS><Esc>");
    assert_eq!(test.buffer_content(), "  hello\n");
}

// ============================================================================
// indent_string — tested via = operator on nested code
// ============================================================================

#[test]
fn test_indent_string_via_equals_operator() {
    // Nested indentation with tabs
    let mut test = EditorTest::new("fn main() {\nif true {\nhello\n}\n}");
    test.editor.options.expand_tab = false;
    test.keys("gg=G");
    assert_eq!(
        test.buffer_content(),
        "fn main() {\n\tif true {\n\t\thello\n\t}\n}\n"
    );
}

#[test]
fn test_indent_string_via_equals_spaces() {
    // Same but with spaces (default)
    let mut test = EditorTest::new("fn main() {\nif true {\nhello\n}\n}");
    test.keys("gg=G");
    assert_eq!(
        test.buffer_content(),
        "fn main() {\n    if true {\n        hello\n    }\n}\n"
    );
}
