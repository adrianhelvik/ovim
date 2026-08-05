use crate::editor::Editor;

/// Shared post-input refresh used by terminal and API input paths.
pub fn refresh_after_input(editor: &mut Editor) {
    if editor.buffer().needs_rehighlight() {
        editor.process_viewport_rehighlight();
    }
    editor.mark_dirty();
}

/// Shared post-mutation refresh for API endpoints that bypass key dispatch.
pub fn refresh_after_api_mutation(editor: &mut Editor, force_full_lsp_sync: bool) {
    if force_full_lsp_sync {
        editor.mark_buffer_modified_force_send();
    }
    editor.request_diagnostics_refresh();
    refresh_after_input(editor);
}

/// Reload a clean buffer after an external write, but never discard local
/// edits. This is shared by focus events and periodic polling so headless
/// sessions have the same file-change behavior as the TUI.
pub fn process_external_file_change(editor: &mut Editor) {
    match editor.buffer().check_external_modification() {
        Ok(false) | Err(_) => {}
        Ok(true) if editor.is_modified() => {
            let status = "File changed on disk; local changes were kept (use :e! to reload)";
            if editor.status_message() != status {
                editor.set_status_message(status);
                editor.mark_dirty();
            }
        }
        Ok(true) => match editor.buffer_mut().reload_if_changed_sync() {
            Ok(true) => {
                editor.mark_saved();
                editor.mark_buffer_modified_force_send();
                editor.request_diagnostics_refresh();
                if editor.buffer().needs_rehighlight() {
                    editor.process_viewport_rehighlight();
                }
                editor.set_status_message("File reloaded after external change");
                editor.mark_dirty();
            }
            Ok(false) => {}
            Err(error) => {
                editor.set_status_message(format!("External file change: {error}"));
                editor.mark_dirty();
            }
        },
    }
}
