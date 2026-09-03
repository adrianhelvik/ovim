//! Branch diff review.
//!
//! `<Space>gd` (or `:GitDiff`) opens a read-only patch of everything the
//! current branch changed relative to the default branch, including
//! uncommitted work. Each hunk is coloured with the grammar of the file it
//! came from — the way `delta` renders a patch — rather than with a diff
//! grammar, so the review reads like code. Inside the review:
//!
//! - `s` (or a click on the toolbar) switches between the unified and the
//!   side-by-side layout
//! - `]c` / `[c` move between hunks, `]f` / `[f` between files
//! - `Enter` (or `gf`) opens the file at the line under the cursor in the tab
//!   the review was opened from; `<Space>gd` returns to the review, refreshed
//! - `r` refreshes, `q` closes, `<Space>gf` fetches the base branch
//!
//! The base is chosen by [`crate::native_diff::resolve_base`], which prefers
//! whichever of `main` / `origin/main` has the most recent merge-base with
//! HEAD so a stale local or remote copy never pollutes the review.

mod highlight;
mod render;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use super::{Editor, ToastLevel, ToastRequest, ToastSource};
use crate::buffer::{Buffer, BufferId};
use crate::native_diff::{self, PatchLineKind, ReviewBase, ReviewPatch};
use crate::unicode::{grapheme_index_for_byte, GraphemeCol};

pub use render::{DiffLayout, DIFF_REVIEW_TITLE_PREFIX};

use render::{
    layout_body, summary_message, Rendered, ReviewCell, ReviewRow, Toolbar, DEFAULT_LAYOUT_WIDTH,
};

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
    pub layout: DiffLayout,
    /// The patch the buffer was rendered from, kept so switching layout or
    /// re-flowing after a resize costs no Git work.
    patch: ReviewPatch,
    /// Text width the side-by-side layout was laid out for.
    layout_width: usize,
    /// Buffer-area width that produced `layout_width`. Re-flowing keys off
    /// this rather than off the text width, which also moves when the line
    /// number gutter grows — that would oscillate.
    layout_area_width: usize,
    /// Source mapping per buffer line.
    rows: Vec<ReviewRow>,
    /// `(buffer line, file index)` for each row of the file summary.
    stat_rows: Vec<(usize, usize)>,
    /// Buffer lines of hunk headers.
    hunk_lines: Vec<usize>,
    /// Buffer line of the first header line of each file, indexed by file.
    file_lines: Vec<usize>,
    /// The same lines in buffer order, for `]f` / `[f`.
    file_nav: Vec<usize>,
    /// The clickable layout switch.
    toolbar: Toolbar,
    /// Buffer text split into lines (for refresh anchoring).
    text_lines: Vec<String>,
    /// Byte range of each patch line inside `patch.text`, for mapping a
    /// cursor column back to a column in the source file. Ranges rather than
    /// owned lines: a review can hold a hundred thousand of them.
    patch_line_ranges: Vec<(usize, usize)>,
}

impl DiffReviewState {
    pub fn root(&self) -> &Path {
        &self.patch.root
    }

    pub fn base(&self) -> &ReviewBase {
        &self.patch.base
    }

    fn row(&self, line: usize) -> Option<&ReviewRow> {
        self.rows.get(line)
    }

    fn file_for_stat_row(&self, line: usize) -> Option<usize> {
        self.stat_rows
            .iter()
            .find(|(row, _)| *row == line)
            .map(|(_, file)| *file)
    }

    fn info(&self, line: usize) -> Option<crate::native_diff::PatchLine> {
        self.row(line).and_then(ReviewRow::info)
    }

    /// Best `(file index, 1-based line)` source target for a buffer line.
    fn source_target(&self, line: usize) -> Option<(usize, usize)> {
        self.cell_target(line, self.info(line)?)
    }

    fn cell_target(
        &self,
        line: usize,
        info: crate::native_diff::PatchLine,
    ) -> Option<(usize, usize)> {
        let file = info.file?;
        if let Some(new_line) = info.new_line {
            return Some((file, new_line));
        }
        // File headers and meta lines: use the first hunk that follows.
        let next_hunk = self
            .rows
            .iter()
            .skip(line + 1)
            .map_while(ReviewRow::info)
            .find(|entry| entry.file == Some(file) && entry.kind == PatchLineKind::HunkHeader)
            .and_then(|entry| entry.new_line);
        Some((file, next_hunk.unwrap_or(1)))
    }

    /// Where the cursor is, in source terms, so a re-render can restore it.
    fn anchor(&self, line: usize, text: Option<String>) -> Option<Anchor> {
        let target = self
            .source_target(line)
            .or_else(|| self.file_for_stat_row(line).map(|file| (file, 1)))?;
        let path = self.patch.files.get(target.0)?.path.clone();
        Some(Anchor {
            path,
            new_line: target.1,
            text: text.filter(|_| self.info(line).is_some()),
        })
    }

    /// Buffer line to restore after a re-render: the same patch line if its
    /// text still exists in that file (nearest to the old position), else the
    /// first line at or after the old source line, else the file header.
    fn line_for_anchor(&self, anchor: &Anchor) -> Option<usize> {
        let file = self
            .patch
            .files
            .iter()
            .position(|entry| entry.path == anchor.path)?;
        let candidates = self.rows.iter().enumerate().filter_map(|(line, row)| {
            let entry = row.info()?;
            (entry.file == Some(file) && entry.kind != PatchLineKind::FileHeader)
                .then_some((line, entry.new_line?))
        });

        if let Some(text) = &anchor.text {
            let same_text = candidates
                .clone()
                .filter(|(line, _)| self.line_text(*line) == Some(text.as_str()))
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

    /// The `(byte range, added)` tints for a rendered line, so the frontend
    /// can paint the added/removed background the way `delta` does.
    pub fn line_tints(&self, line: usize) -> Vec<(std::ops::Range<usize>, bool)> {
        self.row(line)
            .map(|row| row.tints().collect())
            .unwrap_or_default()
    }

    /// Whether a rendered line's tint runs all the way to its end, so the
    /// frontend can carry the band through the padding to the right edge.
    /// `Some(true)` is an addition, `Some(false)` a removal.
    pub fn line_trailing_tint(&self, line: usize) -> Option<bool> {
        let length = self.line_text(line)?.len();
        if length == 0 {
            return None;
        }
        self.row(line)?
            .tints()
            .find(|(range, _)| range.end >= length)
            .map(|(_, added)| added)
    }

    /// The column in the source file a cursor at `col` refers to.
    fn source_column(&self, cell: &ReviewCell, col: usize, tab_width: usize) -> usize {
        if cell.text_len == 0 {
            return 0;
        }
        let Some(body) = self.patch_body(cell.patch_line) else {
            return 0;
        };
        let glyphs = layout_body(body, tab_width, self.layout == DiffLayout::Split);
        if glyphs.is_empty() {
            return 0;
        }
        let offset = col.saturating_sub(cell.text_col).min(cell.text_len - 1);
        let index = (cell.src_glyph + offset).min(glyphs.len() - 1);
        grapheme_index_for_byte(body, glyphs[index].src_byte)
    }

    /// The text of a patch line with its `+`/`-`/space marker stripped.
    fn patch_body(&self, patch_line: usize) -> Option<&str> {
        let (start, end) = *self.patch_line_ranges.get(patch_line)?;
        let text = self.patch.text.get(start..end)?;
        match self.patch.lines.get(patch_line)?.kind {
            PatchLineKind::Added | PatchLineKind::Removed | PatchLineKind::Context => text.get(1..),
            _ => Some(text),
        }
    }
}

/// Source position of the cursor, captured before a re-render.
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

        let layout = self.ui_panels.diff_review_layout;
        let (area_width, width) = self.diff_review_widths();
        let rendered = render::render(
            &patch,
            layout,
            width,
            self.unsaved_buffer_count(),
            self.diff_review_tab_width(),
        );

        let origin_buffer_id = Some(self.buffer().id());
        let origin_tab = self.current_tab_index();
        self.open_diff_buffer_in_new_tab(&rendered.title, &rendered.text);
        let buffer_id = self.buffer().id();

        self.ui_panels.diff_review = Some(DiffReviewState {
            buffer_id,
            origin_buffer_id,
            origin_tab,
            explicit_spec: explicit_spec.map(str::to_string),
            layout,
            layout_width: width,
            layout_area_width: area_width,
            patch_line_ranges: patch_line_ranges(&patch),
            patch,
            rows: Vec::new(),
            stat_rows: Vec::new(),
            hunk_lines: Vec::new(),
            file_lines: Vec::new(),
            file_nav: Vec::new(),
            toolbar: Toolbar::default(),
            text_lines: Vec::new(),
        });
        self.apply_rendered_review(rendered);

        let message = self
            .ui_panels
            .diff_review
            .as_ref()
            .map(|state| summary_message(&state.patch));
        if let Some(message) = message {
            self.set_status_message(message);
        }
        self.mark_dirty();
        Ok(())
    }

    /// Recomputes the patch, keeping the cursor on the same source location.
    pub fn refresh_diff_review(&mut self) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        let (root, base, explicit) = {
            let state = self.ui_panels.diff_review.as_ref().expect("review state");
            (
                state.patch.root.clone(),
                state.patch.base.clone(),
                state.explicit_spec.clone(),
            )
        };

        let base = match &explicit {
            Some(spec) => ReviewBase::explicit(spec),
            None => native_diff::resolve_base(&root).unwrap_or(base),
        };
        let patch = match native_diff::review_patch(&root, &base) {
            Ok(patch) => patch,
            Err(error) => {
                self.review_toast(ToastLevel::Error, format!("Diff review: {error:#}"));
                return;
            }
        };

        let anchor = self.diff_review_anchor(index, true);
        {
            let state = self.ui_panels.diff_review.as_mut().expect("review state");
            state.patch_line_ranges = patch_line_ranges(&patch);
            state.patch = patch;
        }
        self.rerender_diff_review(anchor);

        let message = self
            .ui_panels
            .diff_review
            .as_ref()
            .map(|state| summary_message(&state.patch));
        if let Some(message) = message {
            self.set_status_message(message);
        }
    }

    /// `s` in the review, or a click on the toolbar.
    pub fn toggle_diff_review_layout(&mut self) {
        let next = self
            .ui_panels
            .diff_review
            .as_ref()
            .map(|state| state.layout)
            .unwrap_or(self.ui_panels.diff_review_layout)
            .toggled();
        self.set_diff_review_layout(next);
    }

    /// Switches the review to `layout`, keeping the cursor on the same change.
    /// The choice sticks for reviews opened later in the session.
    pub fn set_diff_review_layout(&mut self, layout: DiffLayout) {
        self.ui_panels.diff_review_layout = layout;
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        if self
            .ui_panels
            .diff_review
            .as_ref()
            .is_some_and(|state| state.layout == layout)
        {
            return;
        }
        // The rendered line text differs between layouts, so anchor on the
        // source position only.
        let anchor = self.diff_review_anchor(index, false);
        if let Some(state) = self.ui_panels.diff_review.as_mut() {
            state.layout = layout;
        }
        self.rerender_diff_review(anchor);
        self.set_status_message(format!("Diff review: {} view", layout.label()));
    }

    /// Re-flows the side-by-side layout after the window changed width.
    /// Returns true when the buffer was re-rendered.
    pub fn relayout_diff_review(&mut self) -> bool {
        let Some(index) = self.review_buffer_index() else {
            return false;
        };
        let (area_width, width) = self.diff_review_widths();
        // Re-flow on a real resize, and once more if the review's own line
        // number gutter turned out wider than the buffer it replaced. Only
        // shrinking is allowed without a resize, so the two cannot chase each
        // other.
        let visible = index == self.current_buffer_index;
        let stale = self.ui_panels.diff_review.as_ref().is_some_and(|state| {
            state.layout == DiffLayout::Split
                && (state.layout_area_width != area_width
                    || (visible && width < state.layout_width))
        });
        if !stale {
            return false;
        }
        let anchor = self.diff_review_anchor(index, true);
        if let Some(state) = self.ui_panels.diff_review.as_mut() {
            state.layout_width = width;
            state.layout_area_width = area_width;
        }
        self.rerender_diff_review(anchor);
        true
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

    /// A left click inside the review. Returns true when it hit the toolbar
    /// and the caller should not move the cursor.
    pub fn diff_review_click(&mut self, line: usize, col: usize) -> bool {
        let Some(layout) = self
            .ui_panels
            .diff_review
            .as_ref()
            .filter(|state| state.buffer_id == self.buffer().id())
            .and_then(|state| state.toolbar.hit(line, col))
        else {
            return false;
        };
        self.set_diff_review_layout(layout);
        true
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

        let cell = state.row(line).and_then(|row| row.cell_at(col));
        let target = cell.and_then(|cell| state.cell_target(line, cell.info));
        let Some((file_index, new_line)) = target else {
            self.set_status_message("Move to a changed line and press Enter to open it");
            return;
        };
        let file = &state.patch.files[file_index];
        if file.status == "deleted" {
            let path = file.path.clone();
            self.review_toast(
                ToastLevel::Warning,
                format!("{path} was deleted on this branch"),
            );
            return;
        }
        let tab_width = self.diff_review_tab_width();
        let target_col = cell
            .map(|cell| state.source_column(&cell, col, tab_width))
            .unwrap_or(0);
        let path = state.patch.root.join(&file.path);
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
        let target = next_in(&state.file_nav, cursor_line, forward);
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
            Some(state) => Ok(state.patch.base.clone()),
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

    /// Captures where the cursor is, in source terms, before a re-render.
    /// `match_text` anchors on the patch line's text too, which only helps
    /// when the layout stays the same.
    fn diff_review_anchor(&self, index: usize, match_text: bool) -> Option<Anchor> {
        let state = self.ui_panels.diff_review.as_ref()?;
        let cursor_line = self.buffers[index].cursor().line();
        let text = match_text
            .then(|| state.line_text(cursor_line).map(str::to_string))
            .flatten();
        state.anchor(cursor_line, text)
    }

    /// Re-renders the review buffer from the patch already in state.
    fn rerender_diff_review(&mut self, anchor: Option<Anchor>) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        let unsaved = self.unsaved_buffer_count();
        let tab_width = self.diff_review_tab_width();
        let rendered = {
            let state = self.ui_panels.diff_review.as_ref().expect("review state");
            render::render(
                &state.patch,
                state.layout,
                state.layout_width,
                unsaved,
                tab_width,
            )
        };
        self.buffers[index].replace_content(&rendered.text);
        self.buffers[index].set_display_name(rendered.title.clone());
        self.apply_rendered_review(rendered);

        if let Some(anchor) = anchor {
            let target = self
                .ui_panels
                .diff_review
                .as_ref()
                .and_then(|state| state.line_for_anchor(&anchor));
            if let Some(line) = target {
                self.buffers[index]
                    .cursor_mut()
                    .set_position(line, GraphemeCol(0));
            }
        }
        if index == self.current_buffer_index {
            self.buffer_mut().validate_cursor_position();
            self.center_cursor_in_viewport();
        }
        self.mark_dirty();
    }

    /// Installs a fresh render into the state and the review buffer.
    fn apply_rendered_review(&mut self, rendered: Rendered) {
        let Some(index) = self.review_buffer_index() else {
            return;
        };
        self.buffers[index].set_forced_highlights(rendered.highlights);
        let state = self.ui_panels.diff_review.as_mut().expect("review state");
        state.rows = rendered.rows;
        state.stat_rows = rendered.stat_rows;
        state.hunk_lines = rendered.hunk_lines;
        state.file_nav = {
            let mut lines = rendered.file_lines.clone();
            lines.sort_unstable();
            lines.dedup();
            lines
        };
        state.file_lines = rendered.file_lines;
        state.toolbar = rendered.toolbar;
        state.text_lines = rendered.text.lines().map(str::to_string).collect();
    }

    /// `(buffer area width, text width)` the review lays out against.
    fn diff_review_widths(&self) -> (usize, usize) {
        let Some(area) = self.render_cache.last_buffer_area else {
            return (0, DEFAULT_LAYOUT_WIDTH);
        };
        let area_width = area.width as usize;
        let text_width = if self.render_cache.last_text_width > 0 {
            self.render_cache.last_text_width
        } else {
            area_width.saturating_sub(self.render_cache.last_gutter_width)
        };
        // Stop one column short so a full-width row never triggers a soft wrap.
        (area_width, text_width.saturating_sub(1).max(20))
    }

    fn diff_review_tab_width(&self) -> usize {
        self.indent_options().tab_width
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
            return state.patch.root.clone();
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

/// Byte ranges of each line of `patch.text`, in the same order as
/// [`ReviewPatch::lines`].
fn patch_line_ranges(patch: &ReviewPatch) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(patch.lines.len());
    let mut start = 0;
    for (offset, byte) in patch.text.bytes().enumerate() {
        if byte == b'\n' {
            ranges.push((start, offset));
            start = offset + 1;
        }
    }
    if start < patch.text.len() {
        ranges.push((start, patch.text.len()));
    }
    ranges
}

fn next_in(lines: &[usize], cursor_line: usize, forward: bool) -> Option<usize> {
    if forward {
        lines.iter().copied().find(|line| *line > cursor_line)
    } else {
        lines.iter().rev().copied().find(|line| *line < cursor_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
