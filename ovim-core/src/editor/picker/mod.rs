mod backend;
mod constructors;
mod filter;
mod fuzzy_backend;
mod grep_backend;
mod nucleo_backend;
mod result;
mod text_editing;

use backend::PickerBackend;
use fuzzy_backend::FuzzyListKind;
pub use result::{PickerAction, PickerField, PickerMode, PickerResult};

use super::{fuzzy, SingleLineInput};
use std::path::{Path, PathBuf};

pub struct Picker {
    /// Current search query
    pub(super) query: SingleLineInput,
    /// File filter string (for LiveGrep mode)
    pub(super) file_filter: SingleLineInput,
    /// Which input field is currently active
    pub(super) active_field: PickerField,
    /// All available results (unfiltered)
    pub(super) all_results: Vec<PickerResult>,
    /// Filtered results based on query
    pub(super) filtered_results: Vec<PickerResult>,
    /// Currently selected index in filtered_results
    pub(super) selected_index: usize,
    /// Base directory for file search
    pub(super) base_dir: PathBuf,
    /// Preferred directory for ranking (typically the current file's folder)
    pub(super) preferred_dir: PathBuf,
    /// Whether filtering is pending (for debouncing)
    pub(super) pending_filter: bool,
    /// Typed backend owning mode-specific state
    pub(super) backend: PickerBackend,
}

impl Picker {
    /// Starts an in-process grep search, cancelling any previous one.
    pub fn start_grep_search(&mut self) {
        if let PickerBackend::Grep(ref mut g) = self.backend {
            g.start_search(
                self.query.text(),
                &self.base_dir,
                &self.preferred_dir,
                &mut self.all_results,
                &mut self.filtered_results,
                &mut self.selected_index,
            );
        }
    }

    /// Drains grep results from the channel with a 2ms budget.
    /// Returns true if any new results were added.
    pub fn drain_grep_results(&mut self) -> bool {
        if let PickerBackend::Grep(ref mut g) = self.backend {
            g.drain_results(
                self.file_filter.text(),
                &mut self.all_results,
                &mut self.filtered_results,
                &mut self.selected_index,
            )
        } else {
            false
        }
    }

    /// Cancels any in-flight grep search.
    pub fn cancel_grep(&mut self) {
        if let PickerBackend::Grep(ref mut g) = self.backend {
            g.cancel();
        }
    }

    /// Returns whether this picker uses nucleo for matching.
    pub fn uses_nucleo(&self) -> bool {
        matches!(self.backend, PickerBackend::Nucleo(_))
    }

    /// Returns the total number of results (before filtering).
    pub fn all_results_count(&self) -> usize {
        self.all_results.len()
    }

    /// Returns the number of filtered (matched) results.
    pub fn filtered_result_count(&self) -> usize {
        match &self.backend {
            PickerBackend::Nucleo(s) => s.matched_count,
            _ => self.filtered_results.len(),
        }
    }

    /// Pre-fetches visible item indices in a single nucleo snapshot.
    pub fn prefetch_visible_range(&mut self, start: usize, count: usize) {
        if let PickerBackend::Nucleo(ref mut s) = self.backend {
            if s.nucleo.is_empty_pattern() {
                s.ensure_empty_pattern_order(&self.all_results, &self.preferred_dir);
                s.cached_visible_indices = s.get_empty_pattern_items_in_range(start, count);
            } else {
                s.cached_visible_indices = s.nucleo.get_items_in_range(start as u32, count as u32);
            }
            s.cached_visible_start = start;
        }
    }

    /// Returns a reference to the nth filtered result (rank-ordered for nucleo).
    pub fn filtered_result(&self, idx: usize) -> Option<&PickerResult> {
        if let PickerBackend::Nucleo(ref s) = self.backend {
            if s.nucleo.is_empty_pattern() {
                let all_idx = s.get_empty_pattern_item_at_rank(idx)?;
                return self.all_results.get(all_idx as usize);
            }
            if idx >= s.cached_visible_start {
                let cache_idx = idx - s.cached_visible_start;
                if cache_idx < s.cached_visible_indices.len() {
                    let all_idx = s.cached_visible_indices[cache_idx] as usize;
                    return self.all_results.get(all_idx);
                }
            }
            let all_idx = s.nucleo.get_item_at_rank(idx as u32)?;
            self.all_results.get(all_idx as usize)
        } else {
            self.filtered_results.get(idx)
        }
    }

    /// Collects up to `max` rank-ordered filtered results.
    ///
    /// Unlike calling [`Self::filtered_result`] per rank — which takes a fresh
    /// nucleo snapshot per call — this resolves the whole range against a
    /// single snapshot, so serializing large result sets (headless API) stays
    /// linear.
    pub fn collect_filtered_results(&self, max: usize) -> Vec<&PickerResult> {
        let count = self.filtered_result_count().min(max);
        if count == 0 {
            return Vec::new();
        }
        if let PickerBackend::Nucleo(ref s) = self.backend {
            let indices = if s.nucleo.is_empty_pattern() {
                s.get_empty_pattern_items_in_range(0, count)
            } else {
                s.nucleo.get_items_in_range(0, count as u32)
            };
            indices
                .into_iter()
                .filter_map(|idx| self.all_results.get(idx as usize))
                .collect()
        } else {
            self.filtered_results.iter().take(count).collect()
        }
    }

    /// Drives the nucleo matcher forward and updates matched count.
    pub fn tick(&mut self) -> bool {
        if let PickerBackend::Nucleo(ref mut s) = self.backend {
            let changed = s.nucleo.tick();
            if changed {
                s.matched_count = s.nucleo.matched_count() as usize;
                if s.matched_count > 0 {
                    self.selected_index = self.selected_index.min(s.matched_count - 1);
                } else {
                    self.selected_index = 0;
                }
            }
            changed
        } else {
            false
        }
    }

    /// Updates the query and refreshes filtered results
    pub fn set_query(&mut self, query: String) {
        self.query = SingleLineInput::new(query);
        if let PickerBackend::Nucleo(ref mut s) = self.backend {
            s.nucleo.update_query(self.query.text());
            if s.nucleo.is_empty_pattern() {
                s.rebuild_empty_pattern_order(&self.all_results, &self.preferred_dir);
            }
        } else {
            self.apply_filter_internal();
        }
    }

    /// Internal filter logic
    fn apply_filter_internal(&mut self) {
        match &self.backend {
            PickerBackend::Nucleo(_) => {
                unreachable!("apply_filter_internal should not be called for Nucleo backend");
            }
            PickerBackend::FuzzyList(_) => {
                let mut scored_results: Vec<(PickerResult, i32, Vec<usize>)> = self
                    .all_results
                    .iter()
                    .filter_map(|r| {
                        fuzzy::fuzzy_score(self.query.text(), &r.display)
                            .map(|(score, positions)| (r.clone(), score, positions))
                    })
                    .collect();

                scored_results.sort_by_key(|(_, score, _)| std::cmp::Reverse(*score));

                self.filtered_results = scored_results
                    .into_iter()
                    .map(|(mut result, _score, positions)| {
                        result.match_positions = positions;
                        result
                    })
                    .collect();
            }
            PickerBackend::Grep(_) => {
                self.start_grep_search();
                self.pending_filter = false;
                return;
            }
        }

        self.selected_index = 0;
        self.pending_filter = false;
    }

    /// Marks that filtering is pending (query changed but not yet filtered).
    pub fn mark_filter_pending(&mut self) {
        enum Action {
            UpdateNucleoQuery,
            SetPendingFilter,
            ApplyFileFilter,
            None,
        }

        let action = match &self.backend {
            PickerBackend::Nucleo(_) => Action::UpdateNucleoQuery,
            PickerBackend::Grep(g) => {
                if self.query.text() != g.last_grep_query {
                    Action::SetPendingFilter
                } else if self.file_filter.text() != g.last_filtered_file_filter {
                    Action::ApplyFileFilter
                } else {
                    Action::None
                }
            }
            PickerBackend::FuzzyList(_) => Action::SetPendingFilter,
        };

        match action {
            Action::UpdateNucleoQuery => {
                if let PickerBackend::Nucleo(s) = &mut self.backend {
                    s.nucleo.update_query(self.query.text());
                }
            }
            Action::SetPendingFilter => {
                self.pending_filter = true;
            }
            Action::ApplyFileFilter => {
                if let PickerBackend::Grep(g) = &mut self.backend {
                    g.last_filtered_file_filter = self.file_filter.text().to_owned();
                }
                filter::apply_file_filter_to(
                    self.file_filter.text(),
                    &self.all_results,
                    &mut self.filtered_results,
                    &mut self.selected_index,
                );
            }
            Action::None => {}
        }
    }

    /// Returns true if there's a pending filter operation
    pub fn has_pending_filter(&self) -> bool {
        self.pending_filter
    }

    /// Applies the pending filter if query has changed since last filter
    pub fn apply_pending_filter(&mut self) {
        if self.pending_filter {
            self.apply_filter_internal();
        }
    }

    /// Moves selection down with wraparound (last → first). Mirrors the
    /// inline completion popup (`completion.rs::select_next`) so picker
    /// navigation is consistent with the rest of the editor (OV-00255).
    pub fn move_down(&mut self) {
        let count = self.filtered_result_count();
        if count > 0 {
            self.selected_index = (self.selected_index + 1) % count;
        }
    }

    /// Moves selection down by n items. Page-wise motion clamps at the
    /// bottom rather than wrapping — vim's PageDown / Ctrl-D never wrap
    /// and users don't expect them to.
    pub fn move_down_n(&mut self, n: usize) {
        let count = self.filtered_result_count();
        if count > 0 {
            self.selected_index = (self.selected_index + n).min(count - 1);
        }
    }

    /// Moves selection up with wraparound (first → last). See
    /// [`Self::move_down`] for rationale (OV-00255).
    pub fn move_up(&mut self) {
        let count = self.filtered_result_count();
        if count > 0 {
            self.selected_index = if self.selected_index == 0 {
                count - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    /// Moves selection up by n items. Like [`Self::move_down_n`], page-wise
    /// motion does not wrap.
    pub fn move_up_n(&mut self, n: usize) {
        self.selected_index = self.selected_index.saturating_sub(n);
    }

    /// Gets the currently selected result
    pub fn selected_result(&self) -> Option<&PickerResult> {
        self.filtered_result(self.selected_index)
    }

    /// Derives the action to execute for the currently selected result.
    pub fn selected_action(&self) -> Option<PickerAction> {
        let result = self.selected_result()?;
        match &self.backend {
            PickerBackend::FuzzyList(FuzzyListKind::Custom) => {
                Some(PickerAction::ApplyCodeAction { index: result.line })
            }
            PickerBackend::FuzzyList(FuzzyListKind::Completion) => {
                Some(PickerAction::ApplyCompletion { index: result.line })
            }
            PickerBackend::FuzzyList(FuzzyListKind::LspLocations) => {
                Some(PickerAction::OpenFileWithTag {
                    path: result.location.clone(),
                    line: result.line,
                    col: result.col,
                })
            }
            PickerBackend::FuzzyList(FuzzyListKind::DebugConfig) => {
                Some(PickerAction::SelectDebugConfig { index: result.line })
            }
            PickerBackend::Nucleo(_) | PickerBackend::Grep(_) => Some(PickerAction::OpenFile {
                path: result.location.clone(),
                line: result.line,
                col: result.col,
            }),
        }
    }

    /// Gets the current query
    pub fn query(&self) -> &str {
        self.query.text()
    }

    /// Gets the query cursor position as a UTF-8 byte offset.
    pub fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    /// Gets filtered results
    pub fn filtered_results(&self) -> &[PickerResult] {
        &self.filtered_results
    }

    /// Gets selected index
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Gets picker mode (derived from backend variant)
    pub fn mode(&self) -> &PickerMode {
        match &self.backend {
            PickerBackend::Nucleo(_) => &PickerMode::FindFiles,
            PickerBackend::Grep(_) => &PickerMode::LiveGrep,
            PickerBackend::FuzzyList(kind) => match kind {
                FuzzyListKind::Custom | FuzzyListKind::DebugConfig => &PickerMode::Custom,
                FuzzyListKind::Completion => &PickerMode::Completion,
                FuzzyListKind::LspLocations => &PickerMode::LspLocations,
            },
        }
    }

    /// Gets the base directory for file operations
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Gets the preferred directory for ranking (typically the current file's folder)
    pub fn preferred_dir(&self) -> &Path {
        &self.preferred_dir
    }

    /// Returns true if this picker mode supports the file filter field
    pub fn has_file_filter(&self) -> bool {
        matches!(self.backend, PickerBackend::Grep(_))
    }

    /// Switches the active input field (only for modes with file filter)
    pub fn toggle_field(&mut self) {
        if self.has_file_filter() {
            self.active_field = match self.active_field {
                PickerField::Query => PickerField::FileFilter,
                PickerField::FileFilter => PickerField::Query,
            };
        }
    }

    /// Gets the current file filter string
    pub fn file_filter(&self) -> &str {
        self.file_filter.text()
    }

    /// Gets the file filter cursor position as a UTF-8 byte offset.
    pub fn file_filter_cursor(&self) -> usize {
        self.file_filter.cursor()
    }

    /// Gets the currently active field
    pub fn active_field(&self) -> PickerField {
        self.active_field
    }

    /// Sets the active input field (for mouse clicks)
    pub fn set_active_field(&mut self, field: PickerField) {
        if field == PickerField::FileFilter && !self.has_file_filter() {
            return;
        }
        self.active_field = field;
    }

    /// Sets the selected index (for mouse clicks), clamped to valid range
    pub fn set_selected_index(&mut self, index: usize) {
        let count = self.filtered_result_count();
        if count > 0 {
            self.selected_index = index.min(count - 1);
        }
    }

    /// Adds a file result (for incremental loading)
    pub fn add_file_result(&mut self, result: PickerResult) {
        self.add_file_results(vec![result]);
    }

    /// Adds a batch of file results (for incremental loading).
    ///
    /// Batching matters: file discovery streams tens of thousands of results,
    /// and per-item channel sends plus per-item bookkeeping dominated Find
    /// Files load time before this existed (OV perf work, v1.2.6).
    pub fn add_file_results(&mut self, results: Vec<PickerResult>) {
        if results.is_empty() {
            return;
        }

        match self.backend {
            PickerBackend::Nucleo(ref mut s) => {
                // The pattern can't change mid-batch; hoist the check.
                let empty_pattern = s.nucleo.is_empty_pattern();
                self.all_results.reserve(results.len());
                for result in results {
                    let idx = self.all_results.len() as u32;
                    match nucleo_match_text_for(&self.base_dir, &self.preferred_dir, &result) {
                        Some(match_text) => s.nucleo.inject(idx, &match_text),
                        None => s.nucleo.inject(idx, &result.display),
                    }
                    if empty_pattern {
                        s.push_empty_pattern_item(idx, &result, &self.preferred_dir);
                    }
                    self.all_results.push(result);
                }
            }
            _ => {
                for result in results {
                    self.all_results.push(result.clone());
                    if self.query.is_empty() {
                        self.filtered_results.push(result);
                    } else if fuzzy::fuzzy_score(self.query.text(), &result.display).is_some() {
                        self.filtered_results.push(result);
                        self.pending_filter = true;
                    }
                }
            }
        }
    }

    /// Marks file loading as complete
    pub fn finish_loading(&mut self) {
        if let PickerBackend::Nucleo(ref mut s) = self.backend {
            s.loading = false;
        }
    }

    /// Returns whether files are still being loaded
    pub fn is_loading(&self) -> bool {
        match &self.backend {
            PickerBackend::Nucleo(s) => s.loading,
            PickerBackend::Grep(g) => g.loading,
            PickerBackend::FuzzyList(_) => false,
        }
    }

    /// Returns whether file loading should be spawned
    pub fn should_spawn_file_loading(&self) -> bool {
        if let PickerBackend::Nucleo(ref s) = self.backend {
            s.loading && !s.loading_spawned
        } else {
            false
        }
    }

    /// Marks file loading as spawned
    pub fn mark_loading_spawned(&mut self) {
        if let PickerBackend::Nucleo(ref mut s) = self.backend {
            s.loading_spawned = true;
        }
    }

    /// Truncates a path in the middle if it's too long
    pub fn truncate_path(path: &str, max_len: usize) -> String {
        filter::truncate_path(path, max_len)
    }
}

/// Text nucleo should match against for `result`. Returns `None` when the
/// plain display string should be used (avoids an allocation per file in the
/// common single-root case).
fn nucleo_match_text_for(
    base_dir: &Path,
    preferred_dir: &Path,
    result: &PickerResult,
) -> Option<String> {
    if preferred_dir == base_dir {
        return None;
    }

    let abs = std::path::Path::new(&result.location);
    if let Ok(preferred_rel) = abs.strip_prefix(preferred_dir) {
        // Prepend a preferred-dir-relative path to boost local results, but keep
        // the base-relative display path searchable as well.
        let preferred_rel = preferred_rel.to_string_lossy();
        if preferred_rel.is_empty() {
            None
        } else {
            Some(format!("{} {}", preferred_rel, result.display))
        }
    } else if abs.strip_prefix(base_dir).is_ok() {
        // Base-relative path is exactly the display string.
        None
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
