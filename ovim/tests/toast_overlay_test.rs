use ovim::editor::{Editor, InputHandler, ToastLevel, ToastRequest, ToastSource};
use ovim_core::{KeyCode, KeyEvent, Modifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, Modifiers::NONE)
}

fn press(editor: &mut Editor, code: KeyCode) {
    InputHandler::handle_key_event(editor, key(code)).unwrap();
}

#[test]
fn test_double_escape_dismisses_top_right_toast_overlay() {
    let mut editor = Editor::with_content("fn main() {}\n");
    editor.push_toast(
        ToastRequest::new(
            ToastSource::System,
            ToastLevel::Warning,
            "Something happened",
        )
        .with_sticky(true)
        .with_dedupe_key("system:warn"),
    );

    assert!(editor.has_top_right_overlay());
    assert!(editor.has_visible_toasts());

    press(&mut editor, KeyCode::Esc);
    assert!(editor.has_visible_toasts());

    press(&mut editor, KeyCode::Esc);
    assert!(!editor.has_visible_toasts());
    assert!(!editor.has_top_right_overlay());
}

#[test]
fn test_lsp_status_error_emits_deduped_toast() {
    let mut editor = Editor::with_content("fn main() {}\n");

    editor.set_lsp_status("Completion failed: No server for language: markdown".to_string());
    editor.set_lsp_status("Completion failed: No server for language: markdown".to_string());

    let toasts = editor.visible_toasts_newest_first(10);
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].source, ToastSource::Lsp);
    assert_eq!(toasts[0].level, ToastLevel::Error);
    assert_eq!(toasts[0].repeat, 2);
    assert!(!toasts[0].sticky, "routine LSP failures must auto-dismiss");
    assert_eq!(toasts[0].ttl, Some(std::time::Duration::from_secs(8)));
}

#[test]
fn generic_status_message_does_not_impersonate_an_lsp_notification() {
    let mut editor = Editor::with_content("fn main() {}\n");

    editor.set_status_message("Delete failed: permission denied");

    assert_eq!(editor.status_message(), "Delete failed: permission denied");
    assert!(!editor.has_visible_toasts());
}

#[test]
fn generic_feedback_does_not_overwrite_the_lsp_subsystem_status() {
    let mut editor = Editor::with_content("fn main() {}\n");
    editor.set_lsp_status("LSP: rust-analyzer ready".to_owned());

    editor.set_status_message("Saved file");

    assert_eq!(editor.status_message(), "Saved file");
    assert_eq!(editor.lsp_status(), "LSP: rust-analyzer ready");
}

#[test]
fn test_tick_toasts_prunes_expired_entries() {
    let mut editor = Editor::with_content("fn main() {}\n");
    editor.push_toast(
        ToastRequest::new(ToastSource::System, ToastLevel::Info, "Ephemeral")
            .with_ttl(Some(std::time::Duration::ZERO))
            .with_sticky(false),
    );

    assert!(editor.tick_toasts());
    assert!(!editor.tick_toasts());
}
