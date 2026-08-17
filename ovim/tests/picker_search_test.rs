use std::path::PathBuf;

use ovim::editor::picker::{Picker, PickerResult};

// ============================================================================
// Helper
// ============================================================================

fn file_result(display: &str) -> PickerResult {
    PickerResult {
        display: display.to_string(),
        location: format!("/project/{}", display),
        line: 0,
        col: 0,
        match_positions: Vec::new(),
        content: None,
    }
}

/// Drives the nucleo background matcher until results stabilize.
/// FindFiles mode uses nucleo for async matching — we need to give
/// the background threads time to process injected items and queries.
fn tick_picker(picker: &mut Picker) {
    // Nucleo runs in background threads; under full test load, a fixed number
    // of ticks can be flaky. Instead, wait until the top result + match count
    // stabilizes for a few consecutive polls.
    let mut stable_polls = 0usize;
    let mut last_count = None;
    let mut last_top = None;

    for _ in 0..300 {
        let changed = picker.tick();
        let count = picker.filtered_result_count();
        let top = picker.filtered_result(0).map(|r| r.display.clone());

        if !changed && last_count == Some(count) && last_top == top {
            stable_polls += 1;
            if stable_polls >= 5 {
                break;
            }
        } else {
            stable_polls = 0;
        }

        last_count = Some(count);
        last_top = top;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn file_picker(files: &[&str]) -> Picker {
    let mut picker = Picker::new_file_finder(PathBuf::from("/project"), PathBuf::from("/project"));
    for f in files {
        picker.add_file_result(file_result(f));
    }
    picker.finish_loading();
    tick_picker(&mut picker);
    picker
}

/// Returns the top N results using the nucleo-compatible API.
fn top_results(picker: &Picker, n: usize) -> Vec<String> {
    let count = picker.filtered_result_count().min(n);
    (0..count)
        .filter_map(|i| picker.filtered_result(i))
        .map(|r| r.display.clone())
        .collect()
}

// ============================================================================
// Fuzzy scoring: exact substring preferred
// ============================================================================

#[test]
fn exact_substring_ranks_above_fuzzy() {
    let mut picker = file_picker(&[
        "src/logging.rs", // s...g scattered
        "src/msg.rs",     // "sg" contiguous in filename
    ]);

    picker.set_query("sg".to_string());
    tick_picker(&mut picker);
    assert_eq!(top_results(&picker, 1), vec!["src/msg.rs"]);
}

#[test]
fn exact_substring_at_word_boundary_preferred() {
    let mut picker = file_picker(&[
        "src/message.rs", // "msg" not present
        "src/xmsg.rs",    // "msg" inside word
        "src/a/msg.rs",   // "msg" at word boundary (after /)
    ]);

    picker.set_query("msg".to_string());
    tick_picker(&mut picker);
    let results = top_results(&picker, 2);
    // Word-boundary match should rank first
    assert_eq!(results[0], "src/a/msg.rs");
}

#[test]
fn exact_match_at_start_of_filename() {
    let mut picker = file_picker(&[
        "src/xmain.rs", // "main" not at start of filename
        "src/main.rs",  // "main" at start of filename
    ]);

    picker.set_query("main".to_string());
    tick_picker(&mut picker);
    assert_eq!(top_results(&picker, 1), vec!["src/main.rs"]);
}

// ============================================================================
// Fuzzy scoring: filename preferred over path
// ============================================================================

#[test]
fn filename_match_ranks_above_path_match() {
    let mut picker = file_picker(&[
        "picker/something.rs", // "picker" only in directory
        "src/picker.rs",       // "picker" in filename
    ]);

    picker.set_query("picker".to_string());
    tick_picker(&mut picker);
    assert_eq!(top_results(&picker, 1), vec!["src/picker.rs"]);
}

// ============================================================================
// Multi-token queries
// ============================================================================

#[test]
fn multi_token_all_must_match() {
    let mut picker = file_picker(&[
        "src/editor/mod.rs",
        "src/editor/picker.rs",
        "src/buffer/mod.rs",
    ]);

    picker.set_query("editor picker".to_string());
    tick_picker(&mut picker);
    let results = top_results(&picker, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "src/editor/picker.rs");
}

#[test]
fn multi_token_no_match_if_token_missing() {
    let mut picker = file_picker(&["src/editor/mod.rs", "src/buffer/mod.rs"]);

    picker.set_query("editor xyz".to_string());
    tick_picker(&mut picker);
    assert_eq!(picker.filtered_result_count(), 0);
}

// ============================================================================
// Case insensitivity
// ============================================================================

#[test]
fn case_insensitive_matching() {
    let mut picker = file_picker(&["src/MyComponent.tsx"]);

    picker.set_query("mycomponent".to_string());
    tick_picker(&mut picker);
    assert_eq!(top_results(&picker, 1), vec!["src/MyComponent.tsx"]);
}

#[test]
fn case_insensitive_uppercase_query() {
    let mut picker = file_picker(&["src/utils.rs"]);

    picker.set_query("UTILS".to_string());
    tick_picker(&mut picker);
    assert_eq!(top_results(&picker, 1), vec!["src/utils.rs"]);
}

// ============================================================================
// Empty / no-match queries
// ============================================================================

#[test]
fn empty_query_returns_all_results() {
    let files = &["a.rs", "b.rs", "c.rs"];
    let picker = file_picker(files);

    assert_eq!(picker.filtered_result_count(), 3);
}

#[test]
fn no_match_returns_empty() {
    let mut picker = file_picker(&["src/main.rs"]);

    picker.set_query("zzzzz".to_string());
    tick_picker(&mut picker);
    assert_eq!(picker.filtered_result_count(), 0);
}

// ============================================================================
// Match positions are populated (uses Custom mode which keeps the
// synchronous fuzzy scorer — nucleo handles positions internally)
// ============================================================================

#[test]
fn match_positions_populated_on_exact_substring() {
    let mut picker = Picker::new_custom(PathBuf::from("/project"), vec!["src/mod.rs".to_string()]);

    picker.set_query("mod".to_string());
    let result = &picker.filtered_results()[0];
    // "mod" starts at char index 4 in "src/mod.rs"
    assert_eq!(result.match_positions, vec![4, 5, 6]);
}

#[test]
fn match_positions_populated_on_fuzzy() {
    let mut picker = Picker::new_custom(PathBuf::from("/project"), vec!["abcdef".to_string()]);

    // "adf" — a at 0, d at 3, f at 5 (no contiguous substring → fuzzy)
    picker.set_query("adf".to_string());
    let results = picker.filtered_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].match_positions, vec![0, 3, 5]);
}

// ============================================================================
// Selection navigation
// ============================================================================

#[test]
fn move_down_and_up_wraps_selection() {
    // OV-00255: picker selection wraps around at both ends, matching the
    // inline completion popup. Pressing Down at the bottom jumps to the
    // top; pressing Up at the top jumps to the bottom.
    let mut picker = file_picker(&["a.rs", "b.rs", "c.rs"]);

    assert_eq!(picker.selected_index(), 0);
    picker.move_down();
    assert_eq!(picker.selected_index(), 1);
    picker.move_down();
    assert_eq!(picker.selected_index(), 2);
    // Wrap from last to first
    picker.move_down();
    assert_eq!(picker.selected_index(), 0);

    // Wrap from first to last
    picker.move_up();
    assert_eq!(picker.selected_index(), 2);
    picker.move_up();
    assert_eq!(picker.selected_index(), 1);
    picker.move_up();
    assert_eq!(picker.selected_index(), 0);
}

#[test]
fn move_n_clamps_does_not_wrap() {
    // Page-wise motion (move_down_n / move_up_n driven by Ctrl-D / Ctrl-U)
    // intentionally clamps rather than wrapping. Wrapping a half-page
    // jump from the bottom of the list back to the middle of the list
    // would be disorienting.
    let mut picker = file_picker(&["a.rs", "b.rs", "c.rs"]);

    picker.move_down_n(10);
    assert_eq!(picker.selected_index(), 2, "down_n clamps at end");
    picker.move_up_n(10);
    assert_eq!(picker.selected_index(), 0, "up_n clamps at start");
}

#[test]
fn set_query_resets_selection_to_zero() {
    let mut picker = file_picker(&["a.rs", "b.rs", "c.rs"]);

    picker.move_down();
    picker.move_down();
    assert_eq!(picker.selected_index(), 2);

    // Query "a" matches only "a.rs" (1 result), so selected_index
    // gets clamped from 2 to 0.
    picker.set_query("a".to_string());
    tick_picker(&mut picker);
    assert_eq!(picker.selected_index(), 0);
}

// ============================================================================
// Query editing
// ============================================================================

#[test]
fn insert_char_and_backspace() {
    let mut picker = file_picker(&["src/main.rs"]);

    picker.insert_char('m');
    picker.insert_char('a');
    assert_eq!(picker.query(), "ma");

    picker.backspace_query();
    assert_eq!(picker.query(), "m");
}

#[test]
fn cursor_movement_in_query() {
    let mut picker = file_picker(&[]);

    picker.insert_char('a');
    picker.insert_char('b');
    picker.insert_char('c');
    assert_eq!(picker.query_cursor(), 3);

    picker.move_cursor_left();
    assert_eq!(picker.query_cursor(), 2);

    // Insert in the middle
    picker.insert_char('x');
    assert_eq!(picker.query(), "abxc");

    picker.move_cursor_home();
    assert_eq!(picker.query_cursor(), 0);

    picker.move_cursor_end();
    assert_eq!(picker.query_cursor(), 4);
}

#[test]
fn delete_char_at_cursor() {
    let mut picker = file_picker(&[]);

    picker.insert_char('a');
    picker.insert_char('b');
    picker.insert_char('c');
    picker.move_cursor_home();

    picker.delete_char();
    assert_eq!(picker.query(), "bc");
}

// ============================================================================
// Debounced filtering (uses Custom mode — nucleo pushes queries immediately
// and never sets pending_filter)
// ============================================================================

#[test]
fn pending_filter_applied_on_demand() {
    let mut picker = Picker::new_custom(
        PathBuf::from("/project"),
        vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
    );

    // insert_char marks filter pending but doesn't apply it
    picker.insert_char('m');
    assert!(picker.has_pending_filter());

    // Results haven't been re-filtered yet (still shows all because
    // insert_char only marks pending, doesn't filter)
    // apply_pending_filter actually runs the filter
    picker.apply_pending_filter();
    assert!(!picker.has_pending_filter());
    assert_eq!(picker.filtered_results().len(), 1);
    assert_eq!(picker.filtered_results()[0].display, "src/main.rs");
}

// ============================================================================
// Incremental file loading
// ============================================================================

#[test]
fn add_file_result_during_loading() {
    let mut picker = Picker::new_file_finder(PathBuf::from("/project"), PathBuf::from("/project"));

    assert!(picker.is_loading());
    assert!(picker.should_spawn_file_loading());

    picker.mark_loading_spawned(1);
    assert!(!picker.should_spawn_file_loading());

    picker.add_file_result(file_result("a.rs"));
    picker.add_file_result(file_result("b.rs"));
    tick_picker(&mut picker);
    assert_eq!(picker.filtered_result_count(), 2);

    picker.finish_loading();
    assert!(!picker.is_loading());
}

#[test]
fn add_file_result_with_active_query_filters_incrementally() {
    let mut picker = Picker::new_file_finder(PathBuf::from("/project"), PathBuf::from("/project"));
    picker.set_query("main".to_string());

    picker.add_file_result(file_result("src/main.rs"));
    picker.add_file_result(file_result("src/lib.rs")); // doesn't match
    tick_picker(&mut picker);

    // Only matching result appears
    assert_eq!(picker.filtered_result_count(), 1);
    let first = picker.filtered_result(0).unwrap();
    assert_eq!(first.display, "src/main.rs");
}

// ============================================================================
// Shorter targets rank higher (specificity)
// ============================================================================

#[test]
fn shorter_filename_ranks_higher() {
    let mut picker = file_picker(&[
        "src/module_helpers.rs", // longer filename
        "src/mod.rs",            // shorter filename
    ]);

    picker.set_query("mod".to_string());
    tick_picker(&mut picker);
    // Shorter target gets higher length bonus
    assert_eq!(top_results(&picker, 1), vec!["src/mod.rs"]);
}

// ============================================================================
// Live grep
// ============================================================================

#[test]
fn live_grep_empty_query_returns_nothing() {
    let mut picker = Picker::new_live_grep(PathBuf::from("/tmp"), PathBuf::from("/tmp"));

    picker.set_query("".to_string());
    assert!(picker.filtered_results().is_empty());
}

#[test]
fn live_grep_searches_real_files() {
    use std::fs;

    let dir = tempfile::tempdir().expect("create temp dir");
    let dir_path = dir.path().to_path_buf();

    fs::write(
        dir_path.join("hello.txt"),
        "needle in haystack\nother line\n",
    )
    .unwrap();
    fs::write(dir_path.join("empty.txt"), "nothing here\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut picker = Picker::new_live_grep(dir_path.clone(), dir_path.clone());
        picker.set_query("needle".to_string());

        // Drain streaming results
        loop {
            picker.drain_grep_results();
            if !picker.is_loading() {
                picker.drain_grep_results();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let results = picker.filtered_results();
        assert_eq!(
            results.len(),
            1,
            "expected 1 grep result, got {}",
            results.len()
        );

        let r = &results[0];
        assert!(
            r.display.contains("hello.txt"),
            "display should contain filename: {}",
            r.display
        );
        assert!(
            r.display.contains("1"),
            "display should contain line number: {}",
            r.display
        );
        assert_eq!(r.line, 0, "line should be 0-indexed");
        assert_eq!(r.content.as_deref(), Some("needle in haystack"));

        // Location should be absolute path
        assert!(
            r.location.starts_with('/') || r.location.starts_with('\\'),
            "location should be absolute: {}",
            r.location
        );
    });
}

#[test]
fn live_grep_multiple_matches() {
    use std::fs;

    let dir = tempfile::tempdir().expect("create temp dir");
    let dir_path = dir.path().to_path_buf();

    fs::write(dir_path.join("a.txt"), "foo bar\nfoo baz\n").unwrap();
    fs::write(dir_path.join("b.txt"), "foo qux\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut picker = Picker::new_live_grep(dir_path.clone(), dir_path);
        picker.set_query("foo".to_string());

        loop {
            picker.drain_grep_results();
            if !picker.is_loading() {
                picker.drain_grep_results();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let results = picker.filtered_results();
        assert_eq!(results.len(), 3, "expected 3 grep matches across 2 files");
    });
}

#[test]
fn live_grep_no_match() {
    use std::fs;

    let dir = tempfile::tempdir().expect("create temp dir");
    fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut picker = Picker::new_live_grep(dir.path().to_path_buf(), dir.path().to_path_buf());
        picker.set_query("zzzznotfound".to_string());

        loop {
            picker.drain_grep_results();
            if !picker.is_loading() {
                picker.drain_grep_results();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        assert!(picker.filtered_results().is_empty());
    });
}

// ============================================================================
// Path truncation
// ============================================================================

#[test]
fn truncate_short_path_unchanged() {
    assert_eq!(Picker::truncate_path("src/main.rs", 40), "src/main.rs");
}

#[test]
fn truncate_long_path_uses_ellipsis() {
    let long = "src/deeply/nested/path/to/some/module/file.rs";
    let truncated = Picker::truncate_path(long, 30);
    assert!(
        truncated.len() <= 30,
        "truncated len {} > 30",
        truncated.len()
    );
    assert!(
        truncated.contains("..."),
        "should contain ellipsis: {}",
        truncated
    );
    assert!(
        truncated.contains("file.rs"),
        "should keep filename: {}",
        truncated
    );
}
