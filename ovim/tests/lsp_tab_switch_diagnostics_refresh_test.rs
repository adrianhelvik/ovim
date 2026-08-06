mod helpers;
use helpers::EditorTest;

#[test]
fn tab_switch_requests_diagnostics_refresh_for_new_current_file() {
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("a.rs");
    let file2 = dir.path().join("b.rs");
    std::fs::write(&file1, "fn a() {}\n").unwrap();
    std::fs::write(&file2, "fn b() {}\n").unwrap();

    let file1 = std::fs::canonicalize(&file1)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let file2 = std::fs::canonicalize(&file2)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let mut t = EditorTest::new("fn a() {}\n");
    t.set_file_path(file1.clone());

    t.editor.new_tab();
    t.set_file_path(file2.clone());

    // Ensure the flag is not already set from setup.
    let _ = t.editor.take_diagnostics_refresh_request();

    t.editor.previous_tab();

    assert!(
        t.editor.take_diagnostics_refresh_request(),
        "Expected tab switching to request diagnostics refresh"
    );
    assert_eq!(
        t.editor.needs_lsp_init(),
        Some(file1),
        "Expected tab switching to request LSP init for the current file"
    );
}

#[test]
fn tab_switch_next_tab_requests_diagnostics_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("a.rs");
    let file2 = dir.path().join("b.rs");
    std::fs::write(&file1, "fn a() {}\n").unwrap();
    std::fs::write(&file2, "fn b() {}\n").unwrap();

    let file1 = std::fs::canonicalize(&file1)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let file2 = std::fs::canonicalize(&file2)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let mut t = EditorTest::new("fn a() {}\n");
    t.set_file_path(file1);
    t.editor.new_tab();
    t.set_file_path(file2);

    // Switch back to tab 1, then clear the refresh flag.
    t.editor.previous_tab();
    let _ = t.editor.take_diagnostics_refresh_request();

    t.editor.next_tab();

    assert!(
        t.editor.take_diagnostics_refresh_request(),
        "Expected tab switching to request diagnostics refresh"
    );
}
