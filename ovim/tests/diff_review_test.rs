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
