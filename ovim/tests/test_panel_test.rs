use ovim::editor::{Editor, InputHandler};
use ovim_core::{KeyCode, KeyEvent, Modifiers};

#[test]
fn escape_closes_the_test_panel() {
    let mut editor = Editor::with_content("fn main() {}\n");
    editor.toggle_test_panel();
    assert!(editor.is_test_panel_open());

    InputHandler::handle_key_event(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
        .unwrap();

    assert!(!editor.is_test_panel_open());
}

#[test]
fn escape_closes_the_test_panel_while_cancelling_a_pending_command() {
    let mut editor = Editor::with_content("fn main() {}\n");
    editor.toggle_test_panel();
    InputHandler::handle_key_event(
        &mut editor,
        KeyEvent::new(KeyCode::Char('g'), Modifiers::NONE),
    )
    .unwrap();

    InputHandler::handle_key_event(&mut editor, KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
        .unwrap();

    assert!(!editor.is_test_panel_open());
    assert!(editor.pending_command().is_none());
}
