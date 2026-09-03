//! Branch diff review (`<Space>gd`): open, navigate hunks, jump into files,
//! resume, refresh and close.

mod helpers;

use git2::{IndexAddOption, Oid, Repository, Signature};
use helpers::EditorTest;
use ovim_core::KeyCode;
use std::fs;
use std::path::Path;

fn commit_all(repo: &Repository, message: &str) -> Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Ovim", "ovim@example.com").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap()
}

/// A repo with `main` (a.txt = one/two/three) and a checked-out `feature`
/// branch that committed a change to a.txt and has an untracked b.txt.
struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = Repository::init(&root).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        commit_all(&repo, "c1");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        fs::write(root.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        commit_all(&repo, "edit a");
        fs::write(root.join("b.txt"), "new file\n").unwrap();
        Self { _dir: dir, root }
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).to_string_lossy().to_string()
    }
}

fn open_editor_on(fixture: &Fixture, name: &str) -> EditorTest {
    let mut test = EditorTest::new("");
    test.editor
        .open_file(Path::new(&fixture.path(name)))
        .unwrap();
    test
}

fn current_line(test: &EditorTest) -> String {
    let line = test.editor.buffer().cursor().line();
    test.editor
        .buffer()
        .line_text(line)
        .map(|text| text.to_string())
        .unwrap_or_default()
}

fn line_index_of(test: &EditorTest, needle: &str) -> usize {
    let buffer = test.editor.buffer();
    (0..buffer.line_count())
        .find(|&index| buffer.line_text(index).as_deref() == Some(needle))
        .unwrap_or_else(|| panic!("no line {needle:?} in review"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn leader_gd_opens_a_highlighted_review_in_a_new_tab() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    assert_eq!(test.editor.tab_count(), 1);

    test.keys(" gd");

    assert!(test.editor.is_diff_review_buffer());
    assert_eq!(test.editor.tab_count(), 2);
    assert!(test.editor.buffer().is_read_only());
    assert_eq!(
        test.editor.buffer().display_name(),
        Some("Diff · feature → main")
    );
    assert_eq!(current_line(&test), "feature → main");

    let text = test.editor.buffer().rope().to_string();
    assert!(text.contains("2 files · +3 −1"), "{text}");
    assert!(text.contains("  M  a.txt  +2 −1"), "{text}");
    assert!(text.contains("  A  b.txt  +1"), "{text}");
    assert!(text.contains("@@ -1,3 +1,4 @@"), "{text}");
    assert!(text.contains("\n+new file\n"), "{text}");
    assert!(text.contains("1 commit ahead"), "{text}");

    // The pathless buffer still gets the diff grammar.
    assert!(
        test.editor.buffer().has_syntax_highlighting(),
        "review buffer should be highlighted as a diff"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bracket_c_walks_hunks_and_enter_opens_the_source_line() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    test.keys(" gd");

    test.keys("]c");
    assert_eq!(current_line(&test), "@@ -1,3 +1,4 @@");
    test.keys("]c");
    assert_eq!(current_line(&test), "@@ -0,0 +1 @@");
    test.keys("]c");
    assert_eq!(
        current_line(&test),
        "@@ -0,0 +1 @@",
        "stays on the last hunk"
    );
    assert_eq!(test.editor.status_message(), "Last hunk");
    test.keys("[c");
    assert_eq!(current_line(&test), "@@ -1,3 +1,4 @@");

    // Land on the added `+four` line, column 3 → column 2 in the file.
    let four = line_index_of(&test, "+four");
    test.set_cursor(four, 3);
    test.press_key(KeyCode::Enter);

    assert!(!test.editor.is_diff_review_buffer());
    assert_eq!(
        test.editor.tab_count(),
        2,
        "file opens in the originating tab"
    );
    assert_eq!(test.editor.current_tab_index(), 0);
    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.txt"));
    let cursor = test.editor.buffer().cursor();
    assert_eq!((cursor.line(), cursor.col().0), (3, 2));
    assert_eq!(current_line(&test), "four");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enter_on_a_removed_line_lands_where_the_removal_happened() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    test.keys(" gd");

    let removed = line_index_of(&test, "-two");
    test.set_cursor(removed, 0);
    test.press_key(KeyCode::Enter);

    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.txt"));
    assert_eq!(test.editor.buffer().cursor().line(), 1);
    assert_eq!(current_line(&test), "2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enter_on_a_file_row_jumps_to_that_file_and_opens_new_files() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    test.keys(" gd");

    let row = line_index_of(&test, "  A  b.txt  +1");
    test.set_cursor(row, 0);
    test.press_key(KeyCode::Enter);
    assert!(test.editor.is_diff_review_buffer());
    assert_eq!(current_line(&test), "diff --git a/b.txt b/b.txt");

    // Enter on a file header opens the file at its first hunk.
    test.press_key(KeyCode::Enter);
    assert!(test.editor.buffer().file_path().unwrap().ends_with("b.txt"));
    assert_eq!(test.editor.buffer().cursor().line(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn leader_gd_resumes_the_review_at_the_same_hunk_after_editing() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    test.keys(" gd");

    let four = line_index_of(&test, "+four");
    test.set_cursor(four, 0);
    test.press_key(KeyCode::Enter);
    assert_eq!(current_line(&test), "four");

    // Edit the file on disk (as if saved) so the refreshed review differs.
    fs::write(fixture.path("a.txt"), "zero\none\n2\nthree\nfour\n").unwrap();

    test.keys(" gd");
    assert!(test.editor.is_diff_review_buffer());
    assert_eq!(test.editor.current_tab_index(), 1);
    assert_eq!(
        current_line(&test),
        "+four",
        "cursor follows the source line through the refresh"
    );
    let text = test.editor.buffer().rope().to_string();
    assert!(
        text.contains("+zero"),
        "review picked up the new change: {text}"
    );

    // Leaving the review returns to the file tab.
    test.keys(" gd");
    assert!(!test.editor.is_diff_review_buffer());
    assert_eq!(test.editor.current_tab_index(), 0);
    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn q_closes_the_review_and_drops_its_buffer() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    let buffers_before = test.editor.buffer_count();
    test.keys(" gd");
    assert_eq!(test.editor.buffer_count(), buffers_before + 1);

    test.keys("q");

    assert!(test.editor.diff_review().is_none());
    assert_eq!(test.editor.tab_count(), 1);
    assert_eq!(test.editor.buffer_count(), buffers_before);
    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn gitdiff_command_accepts_an_explicit_base() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");

    let result = ovim_core::commands::execute_command(&mut test.editor, "GitDiff HEAD");
    assert!(
        matches!(result, ovim_core::CommandResult::Success(_)),
        "{result:?}"
    );

    assert!(test.editor.is_diff_review_buffer());
    let text = test.editor.buffer().rope().to_string();
    assert!(text.starts_with("feature → HEAD\n"), "{text}");
    assert!(
        text.contains("+new file"),
        "uncommitted b.txt is included: {text}"
    );
    assert!(
        !text.contains("+four"),
        "committed work is excluded: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn on_the_default_branch_the_review_shows_uncommitted_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let repo = Repository::init(&root).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    fs::write(root.join("a.txt"), "one\n").unwrap();
    commit_all(&repo, "c1");
    fs::write(root.join("a.txt"), "one\nuncommitted\n").unwrap();

    let mut test = EditorTest::new("");
    test.editor.open_file(root.join("a.txt").as_path()).unwrap();
    test.keys(" gd");

    let text = test.editor.buffer().rope().to_string();
    assert!(text.starts_with("main → main\n"), "{text}");
    assert!(
        text.contains("On main: uncommitted changes against HEAD"),
        "{text}"
    );
    assert!(text.contains("+uncommitted"), "{text}");
}

/// A repo whose feature branch rewrites one line of a Rust file and adds a
/// tab-indented line, so the split layout has to align both sides.
struct RustFixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl RustFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = Repository::init(&root).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        fs::write(
            root.join("a.rs"),
            "fn main() {\n    let x = 1;\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        commit_all(&repo, "c1");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        fs::write(
            root.join("a.rs"),
            "fn main() {\n    let x = 42;\n\tlet y = \"two\";\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        commit_all(&repo, "edit a");
        Self { _dir: dir, root }
    }

    fn open(&self) -> EditorTest {
        let mut test = EditorTest::new("");
        test.editor
            .open_file(Path::new(
                &self.root.join("a.rs").to_string_lossy().to_string(),
            ))
            .unwrap();
        test
    }
}

fn buffer_text(test: &EditorTest) -> String {
    test.editor.buffer().rope().to_string()
}

/// First buffer line whose text contains `needle`.
fn line_containing(test: &EditorTest, needle: &str) -> usize {
    let buffer = test.editor.buffer();
    (0..buffer.line_count())
        .find(|&index| {
            buffer
                .line_text(index)
                .is_some_and(|text| text.contains(needle))
        })
        .unwrap_or_else(|| panic!("no line containing {needle:?} in review"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_patch_is_highlighted_with_each_file_s_own_grammar() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");

    // `let` inside an added line is a Rust keyword, not "an added line".
    let added = line_index_of(&test, "+    let x = 42;");
    let highlights = test.editor.buffer().highlights_for_line(added);
    assert!(
        highlights.iter().any(|(range, group)| *group
            == ovim_core::syntax::HighlightGroup::Keyword
            && range.start == 5),
        "expected a keyword span over `let`: {highlights:?}"
    );
    // The marker column still carries the diff colour.
    assert!(highlights
        .iter()
        .any(|(range, group)| *range == (0..1)
            && *group == ovim_core::syntax::HighlightGroup::DiffAdded));

    // Removed lines are highlighted from the old side of the patch.
    let removed = line_index_of(&test, "-    let x = 1;");
    assert!(test
        .editor
        .buffer()
        .highlights_for_line(removed)
        .iter()
        .any(|(_, group)| *group == ovim_core::syntax::HighlightGroup::Keyword));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn changed_rows_carry_a_background_tint_and_context_rows_do_not() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");

    let added = line_index_of(&test, "+    let x = 42;");
    let removed = line_index_of(&test, "-    let x = 1;");
    let context = line_index_of(&test, " fn main() {");
    let review = test.editor.diff_review().unwrap();
    assert_eq!(review.line_tints(added).len(), 1);
    assert!(review.line_tints(added)[0].1, "added rows tint green");
    assert!(!review.line_tints(removed)[0].1, "removed rows tint red");
    assert!(review.line_tints(context).is_empty());

    // The band runs to the end of the row so the renderer can carry it
    // through the padding to the right edge.
    assert_eq!(review.line_trailing_tint(added), Some(true));
    assert_eq!(review.line_trailing_tint(removed), Some(false));
    assert_eq!(review.line_trailing_tint(context), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn only_the_side_that_changed_is_tinted_in_the_split_layout() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gds");

    // A rewritten line pairs a removal on the left with an addition on the
    // right, so the row carries both tints and the band ends on the new side.
    let row = line_containing(&test, "let x = 1;");
    let review = test.editor.diff_review().unwrap();
    let tints = review.line_tints(row);
    assert_eq!(tints.len(), 2, "{tints:?}");
    assert!(!tints[0].1, "the old column is a removal");
    assert!(tints[1].1, "the new column is an addition");
    assert!(
        tints[0].0.end < tints[1].0.start,
        "the separator is untinted"
    );
    assert_eq!(review.line_trailing_tint(row), Some(true));

    // A row that only adds leaves the old column plain.
    let added_only = line_containing(&test, "let y = \"two\";");
    let tints = test.editor.diff_review().unwrap().line_tints(added_only);
    assert_eq!(tints.len(), 1, "{tints:?}");
    assert!(tints[0].1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn s_switches_between_the_unified_and_split_layouts() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");
    assert!(buffer_text(&test).contains("diff --git a/a.rs b/a.rs"));
    assert!(!buffer_text(&test).contains('│'));

    test.keys("s");

    let text = buffer_text(&test);
    assert!(
        text.contains('│'),
        "side-by-side rows are separated: {text}"
    );
    assert!(
        text.contains("── a.rs "),
        "the raw file header becomes a banner: {text}"
    );
    assert!(
        !text.contains("diff --git"),
        "the git header is folded into the banner: {text}"
    );
    // Both sides of the rewritten line share a row, with their line numbers.
    let row = line_containing(&test, "let x = 1;");
    let row_text = test.editor.buffer().line_text(row).unwrap().to_string();
    assert!(row_text.contains("let x = 42;"), "{row_text}");
    assert!(row_text.starts_with("  2 -"), "{row_text}");

    test.keys("s");
    assert!(buffer_text(&test).contains("diff --git a/a.rs b/a.rs"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn split_rows_align_after_expanding_tabs() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gds");

    // The added line is indented with a tab; both sides must still line up on
    // the separator column.
    let rows: Vec<String> = (0..test.editor.buffer().line_count())
        .filter_map(|index| {
            test.editor
                .buffer()
                .line_text(index)
                .map(|text| text.to_string())
        })
        .filter(|text| text.contains('│'))
        .collect();
    assert!(rows.len() >= 4, "{rows:?}");
    let columns: Vec<usize> = rows
        .iter()
        .map(|text| text.chars().position(|c| c == '│').unwrap())
        .collect();
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "every row separates at the same column: {columns:?}"
    );
    assert!(
        rows.iter()
            .any(|text| text.contains("    let y = \"two\";")),
        "the tab is expanded, not passed through: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enter_in_the_split_layout_opens_the_column_under_the_cursor() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gds");

    let row = line_containing(&test, "let x = 42;");
    let text = test.editor.buffer().line_text(row).unwrap().to_string();
    // Land on the `4` of `42` on the new side. Columns are graphemes, and the
    // row's separator is multi-byte.
    let col = text[..text.find("42;").unwrap()].chars().count();
    test.set_cursor(row, col);
    test.press_key(KeyCode::Enter);

    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.rs"));
    let cursor = test.editor.buffer().cursor();
    assert_eq!(cursor.line(), 1, "the new side's line 2");
    assert_eq!(
        current_line(&test)
            .chars()
            .nth(cursor.col().0)
            .unwrap_or(' '),
        '4',
        "cursor keeps its column: {:?}",
        current_line(&test)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enter_on_the_old_column_lands_where_the_removal_happened() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gds");

    let row = line_containing(&test, "let x = 1;");
    let text = test.editor.buffer().line_text(row).unwrap().to_string();
    let col = text[..text.find("let x = 1;").unwrap()].chars().count();
    test.set_cursor(row, col);
    test.press_key(KeyCode::Enter);

    assert!(test.editor.buffer().file_path().unwrap().ends_with("a.rs"));
    assert_eq!(test.editor.buffer().cursor().line(), 1);
    assert_eq!(current_line(&test), "    let x = 42;");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn clicking_the_toolbar_switches_layout_without_moving_the_cursor() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");

    let toolbar = line_containing(&test, "[ Split ]");
    let text = test.editor.buffer().line_text(toolbar).unwrap().to_string();
    let split_col = text.find("[ Split ]").unwrap() + 2;
    let unified_col = text.find("[ Unified ]").unwrap() + 2;
    let hint_col = text.find("· click").unwrap();

    assert!(test.editor.diff_review_click(toolbar, split_col));
    assert!(buffer_text(&test).contains('│'));

    assert!(test.editor.diff_review_click(toolbar, unified_col));
    assert!(buffer_text(&test).contains("diff --git"));

    assert!(
        !test.editor.diff_review_click(toolbar, hint_col),
        "clicks outside the buttons fall through to the cursor"
    );
    assert!(
        !test.editor.diff_review_click(toolbar + 1, split_col),
        "only the toolbar row is clickable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn switching_layout_keeps_the_cursor_on_the_same_change() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");

    let added = line_index_of(&test, "+\tlet y = \"two\";");
    test.set_cursor(added, 0);
    test.keys("s");
    assert!(
        current_line(&test).contains("let y = \"two\";"),
        "cursor followed the change into the split view: {:?}",
        current_line(&test)
    );

    test.keys("s");
    assert_eq!(current_line(&test), "+\tlet y = \"two\";");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn hunk_and_file_navigation_work_in_the_split_layout() {
    let fixture = Fixture::new();
    let mut test = open_editor_on(&fixture, "a.txt");
    test.keys(" gds");

    test.keys("]f");
    assert!(
        current_line(&test).contains("── a.txt "),
        "{}",
        current_line(&test)
    );
    test.keys("]f");
    assert!(
        current_line(&test).contains("── b.txt "),
        "{}",
        current_line(&test)
    );
    test.keys("[f");
    assert!(
        current_line(&test).contains("── a.txt "),
        "{}",
        current_line(&test)
    );

    test.keys("]c");
    assert_eq!(current_line(&test), "@@ -1,3 +1,4 @@");
    test.keys("]c");
    assert_eq!(current_line(&test), "@@ -0,0 +1 @@");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_layout_choice_survives_closing_and_reopening_the_review() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gds");
    test.keys("q");
    assert!(test.editor.diff_review().is_none());

    test.keys(" gd");
    assert!(
        buffer_text(&test).contains('│'),
        "the next review opens in the layout you last chose"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn gitdifflayout_command_sets_the_layout_explicitly() {
    let fixture = RustFixture::new();
    let mut test = fixture.open();
    test.keys(" gd");

    let result = ovim_core::commands::execute_command(&mut test.editor, "GitDiffLayout split");
    assert!(
        matches!(result, ovim_core::CommandResult::Success(_)),
        "{result:?}"
    );
    assert!(buffer_text(&test).contains('│'));

    let result = ovim_core::commands::execute_command(&mut test.editor, "GitDiffLayout sideways");
    assert!(
        matches!(result, ovim_core::CommandResult::Error(_)),
        "{result:?}"
    );
    assert!(
        buffer_text(&test).contains('│'),
        "an invalid name changes nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_split_layout_handles_a_branch_with_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let repo = Repository::init(&root).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    fs::write(root.join("a.txt"), "one\n").unwrap();
    commit_all(&repo, "c1");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("feature", &head, false).unwrap();
    repo.set_head("refs/heads/feature").unwrap();

    let mut test = EditorTest::new("");
    test.editor.open_file(root.join("a.txt").as_path()).unwrap();
    test.keys(" gds");

    let text = buffer_text(&test);
    assert!(text.contains("No changes"), "{text}");
    // Navigation on an empty review reports rather than panics.
    test.keys("]c");
    assert_eq!(test.editor.status_message(), "Last hunk");
    test.keys("]f");
    assert_eq!(test.editor.status_message(), "Last file");
    test.press_key(KeyCode::Enter);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_split_layout_shows_deletions_and_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let repo = Repository::init(&root).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    fs::write(root.join("gone.txt"), "one\ntwo\n").unwrap();
    fs::write(root.join("keep.txt"), "keep\n").unwrap();
    commit_all(&repo, "c1");

    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("feature", &head, false).unwrap();
    repo.set_head("refs/heads/feature").unwrap();
    fs::remove_file(root.join("gone.txt")).unwrap();
    fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 0]).unwrap();
    commit_all(&repo, "delete and add a binary");

    let mut test = EditorTest::new("");
    test.editor
        .open_file(root.join("keep.txt").as_path())
        .unwrap();
    test.keys(" gds");

    let text = buffer_text(&test);
    assert!(text.contains("── blob.bin "), "{text}");
    assert!(text.contains("── gone.txt "), "{text}");
    assert!(text.contains("D  +0 −2"), "{text}");

    // Enter on a deleted file refuses instead of opening a missing path.
    let row = line_containing(&test, "- one");
    test.set_cursor(row, 8);
    test.press_key(KeyCode::Enter);
    assert!(test.editor.is_diff_review_buffer());
    assert!(test
        .editor
        .buffer()
        .file_path()
        .is_none_or(|path| !path.ends_with("gone.txt")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_long_line_wraps_inside_its_split_column() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let repo = Repository::init(&root).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    fs::write(root.join("a.txt"), "short\n").unwrap();
    commit_all(&repo, "c1");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("feature", &head, false).unwrap();
    repo.set_head("refs/heads/feature").unwrap();
    let long = "x".repeat(1200);
    fs::write(root.join("a.txt"), format!("short\n{long}\n")).unwrap();
    commit_all(&repo, "long line");

    let mut test = EditorTest::new("");
    test.editor.open_file(root.join("a.txt").as_path()).unwrap();
    test.keys(" gds");

    let rows: Vec<String> = (0..test.editor.buffer().line_count())
        .filter_map(|index| test.editor.buffer().line_text(index).map(|t| t.to_string()))
        .filter(|text| text.contains("xxx"))
        .collect();
    assert!(rows.len() > 1, "the long line wraps: {}", rows.len());
    // Wrapping is capped, and the clipped row says so.
    assert!(rows.len() <= 12, "wrapping is bounded: {}", rows.len());
    assert!(
        rows.last().unwrap().contains('…'),
        "the clipped row is marked: {:?}",
        rows.last()
    );
    // Every wrapped row still ends at the same separator column.
    let columns: Vec<usize> = rows
        .iter()
        .map(|text| text.chars().position(|c| c == '│').unwrap())
        .collect();
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "{columns:?}"
    );

    // Enter from a continuation row lands on the same source line.
    let row = line_containing(&test, "xxx");
    test.set_cursor(row + 1, 70);
    test.press_key(KeyCode::Enter);
    assert_eq!(test.editor.buffer().cursor().line(), 1);
}
