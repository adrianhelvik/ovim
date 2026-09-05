mod helpers;
use helpers::EditorTest;

#[test]
fn repeat_updates_the_deleted_text_register() {
    let mut test = EditorTest::new("one two three");
    test.keys("dw.");
    assert_eq!(test.editor.registers().get_default(), "two ");
    test.keys("u");
    assert_eq!(test.editor.registers().get_default(), "two ");
}

#[test]
fn repeat_remembers_the_named_delete_register() {
    let mut test = EditorTest::new("one two three");
    test.keys("\"adw.");
    assert_eq!(test.editor.registers().get(Some('a')), "two ");
}

#[test]
fn repeat_preserves_the_black_hole_register() {
    let mut test = EditorTest::new("one two three");
    test.keys("yiw\"_dw.");
    assert_eq!(test.editor.registers().get_default(), "one");
}

#[test]
fn repeat_pastes_from_the_original_named_register_after_a_yank() {
    let mut test = EditorTest::new("one two three");
    test.keys("\"ayiw$\"ap0yy$.");
    assert_eq!(test.buffer_content(), "one two threeoneone\n");
}

#[test]
fn repeated_change_stores_the_new_target_and_linewise_type() {
    let mut test = EditorTest::new("  one\n    two\nthree");
    test.keys("ccX<Esc>j.");
    assert_eq!(test.editor.registers().get_default(), "    two\n");
    assert_eq!(
        test.editor.registers().get_default_with_type().1,
        ovim::editor::RegisterType::Line
    );
}

#[test]
fn repeat_does_not_overwrite_registers_for_case_changes() {
    let mut test = EditorTest::new("one two");
    test.keys("yiwgUiw w.");
    assert_eq!(test.editor.registers().get_default(), "one");
}
