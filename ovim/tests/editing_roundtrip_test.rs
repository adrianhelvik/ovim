mod helpers;
use helpers::EditorTest;

#[test]
fn editing_commands_repeat_and_roundtrip() {
    let contents = [
        "one two three\nfour five six\nseven eight nine\n",
        "  one two\n    three four\n  five six\n",
        "a\nb\nc\n",
        "one (two) three\nfour (five) six\n",
        "áb 👩‍💻 café\none two three\n",
    ];
    let setups = ["", "w", "$", "j0", "G$"];
    let commands = [
        "x",
        "X",
        "dd",
        "dw",
        "db",
        "de",
        "dW",
        "dB",
        "dE",
        "dh",
        "dl",
        "d0",
        "d^",
        "d$",
        "dj",
        "dk",
        "d%",
        "diw",
        "daw",
        "cwX<Esc>",
        "cWX<Esc>",
        "ceX<Esc>",
        "cEX<Esc>",
        "cbX<Esc>",
        "cBX<Esc>",
        "chX<Esc>",
        "clX<Esc>",
        "c0X<Esc>",
        "c^X<Esc>",
        "c$X<Esc>",
        "c%X<Esc>",
        "ccX<Esc>",
        "cjX<Esc>",
        "ckX<Esc>",
        "ciwX<Esc>",
        "cawX<Esc>",
        "sX<Esc>",
        "CX<Esc>",
        "SX<Esc>",
        "iX<Esc>",
        "aX<Esc>",
        "IX<Esc>",
        "AX<Esc>",
        "oX<Esc>",
        "OX<Esc>",
        "rX",
        "RX<Esc>",
        "~",
        "J",
        ">>",
        "<<",
        "cwab<BS>X<Esc>",
        "ccab<BS>X<Esc>",
        "oab<BS>X<Esc>",
        "iab<BS>X<Esc>",
        "cw<Esc>",
        "cc<Esc>",
        "o<Esc>",
        "vld",
        "vlcX<Esc>",
        "Vd",
        "VcX<Esc>",
    ];
    let mut failures = Vec::new();
    for content in contents {
        for setup in setups {
            for command in commands {
                let mut test = EditorTest::new(content);
                test.keys(setup);
                let before = test.buffer_content();
                let cursor_before = test.editor.cursor_position();
                test.keys(command);
                let after = test.buffer_content();
                let cursor_after = test.editor.cursor_position();
                if before == after {
                    continue;
                }
                test.keys("u");
                if test.buffer_content() != before {
                    failures.push(format!(
                        "undo {content:?} {setup:?} {command:?}: {:?}",
                        test.buffer_content()
                    ));
                    continue;
                }
                test.keys("<C-r>");
                if test.buffer_content() != after {
                    failures.push(format!(
                        "redo {content:?} {setup:?} {command:?}: {:?}",
                        test.buffer_content()
                    ));
                    continue;
                }
                test.keys("u");
                test.editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(cursor_before.line, cursor_before.col);
                test.keys(".");
                if test.buffer_content() != after || test.editor.cursor_position() != cursor_after {
                    failures.push(format!("repeat {content:?} {setup:?} {command:?}: expected {after:?} {cursor_after:?}, got {:?} {:?}", test.buffer_content(), test.editor.cursor_position()));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn counted_undo_redo_and_exhausted_history() {
    let mut test = EditorTest::new("abcdef");
    test.keys("xxx2u");
    assert_eq!(test.buffer_content(), "bcdef\n");
    test.keys("2<C-r>");
    assert_eq!(test.buffer_content(), "def\n");
    test.keys("99u");
    assert_eq!(test.buffer_content(), "abcdef\n");
    test.keys("99<C-r>");
    assert_eq!(test.buffer_content(), "def\n");
}

#[test]
fn repeat_uses_corrected_text_and_target_indentation() {
    for (before, keys, after) in [
        ("one two three", "cwab<BS>X<Esc>w.", "aX aX three\n"),
        ("  one\n    two", "ccX<Esc>j.", "  X\n    X\n"),
        ("one\ntwo\nthree", "GccX<Esc>uG.", "one\ntwo\nX\n"),
        ("one two\nthree four", "wcbX<Esc>j$.", "Xtwo\nthree Xr\n"),
        ("abc", "$sX<Esc>", "abX\n"),
        ("abc", "iX<Esc>3.", "XXXXabc\n"),
    ] {
        let mut test = EditorTest::new(before);
        test.keys(keys);
        assert_eq!(test.buffer_content(), after, "{keys}");
    }
}

#[test]
fn word_and_change_commands_preserve_graphemes() {
    for (keys, expected) in [
        ("cwX<Esc>", "X café\n"),
        ("dw", "café\n"),
        ("clX<Esc>", "Xb café\n"),
        ("wC!<Esc>", "áb !\n"),
        ("wc$!<Esc>", "áb !\n"),
        ("A!<Esc>", "áb café!\n"),
    ] {
        let mut test = EditorTest::new("áb café");
        test.keys(keys);
        assert_eq!(test.buffer_content(), expected, "{keys}");
        test.keys("u");
        assert_eq!(test.buffer_content(), "áb café\n", "undo {keys}");
    }
    let mut test = EditorTest::new("ab á");
    test.keys("de");
    test.keys("wde");
    assert_eq!(test.buffer_content(), " \n");
}

#[test]
fn counted_repeat_is_one_undo_unit_and_updates_the_template() {
    let mut test = EditorTest::new("one two three four five six seven eight nine ten");
    test.keys("2dw3.");
    assert_eq!(test.buffer_content(), "six seven eight nine ten\n");
    test.keys("u");
    assert_eq!(
        test.buffer_content(),
        "three four five six seven eight nine ten\n"
    );
    test.keys("<C-r>.");
    assert_eq!(test.buffer_content(), "nine ten\n");
}
