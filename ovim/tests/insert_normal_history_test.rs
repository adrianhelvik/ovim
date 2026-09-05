mod helpers;
use helpers::EditorTest;

#[test]
fn insert_normal_command_undo_restores_the_complete_edit() {
    let mut test = EditorTest::new("one two three");
    test.keys("iX<C-o>dwY<Esc>");
    let edited = test.buffer_content();
    assert_eq!(edited, "XYtwo three\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "Xtwo three\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "Xone two three\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "one two three\n");
    test.keys("3<C-r>");
    assert_eq!(test.buffer_content(), edited);
}

#[test]
fn insert_normal_waits_for_a_counted_motion() {
    let mut test = EditorTest::new("one two three");
    test.keys("iX<C-o>2wY<Esc>");
    assert_eq!(test.buffer_content(), "Xone two Ythree\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "Xone two three\n");
}

#[test]
fn insert_normal_undo_closes_the_recording_before_reversing_edits() {
    let mut test = EditorTest::new("abc");
    test.keys("iX<C-o>uY<Esc>");
    assert_eq!(test.buffer_content(), "Yabc\n");
    test.keys("u");
    assert_eq!(test.buffer_content(), "abc\n");
}

#[test]
fn insert_normal_change_opens_its_own_session() {
    let mut test = EditorTest::new("one two");
    test.keys("iX<C-o>cwY<Esc>x");
    assert_eq!(test.buffer_content(), "X two\n");
    test.keys("3u");
    assert_eq!(test.buffer_content(), "one two\n");
}
