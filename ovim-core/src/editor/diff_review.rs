//! Branch diff review.
//!
//! `<Space>gd` (or `:GitDiff`) opens a read-only, syntax-highlighted patch of
//! everything the current branch changed relative to the default branch,
//! including uncommitted work. Inside the review:
//!
//! - `]c` / `[c` move between hunks, `]f` / `[f` between files
//! - `Enter` (or `gf`) opens the file at the line under the cursor in the tab
//!   the review was opened from; `<Space>gd` returns to the review, refreshed
//! - `r` refreshes, `q` closes, `<Space>gf` fetches the base branch
//!
//! The base is chosen by [`crate::native_diff::resolve_base`], which prefers
//! whichever of `main` / `origin/main` has the most recent merge-base with
//! HEAD so a stale local or remote copy never pollutes the review.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Duration;

use super::{Editor, ToastLevel, ToastRequest, ToastSource};
use crate::buffer::{Buffer, BufferId};
use crate::native_diff::{
    self, BaseKind, DiffFile, PatchLine, PatchLineKind, ReviewBase, ReviewPatch,
};
use crate::unicode::GraphemeCol;

/// Display-name prefix shared with the GUI, which uses it to enable diff
/// line styling for pathless buffers.
pub const DIFF_REVIEW_TITLE_PREFIX: &str = "Diff · ";

/// Fake path used to select the `diff` grammar for the pathless review buffer.
const REVIEW_SYNTAX_PATH: &str = "review.diff";

const KEY_HINT: &str =
    "# Enter open at cursor · ]c [c hunk · ]f [f file · r refresh · q close · <Space>gf fetch base";

/// State of the open branch review, if any.
pub struct DiffReviewState {
    /// The read-only buffer showing the patch.
    pub buffer_id: BufferId,
    /// Buffer that was current when the review was (re-)entered. `Enter`
    /// opens files in the tab showing this buffer.
    pub origin_buffer_id: Option<BufferId>,
    /// Tab index the review was (re-)entered from; fallback when
    /// `origin_buffer_id` is no longer shown in any tab.
    pub origin_tab: usize,
    /// User-supplied comparison (`:GitDiff <spec>`); `None` means auto.
    pub explicit_spec: Option<String>,
    pub root: PathBuf,
    pub base: ReviewBase,
    pub files: Vec<DiffFile>,
    /// Source mapping per buffer line; `None` for header/summary lines.
    pub lines: Vec<Option<PatchLine>>,
    /// `(buffer line, file index)` for each row of the file summary.
    pub stat_rows: Vec<(usize, usize)>,
    /// Buffer lines of hunk headers.
    pub hunk_lines: Vec<usize>,
    /// Buffer line of the first header line of each file.
    pub file_lines: Vec<usize>,
    /// Buffer text split into lines (for refresh anchoring).
    text_lines: Vec<String>,
}

impl DiffReviewState {
    fn file_for_stat_row(&self, line: usize) -> Option<usize> {
        self.stat_rows
            .iter()
            .find(|(row, _)| *row == line)
            .map(|(_, file)| *file)
    }

    fn info(&self, line: usize) -> Option<PatchLine> {
        self.lines.get(line).copied().flatten()
    }

    /// Best `(file index, 1-based line)` source target for a buffer line.
    fn source_target(&self, line: usize) -> Option<(usize, usize)> {
        let info = self.info(line)?;
        let file = info.file?;
        if let Some(new_line) = info.new_line {
            return Some((file, new_line));
        }
        // File headers and meta lines: use the first hunk that follows.
        let next_hunk = self
            .lines
            .iter()
            .skip(line + 1)
            .map_while(|entry| entry.as_ref())
            .find(|entry| entry.file == Some(file) && entry.kind == PatchLineKind::HunkHeader)
            .and_then(|entry| entry.new_line);
        Some((file, next_hunk.unwrap_or(1)))
    }

    /// Where the cursor is, in source terms, so a refresh can restore it.
    fn anchor(&self, line: usize, text: Option<String>) -> Option<Anchor> {
        let target = self
            .source_target(line)
            .or_else(|| self.file_for_stat_row(line).map(|file| (file, 1)))?;
        let path = self.files.get(target.0)?.path.clone();
        Some(Anchor {
            path,
            new_line: target.1,
            text: text.filter(|_| self.info(line).is_some()),
        })
    }

    /// Buffer line to restore after a refresh: the same patch line if its
    /// text still exists in that file (nearest to the old position), else the
    /// first line at or after the old source line, else the file header.
    fn line_for_anchor(&self, anchor: &Anchor) -> Option<usize> {
        let file = self
            .files
            .iter()
            .position(|entry| entry.path == anchor.path)?;
        let candidates = self.lines.iter().enumerate().filter_map(|(line, entry)| {
            let entry = entry.as_ref()?;
            (entry.file == Some(file) && entry.kind != PatchLineKind::FileHeader)
                .then_some((line, entry.new_line?))
        });

        if let Some(text) = &anchor.text {
            let same_text = candidates
                .clone()
                .filter(|(line, _)| self.line_text(*line).as_deref() == Some(text.as_str()))
                .min_by_key(|(_, new_line)| new_line.abs_diff(anchor.new_line));
            if let Some((line, _)) = same_text {
                return Some(line);
            }
        }

        candidates
            .filter(|(_, new_line)| *new_line >= anchor.new_line)
            .min_by_key(|(_, new_line)| *new_line)
            .map(|(line, _)| line)
            .or_else(|| self.file_lines.get(file).copied())
    }

    fn line_text(&self, line: usize) -> Option<&str> {
        self.text_lines.get(line).map(String::as_str)
    }
}

/// Source position of the cursor, captured before a refresh.
struct Anchor {
    path: String,
    new_line: usize,
    /// Text of the patch line under the cursor, when it was a patch line.
    text: Option<String>,
}

/// A background `git fetch` started by `<Space>gf`.
pub struct PendingGitFetch {
    receiver: Receiver<Result<(), String>>,
    target: String,
}

struct Rendered {
    title: String,
    text: String,
    lines: Vec<Option<PatchLine>>,
    stat_rows: Vec<(usize, usize)>,
    hunk_lines: Vec<usize>,
    file_lines: Vec<usize>,
}

impl Editor {
    pub fn diff_review(&self) -> Option<&DiffReviewState> {
        self.ui_panels.diff_review.as_ref()
    }

    /// True when the current buffer is the branch review.
    pub fn is_diff_review_buffer(&self) -> bool {
        self.ui_panels
            .diff_review
            .as_ref()
            .is_some_and(|state| state.buffer_id == self.buffer().id())
    }

    /// `<Space>gd`: open the review, return to it from a file, or leave it.
    pub fn toggle_diff_review(&mut self) {
        if self.is_diff_review_buffer() {
            self.leave_diff_review();
            return;
        }
        if self.review_buffer_index().is_some() {
            self.enter_diff_review();
            return;
        }
        if let Err(error) = self.open_diff_review(None) {
            self.review_toast(ToastLevel::Error, format!("Diff review: {error:#}"));
        }
    }

    /// `:GitDiff [spec]`. Reuses the open review buffer when there is one.
    pub fn open_diff_review(&mut self, spec: Option<&str>) -> anyhow::Result<()> {
        let explicit_spec = spec.map(str::trim).filter(|spec| !spec.is_empty());
        if let Some(state) = self.ui_panels.diff_review.as_mut() {
            state.explicit_spec = explicit_spec.map(str::to_string);
        }
        if self.review_buffer_index().is_some() {
            if !self.is_diff_review_buffer() {
                self.enter_diff_review();
            } else {
                self.refresh_diff_review();
            }
            return Ok(());
        }

        let root_hint = self.diff_review_root_hint();
        let base = match explicit_spec {
            Some(spec) => ReviewBase::explicit(spec),
            None => native_diff::resolve_base(&root_hint)?,
        };
        let patch = native_diff::review_patch(&root_hint, &base)?;
        let rendered = render_review(&patch, self.unsaved_buffer_count());

        let origin_buffer_id = Some(self.buffer().id());
        let origin_tab = self.current_tab_index();
        self.open_diff_buffer_in_new_tab(&rendered.title, &rendered.text);
        self.buffer_mut()
            .enable_syntax_highlighting_for_path(REVIEW_SYNTAX_PATH);
        let buffer_id = self.buffer().id();

        self.ui_panels.diff_review = Some(DiffReviewState {
            buffer_id,
            origin_buffer_id,
            origin_tab,
            explicit_spec: explicit_spec.map(str::to_string),
            root: patch.root.clone(),
            base: patch.base.clone(),
            files: patch.files.clone(),
            lines: rendered.lines,
            stat_rows: rendered.stat_rows,
            hunk_lines: rendered.hunk_lines,
            file_lines: rendered.file_lines,
            text_lines: rendered.text.lines().map(str::to_string).collect(),
        });
        self.set_status_message(summary_message(&patch));
        self.mark_dirty();
        Ok(())
    }

    /// Recomputes the patch, keeping the cursor on the same source location.
    pub fn refresh_diff_review(&mut self) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        let (root, base, anchor, explicit) = {
            let state = self.ui_panels.diff_review.as_ref().expect("review state");
            let cursor_line = self.buffers[index].cursor().line();
            let cursor_text = state.line_text(cursor_line).map(str::to_string);
            (
                state.root.clone(),
                state.base.clone(),
                state.anchor(cursor_line, cursor_text),
                state.explicit_spec.clone(),
            )
        };

        let base = match &explicit {
            Some(spec) => ReviewBase::explicit(spec),
            None => match native_diff::resolve_base(&root) {
                Ok(base) => base,
                Err(_) => base,
            },
        };
        let patch = match native_diff::review_patch(&root, &base) {
            Ok(patch) => patch,
            Err(error) => {
                self.review_toast(ToastLevel::Error, format!("Diff review: {error:#}"));
                return;
            }
        };
        let rendered = render_review(&patch, self.unsaved_buffer_count());

        let buffer = &mut self.buffers[index];
        buffer.replace_content(&rendered.text);
        buffer.set_display_name(rendered.title);

        let state = self.ui_panels.diff_review.as_mut().expect("review state");
        state.base = patch.base.clone();
        state.files = patch.files.clone();
        state.lines = rendered.lines;
        state.stat_rows = rendered.stat_rows;
        state.hunk_lines = rendered.hunk_lines;
        state.file_lines = rendered.file_lines;
        state.text_lines = rendered.text.lines().map(str::to_string).collect();

        if let Some(anchor) = anchor {
            if let Some(line) = state.line_for_anchor(&anchor) {
                self.buffers[index]
                    .cursor_mut()
                    .set_position(line, GraphemeCol(0));
            }
        }
        if index == self.current_buffer_index {
            self.buffer_mut().validate_cursor_position();
            self.center_cursor_in_viewport();
        }
        self.set_status_message(summary_message(&patch));
        self.mark_dirty();
    }

    /// Closes the review tab and drops its buffer and state.
    pub fn close_diff_review(&mut self) {
        let Some(index) = self.review_buffer_index() else {
            self.ui_panels.diff_review = None;
            return;
        };
        if index == self.current_buffer_index {
            if self.tab_count() > 1 {
                self.close_current_tab();
            } else if self.buffers.len() > 1 {
                let other = if index == 0 { 1 } else { index - 1 };
                self.switch_to_buffer(other);
                self.sync_current_tab_buffer();
            } else {
                self.add_buffer(Buffer::new());
                self.sync_current_tab_buffer();
            }
        }
        self.remove_review_buffer();
        self.ui_panels.diff_review = None;
        self.set_status_message("Diff review closed");
        self.mark_dirty();
    }

    /// `Enter` in the review: open the file at the line under the cursor in
    /// the originating tab.
    pub fn diff_review_open_at_cursor(&mut self) {
        let Some(state) = self.ui_panels.diff_review.as_ref() else {
            return;
        };
        let cursor = self.buffer().cursor();
        let line = cursor.line();
        let col = cursor.col().0;

        if let Some(file) = state.file_for_stat_row(line) {
            if let Some(&target) = state.file_lines.get(file) {
                self.jump_to_review_line(target);
            }
            return;
        }

        let Some((file_index, new_line)) = state.source_target(line) else {
            self.set_status_message("Move to a changed line and press Enter to open it");
            return;
        };
        let file = &state.files[file_index];
        if file.status == "deleted" {
            self.review_toast(
                ToastLevel::Warning,
                format!("{} was deleted on this branch", file.path),
            );
            return;
        }
        let is_text_line = matches!(
            state.info(line).map(|info| info.kind),
            Some(PatchLineKind::Added | PatchLineKind::Removed | PatchLineKind::Context)
        );
        let target_col = if is_text_line {
            col.saturating_sub(1)
        } else {
            0
        };
        let path = state.root.join(&file.path);
        let display_path = file.path.clone();

        self.go_to_review_origin();
        if let Err(error) = self.open_file(&path) {
            self.review_toast(
                ToastLevel::Error,
                format!("Could not open {display_path}: {error}"),
            );
            return;
        }
        self.buffer_mut()
            .cursor_mut()
            .set_position(new_line.saturating_sub(1), GraphemeCol(target_col));
        self.buffer_mut().validate_cursor_position();
        self.center_cursor_in_viewport();
        self.set_status_message(format!(
            "{display_path}:{new_line} · <Space>gd returns to the review"
        ));
        self.mark_dirty();
    }

    /// `]c` / `[c`: next or previous hunk in the review, or next changed
    /// region (git gutter) in an ordinary file.
    pub fn goto_change(&mut self, forward: bool) {
        if self.is_diff_review_buffer() {
            self.diff_review_goto_hunk(forward);
            return;
        }
        let cursor_line = self.buffer().cursor().line();
        let starts = self.buffer().git_status().hunk_starts();
        let target = if forward {
            starts.iter().copied().find(|line| *line > cursor_line)
        } else {
            starts
                .iter()
                .rev()
                .copied()
                .find(|line| *line < cursor_line)
        };
        match target {
            Some(line) => {
                self.buffer_mut()
                    .cursor_mut()
                    .set_position(line, GraphemeCol(0));
                self.buffer_mut().validate_cursor_position();
                self.center_cursor_in_viewport();
            }
            None => self.set_status_message(if forward {
                "No more changes below"
            } else {
                "No more changes above"
            }),
        }
    }

    pub fn diff_review_goto_hunk(&mut self, forward: bool) {
        let Some(state) = self.ui_panels.diff_review.as_ref() else {
            return;
        };
        let cursor_line = self.buffer().cursor().line();
        let target = next_in(&state.hunk_lines, cursor_line, forward);
        match target {
            Some(line) => self.jump_to_review_line(line),
            None => self.set_status_message(if forward { "Last hunk" } else { "First hunk" }),
        }
    }

    pub fn diff_review_goto_file(&mut self, forward: bool) {
        let Some(state) = self.ui_panels.diff_review.as_ref() else {
            return;
        };
        let cursor_line = self.buffer().cursor().line();
        let target = next_in(&state.file_lines, cursor_line, forward);
        match target {
            Some(line) => self.jump_to_review_line(line),
            None => self.set_status_message(if forward { "Last file" } else { "First file" }),
        }
    }

    /// `<Space>gf`: fetch the review base's remote branch in the background
    /// and refresh the review when it lands.
    pub fn fetch_review_base(&mut self) {
        if self.ui_panels.pending_git_fetch.is_some() {
            self.review_toast(ToastLevel::Info, "A fetch is already running");
            return;
        }
        let root = self.diff_review_root_hint();
        let base = match self.ui_panels.diff_review.as_ref() {
            Some(state) => Ok(state.base.clone()),
            None => native_diff::resolve_base(&root),
        };
        let remote = match base {
            Ok(base) => base.remote,
            Err(error) => {
                self.review_toast(ToastLevel::Error, format!("Git fetch: {error:#}"));
                return;
            }
        };
        let Some((remote, branch)) = remote else {
            self.review_toast(
                ToastLevel::Warning,
                "The review base is not a remote branch; nothing to fetch",
            );
            return;
        };
        let target = format!("{remote}/{branch}");
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let result = std::process::Command::new("git")
                .args(["fetch", "--no-tags", &remote, &branch])
                .current_dir(&root)
                .output();
            let outcome = match result {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
                Err(error) => Err(format!("could not run git: {error}")),
            };
            let _ = sender.send(outcome);
        });
        self.ui_panels.pending_git_fetch = Some(PendingGitFetch {
            receiver,
            target: target.clone(),
        });
        self.set_status_message(format!("Fetching {target}…"));
    }

    /// Polls a background fetch. Returns true when something changed.
    pub fn poll_git_fetch(&mut self) -> bool {
        let Some(pending) = self.ui_panels.pending_git_fetch.take() else {
            return false;
        };
        match pending.receiver.try_recv() {
            Ok(Ok(())) => {
                let refreshed = self.review_buffer_index().is_some();
                if refreshed {
                    self.refresh_diff_review();
                }
                self.review_toast(
                    ToastLevel::Success,
                    if refreshed {
                        format!("Fetched {} · review refreshed", pending.target)
                    } else {
                        format!("Fetched {}", pending.target)
                    },
                );
                true
            }
            Ok(Err(message)) => {
                let message = if message.is_empty() {
                    "git fetch failed".to_string()
                } else {
                    message
                };
                self.review_toast(
                    ToastLevel::Error,
                    format!("Fetch {} failed: {message}", pending.target),
                );
                true
            }
            Err(TryRecvError::Empty) => {
                self.ui_panels.pending_git_fetch = Some(pending);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.review_toast(
                    ToastLevel::Error,
                    format!("Fetch {} failed unexpectedly", pending.target),
                );
                true
            }
        }
    }

    // -- internals ---------------------------------------------------------

    fn review_buffer_index(&self) -> Option<usize> {
        let state = self.ui_panels.diff_review.as_ref()?;
        self.find_buffer_index_by_id(state.buffer_id)
    }

    /// Switches to the review tab (or shows the review buffer in a new tab)
    /// and refreshes it.
    fn enter_diff_review(&mut self) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        let review_id = self.buffers[index].id();
        let current_id = self.buffer().id();
        let current_tab = self.current_tab_index();
        if let Some(state) = self.ui_panels.diff_review.as_mut() {
            state.origin_buffer_id = Some(current_id);
            state.origin_tab = current_tab;
        }

        let tab = self
            .tab_page_manager()
            .tabs()
            .iter()
            .position(|tab| tab.buffer_id() == Some(review_id));
        match tab {
            Some(tab) => self.goto_tab(tab),
            None => {
                self.sync_current_tab_buffer();
                self.tab_page_manager_mut().new_tab();
                self.tab_page_manager_mut()
                    .current_tab_mut()
                    .set_buffer_id(review_id);
                self.switch_to_buffer(index);
            }
        }
        self.refresh_diff_review();
    }

    fn leave_diff_review(&mut self) {
        self.go_to_review_origin();
        self.mark_dirty();
    }

    /// Switches to the tab the review was entered from.
    fn go_to_review_origin(&mut self) {
        let Some(state) = self.ui_panels.diff_review.as_ref() else {
            return;
        };
        let review_id = state.buffer_id;
        let origin_id = state.origin_buffer_id;
        let origin_tab = state.origin_tab;
        let review_tab = self.current_tab_index();

        let tabs = self.tab_page_manager().tabs();
        let by_buffer = origin_id.and_then(|origin| {
            tabs.iter().position(|tab| {
                tab.buffer_id() == Some(origin) && tab.buffer_id() != Some(review_id)
            })
        });
        let target = by_buffer.or_else(|| {
            (origin_tab < tabs.len() && origin_tab != review_tab).then_some(origin_tab)
        });
        match target {
            Some(tab) => self.goto_tab(tab),
            None => {
                // Nowhere to return to: give the file its own tab and leave
                // the review where it is.
                self.new_tab();
            }
        }
    }

    fn jump_to_review_line(&mut self, line: usize) {
        self.buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(0));
        self.buffer_mut().validate_cursor_position();
        self.center_cursor_in_viewport();
        self.mark_dirty();
    }

    fn remove_review_buffer(&mut self) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        if index == self.current_buffer_index {
            return;
        }
        self.buffers.remove(index);
        if self.current_buffer_index > index {
            self.current_buffer_index -= 1;
        }
    }

    fn diff_review_root_hint(&self) -> PathBuf {
        if let Some(state) = self.ui_panels.diff_review.as_ref() {
            return state.root.clone();
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let Some(path) = self.buffer().file_path() else {
            return cwd;
        };
        let path = Path::new(path);
        if path.is_absolute() {
            path.parent().map(Path::to_path_buf).unwrap_or(cwd)
        } else {
            cwd.join(path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(cwd)
        }
    }

    fn unsaved_buffer_count(&self) -> usize {
        self.buffers
            .iter()
            .filter(|buffer| buffer.is_modified() && buffer.file_path().is_some())
            .count()
    }

    fn review_toast(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.push_toast(ToastRequest::new(ToastSource::Git, level, message));
    }
}

fn next_in(lines: &[usize], cursor_line: usize, forward: bool) -> Option<usize> {
    if forward {
        lines.iter().copied().find(|line| *line > cursor_line)
    } else {
        lines.iter().rev().copied().find(|line| *line < cursor_line)
    }
}

fn summary_message(patch: &ReviewPatch) -> String {
    if patch.files.is_empty() {
        return format!("No changes: {} matches {}", patch.head, patch.base.name);
    }
    format!(
        "{} → {}: {} · +{} −{}",
        patch.head,
        patch.base.name,
        plural(patch.files.len(), "file"),
        patch.additions(),
        patch.deletions()
    )
}

/// Renders the review buffer: a summary header, a file list, then the patch.
fn render_review(patch: &ReviewPatch, unsaved_buffers: usize) -> Rendered {
    let mut text = String::new();
    let mut lines: Vec<Option<PatchLine>> = Vec::new();
    let mut stat_rows = Vec::new();

    let header = |text: &mut String, lines: &mut Vec<Option<PatchLine>>, line: &str| {
        text.push_str(line);
        text.push('\n');
        lines.push(None);
    };

    header(
        &mut text,
        &mut lines,
        &format!("{} → {}", patch.head, patch.base.name),
    );
    header(&mut text, &mut lines, &describe_base(patch));
    if patch.files.is_empty() {
        header(&mut text, &mut lines, "No changes");
    } else {
        header(
            &mut text,
            &mut lines,
            &format!(
                "{} · +{} −{}",
                plural(patch.files.len(), "file"),
                patch.additions(),
                patch.deletions()
            ),
        );
    }
    if unsaved_buffers > 0 {
        header(
            &mut text,
            &mut lines,
            &format!(
                "! {} with unsaved changes; the review reflects what is on disk",
                plural(unsaved_buffers, "buffer")
            ),
        );
    }
    if patch.truncated {
        header(&mut text, &mut lines, "! Diff truncated at 4 MiB");
    }

    if !patch.files.is_empty() {
        header(&mut text, &mut lines, "");
        let width = patch
            .files
            .iter()
            .map(|file| stat_label(file).chars().count())
            .max()
            .unwrap_or(0)
            .min(72);
        for (index, file) in patch.files.iter().enumerate() {
            stat_rows.push((lines.len(), index));
            let label = stat_label(file);
            let counts = if file.binary {
                "binary".to_string()
            } else {
                let mut counts = String::new();
                if file.additions > 0 || file.deletions == 0 {
                    counts.push_str(&format!("+{}", file.additions));
                }
                if file.deletions > 0 {
                    if !counts.is_empty() {
                        counts.push(' ');
                    }
                    counts.push_str(&format!("−{}", file.deletions));
                }
                counts
            };
            header(
                &mut text,
                &mut lines,
                &format!(
                    "  {}  {label:<width$}  {counts}",
                    status_letter(&file.status)
                ),
            );
        }
    }

    header(&mut text, &mut lines, "");
    header(&mut text, &mut lines, KEY_HINT);
    header(&mut text, &mut lines, "");

    let offset = lines.len();
    text.push_str(&patch.text);
    lines.extend(patch.lines.iter().copied().map(Some));

    let mut hunk_lines = Vec::new();
    let mut file_lines = Vec::new();
    let mut seen_file: Option<usize> = None;
    for (index, info) in patch.lines.iter().enumerate() {
        let line = offset + index;
        if info.kind == PatchLineKind::HunkHeader {
            hunk_lines.push(line);
        }
        if info.kind == PatchLineKind::FileHeader && info.file != seen_file {
            seen_file = info.file;
            file_lines.push(line);
        }
    }
    // `file_lines` must be indexable by file index; fill gaps for files that
    // produced no header (should not happen, but keep lookups safe).
    if file_lines.len() < patch.files.len() {
        let mut by_file = vec![usize::MAX; patch.files.len()];
        for (index, info) in patch.lines.iter().enumerate() {
            if let (PatchLineKind::FileHeader, Some(file)) = (info.kind, info.file) {
                if by_file[file] == usize::MAX {
                    by_file[file] = offset + index;
                }
            }
        }
        let fallback = offset;
        file_lines = by_file
            .into_iter()
            .map(|line| if line == usize::MAX { fallback } else { line })
            .collect();
    }

    Rendered {
        title: format!(
            "{DIFF_REVIEW_TITLE_PREFIX}{} → {}",
            patch.head, patch.base.name
        ),
        text,
        lines,
        stat_rows,
        hunk_lines,
        file_lines,
    }
}

fn describe_base(patch: &ReviewPatch) -> String {
    let base = &patch.base;
    match base.kind {
        BaseKind::OnDefaultBranch => {
            format!("On {}: uncommitted changes against HEAD", base.name)
        }
        BaseKind::NoDefaultBranch => {
            "No default branch found: uncommitted changes against HEAD".to_string()
        }
        BaseKind::Explicit if !base.spec.ends_with("...WORKTREE") => {
            format!("Comparison {}", base.spec)
        }
        BaseKind::DefaultBranch | BaseKind::Explicit => {
            let mut parts = Vec::new();
            if let Some(merge_base) = &patch.merge_base {
                parts.push(format!("merge-base {merge_base}"));
            }
            parts.push(format!("{} ahead", plural(patch.ahead, "commit")));
            if patch.behind > 0 {
                parts.push(format!("{} behind", patch.behind));
            }
            if base.remote.is_some() {
                let fetched = match (base.ever_fetched, base.fetched_ago) {
                    (false, _) => "never fetched".to_string(),
                    (true, Some(age)) => format!("fetched {}", humanize(age)),
                    (true, None) => "fetched".to_string(),
                };
                parts.push(format!("{} {fetched}", base.name));
            }
            parts.join(" · ")
        }
    }
}

fn stat_label(file: &DiffFile) -> String {
    match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    }
}

fn status_letter(status: &str) -> char {
    match status {
        "added" => 'A',
        "deleted" => 'D',
        "renamed" => 'R',
        "copied" => 'C',
        "typechanged" => 'T',
        "conflicted" => 'U',
        _ => 'M',
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn humanize(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 90 {
        "just now".to_string()
    } else if seconds < 90 * 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 36 * 3600 {
        format!("{} h ago", seconds / 3600)
    } else {
        format!("{} d ago", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_buckets() {
        assert_eq!(humanize(Duration::from_secs(10)), "just now");
        assert_eq!(humanize(Duration::from_secs(600)), "10 min ago");
        assert_eq!(humanize(Duration::from_secs(3 * 3600)), "3 h ago");
        assert_eq!(humanize(Duration::from_secs(3 * 86_400)), "3 d ago");
    }

    #[test]
    fn next_in_moves_relative_to_cursor() {
        let lines = [3, 8, 12];
        assert_eq!(next_in(&lines, 0, true), Some(3));
        assert_eq!(next_in(&lines, 3, true), Some(8));
        assert_eq!(next_in(&lines, 12, true), None);
        assert_eq!(next_in(&lines, 12, false), Some(8));
        assert_eq!(next_in(&lines, 3, false), None);
    }
}
