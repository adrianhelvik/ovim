use super::super::nucleo_matcher::NucleoMatcher;
use super::PickerResult;
use std::path::Path;

/// State for the FindFiles picker mode using nucleo parallel fuzzy matching.
pub struct NucleoState {
    pub nucleo: NucleoMatcher,
    pub matched_count: usize,
    pub cached_visible_indices: Vec<u32>,
    pub cached_visible_start: usize,
    pub loading: bool,
    pub loading_spawned: bool,
    /// Identifies the file-discovery walk feeding this picker. Batches from a
    /// previous picker's still-running walk carry a different id and are
    /// dropped instead of polluting this picker with stale entries.
    pub walk_id: Option<u64>,
    pub empty_pattern_local: Vec<u32>,
    pub empty_pattern_other: Vec<u32>,
    pub empty_pattern_built_for_len: usize,
}

impl NucleoState {
    pub fn new() -> Self {
        Self {
            nucleo: NucleoMatcher::new(),
            matched_count: 0,
            cached_visible_indices: Vec::new(),
            cached_visible_start: 0,
            loading: true,
            loading_spawned: false,
            walk_id: None,
            empty_pattern_local: Vec::new(),
            empty_pattern_other: Vec::new(),
            empty_pattern_built_for_len: 0,
        }
    }

    pub fn ensure_empty_pattern_order(
        &mut self,
        all_results: &[PickerResult],
        preferred_dir: &Path,
    ) {
        if self.empty_pattern_built_for_len != all_results.len() {
            self.rebuild_empty_pattern_order(all_results, preferred_dir);
        }
    }

    pub fn rebuild_empty_pattern_order(
        &mut self,
        all_results: &[PickerResult],
        preferred_dir: &Path,
    ) {
        self.empty_pattern_local.clear();
        self.empty_pattern_other.clear();
        for (idx, r) in all_results.iter().enumerate() {
            let idx = idx as u32;
            self.push_empty_pattern_item(idx, r, preferred_dir);
        }
        self.empty_pattern_built_for_len = all_results.len();
    }

    pub fn push_empty_pattern_item(
        &mut self,
        idx: u32,
        result: &PickerResult,
        preferred_dir: &Path,
    ) {
        let abs = Path::new(&result.location);
        if abs.starts_with(preferred_dir) {
            self.empty_pattern_local.push(idx);
        } else {
            self.empty_pattern_other.push(idx);
        }
        self.empty_pattern_built_for_len = (idx as usize) + 1;
    }

    pub fn get_empty_pattern_item_at_rank(&self, rank: usize) -> Option<u32> {
        if rank < self.empty_pattern_local.len() {
            return Some(self.empty_pattern_local[rank]);
        }
        let other_rank = rank - self.empty_pattern_local.len();
        self.empty_pattern_other.get(other_rank).copied()
    }

    pub fn get_empty_pattern_items_in_range(&self, start: usize, count: usize) -> Vec<u32> {
        let total = self.empty_pattern_local.len() + self.empty_pattern_other.len();
        if start >= total || count == 0 {
            return Vec::new();
        }
        let end = (start + count).min(total);
        let mut out = Vec::with_capacity(end - start);
        for rank in start..end {
            if let Some(idx) = self.get_empty_pattern_item_at_rank(rank) {
                out.push(idx);
            }
        }
        out
    }
}
