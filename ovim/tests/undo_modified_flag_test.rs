//! OV-00340 / OV-00341: the modified flag must track the undo save point.
//!
//! Semantics derived from actual vim behavior, verified in `nvim --clean`
//! (see notes/TESTING_VIM_SEMANTICS.md):
//! - edit, `u`                          -> &modified == 0  (undo to save point)
//! - edit, `:w`, edit, `u`              -> &modified == 0
//! - edit, `:w`, `u`                    -> &modified == 1  (below save point)
//! - edit, `:w`, `u`, `<C-r>`           -> &modified == 0  (redo to save point)
//! - edit, `:w`, `u`, different edit    -> &modified == 1  (same undo depth,
//!   different state — the save point is a state identity, not a stack depth)
//!
//! The user-visible failure this pins down: undoing every change still left
//! the buffer "modified", so `:q` refused and `:q!` was required.

mod helpers;
use helpers::EditorTest;

#[test]
fn undo_back_to_start_clears_modified() {
    let mut test = EditorTest::new("hello\n");
    assert!(!test.editor.is_modified(), "fresh buffer starts unmodified");

    test.keys("ccedit1<Esc>");
    assert!(test.editor.is_modified(), "edit marks the buffer modified");

    test.keys("u");
    assert!(
        !test.editor.is_modified(),
        "undoing the only change returns to the saved state; :q must work without ! (OV-00340)"
    );
}

#[test]
fn undo_back_to_save_point_clears_modified() {
    let mut test = EditorTest::new("hello\n");
    test.keys("ccedit1<Esc>");
    test.editor.mark_saved(); // :w
    assert!(!test.editor.is_modified());

    test.keys("ccedit2<Esc>");
    assert!(test.editor.is_modified());

    test.keys("u");
    assert!(
        !test.editor.is_modified(),
        "undo back to the :w state is unmodified (OV-00340)"
    );
}

#[test]
fn undo_below_save_point_marks_modified_and_redo_clears() {
    let mut test = EditorTest::new("hello\n");
    test.keys("ccedit1<Esc>");
    test.editor.mark_saved(); // :w

    test.keys("u");
    assert!(
        test.editor.is_modified(),
        "below the save point the buffer differs from disk"
    );

    test.keys("<C-r>");
    assert!(
        !test.editor.is_modified(),
        "redo back up to the save point is unmodified again"
    );
}

#[test]
fn divergent_edit_at_same_undo_depth_stays_modified() {
    let mut test = EditorTest::new("hello\n");
    test.keys("ccedit1<Esc>");
    test.editor.mark_saved(); // :w

    // Undo the saved change, then make a DIFFERENT change: the undo stack
    // is back at the same depth as when we saved, but the content differs
    // from disk. A depth-based save point would falsely report unmodified
    // here — and :q would silently drop the change (OV-00341).
    test.keys("u");
    test.keys("ccother<Esc>");
    assert!(
        test.editor.is_modified(),
        "different change at the same undo depth must stay modified (OV-00341)"
    );
}
