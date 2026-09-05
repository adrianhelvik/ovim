mod helpers;
use helpers::EditorTest;
use ovim::mode::Mode;

#[test]
fn missing_repeat_targets_cancel_the_whole_change() {
    for (content, keys) in [
        ("one:two\nplain", "cf:X<Esc>j0."),
        ("(one)\nplain", "ci(X<Esc>j0."),
        ("(one)\nplain", "c%X<Esc>j0."),
    ] {
        let mut test = EditorTest::new(content);
        let (command, _) = keys.rsplit_once('.').unwrap();
        test.keys(command);
        let before = test.buffer_content();
        let undo_depth = test.editor.buffer().change_manager().undo_stack.len();
        test.keys(".");
        assert_eq!(test.buffer_content(), before, "{keys}");
        assert_eq!(
            test.editor.buffer().change_manager().undo_stack.len(),
            undo_depth
        );
    }
}

#[test]
fn missing_matching_bracket_does_not_enter_insert_mode() {
    let mut test = EditorTest::new("plain");
    test.keys("c%");
    assert_eq!(test.editor.mode(), Mode::Normal);
    assert_eq!(test.buffer_content(), "plain\n");
}

#[test]
fn empty_text_objects_still_accept_repeat_insertion() {
    let mut test = EditorTest::new("(one) ()");
    test.keys("ci(X<Esc>f(.");
    assert_eq!(test.buffer_content(), "(X) (X)\n");
}

#[test]
fn repeat_character_find_deletes_whole_target_graphemes() {
    let mut test = EditorTest::new("xab xab́");
    test.keys("cfbX<Esc>w.");
    assert_eq!(test.buffer_content(), "X X\n");
    test.keys("2u");
    assert_eq!(test.buffer_content(), "xab xab́\n");
}

#[test]
fn change_inside_empty_delimiters_is_an_undoable_insertion() {
    for (before, keys, after) in [
        ("()", "ci(X<Esc>", "(X)\n"),
        ("\"\"", "ci\"X<Esc>", "\"X\"\n"),
    ] {
        let mut test = EditorTest::new(before);
        test.keys(keys);
        assert_eq!(test.buffer_content(), after);
        test.keys("u");
        assert_eq!(test.buffer_content(), format!("{before}\n"));
        test.keys("<C-r>");
        assert_eq!(test.buffer_content(), after);
    }
}
