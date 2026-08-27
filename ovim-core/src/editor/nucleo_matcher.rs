//! Wrapper around the `nucleo` crate for parallel fuzzy matching.
//!
//! Used by FindFiles mode where the dataset is large (thousands of files).
//! The matcher runs on a background threadpool and never blocks the UI thread.

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo, Utf32String};
use std::sync::Arc;

/// Wraps `Nucleo<u32>` with a clean API for the picker.
///
/// `T = u32`: each item stores an index into the picker's `all_results` vec.
/// Nucleo only holds the display string for matching; actual `PickerResult`
/// data stays on the `Picker`.
pub struct NucleoMatcher {
    nucleo: Nucleo<u32>,
    /// Cached injector — `Nucleo::injector()` clones internal state, so
    /// grab it once instead of once per pushed item.
    injector: nucleo::Injector<u32>,
    last_query: String,
}

impl Default for NucleoMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl NucleoMatcher {
    /// Creates a new matcher with 1 column and automatic thread count.
    pub fn new() -> Self {
        let nucleo = Nucleo::new(
            Config::DEFAULT.match_paths(),
            Arc::new(|| {}),
            None, // auto thread count
            1,    // single column: the display string
        );
        let injector = nucleo.injector();
        Self {
            nucleo,
            injector,
            last_query: String::new(),
        }
    }

    /// Returns a cloneable `Injector` for pushing items from any thread.
    pub fn injector(&self) -> nucleo::Injector<u32> {
        self.injector.clone()
    }

    /// Pushes an item with its index and display text.
    pub fn inject(&self, index: u32, display: &str) {
        self.injector.push(index, |_data, cols| {
            cols[0] = Utf32String::from(display);
        });
    }

    /// Updates the search pattern. Call this when the query changes.
    /// This is instant and non-blocking — matching happens in background.
    pub fn update_query(&mut self, query: &str) {
        let append = query.starts_with(&self.last_query) && !self.last_query.is_empty();
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Ignore, Normalization::Smart, append);
        self.last_query = query.to_string();
    }

    /// Drives the matcher forward. Returns `true` if results changed.
    /// Call this regularly from the event loop (e.g., every tick).
    pub fn tick(&mut self) -> bool {
        let status = self.nucleo.tick(0); // non-blocking: just poll for ready results
        status.changed
    }

    /// Returns the number of items that match the current pattern.
    pub fn matched_count(&self) -> u32 {
        self.nucleo.snapshot().matched_item_count()
    }

    /// Returns the total number of injected items.
    pub fn total_count(&self) -> u32 {
        self.nucleo.snapshot().item_count()
    }

    /// Returns rank-ordered indices for items in the given range.
    /// The indices refer to positions in `Picker::all_results`.
    pub fn matched_indices(&self, count: usize) -> Vec<u32> {
        let snapshot = self.nucleo.snapshot();
        let matched = snapshot.matched_item_count() as usize;
        let take = count.min(matched);
        if take == 0 {
            return Vec::new();
        }
        snapshot
            .matched_items(0..take as u32)
            .map(|item| *item.data)
            .collect()
    }

    /// Returns the index (into `Picker::all_results`) of the item at the given rank.
    /// O(1) lookup — no allocation. Returns `None` if rank is out of bounds.
    pub fn get_item_at_rank(&self, rank: u32) -> Option<u32> {
        let snapshot = self.nucleo.snapshot();
        if rank >= snapshot.matched_item_count() {
            return None;
        }
        snapshot
            .matched_items(rank..rank + 1)
            .next()
            .map(|item| *item.data)
    }

    /// Returns rank-ordered indices for a visible range, using a single snapshot.
    /// Much cheaper than calling `get_item_at_rank()` per item during render.
    pub fn get_items_in_range(&self, start: u32, count: u32) -> Vec<u32> {
        let snapshot = self.nucleo.snapshot();
        let matched = snapshot.matched_item_count();
        if start >= matched {
            return Vec::new();
        }
        let end = (start + count).min(matched);
        snapshot
            .matched_items(start..end)
            .map(|item| *item.data)
            .collect()
    }

    /// Returns true if the pattern is empty (all items match).
    pub fn is_empty_pattern(&self) -> bool {
        self.nucleo.pattern.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_until(
        matcher: &mut NucleoMatcher,
        description: &str,
        condition: impl Fn(&NucleoMatcher) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(matcher) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Nucleo to {description}"
            );
            std::thread::sleep(Duration::from_millis(1));
            matcher.tick();
        }
    }

    #[test]
    fn test_basic_matching() {
        let mut matcher = NucleoMatcher::new();

        matcher.inject(0, "src/main.rs");
        matcher.inject(1, "src/lib.rs");
        matcher.inject(2, "Cargo.toml");

        wait_until(&mut matcher, "process injected items", |matcher| {
            matcher.total_count() == 3 && matcher.matched_count() == 3
        });

        // Empty query matches all
        assert_eq!(matcher.matched_count(), 3);
        assert_eq!(matcher.total_count(), 3);
    }

    #[test]
    fn test_query_filtering() {
        let mut matcher = NucleoMatcher::new();

        matcher.inject(0, "src/main.rs");
        matcher.inject(1, "src/lib.rs");
        matcher.inject(2, "Cargo.toml");

        wait_until(&mut matcher, "process injected items", |matcher| {
            matcher.total_count() == 3 && matcher.matched_count() == 3
        });

        matcher.update_query("main");

        wait_until(&mut matcher, "apply the query", |matcher| {
            matcher.matched_count() == 1
        });

        assert_eq!(matcher.matched_indices(10), vec![0]);
    }

    #[test]
    fn test_empty_pattern_detection() {
        let mut matcher = NucleoMatcher::new();
        assert!(matcher.is_empty_pattern());

        matcher.update_query("foo");
        assert!(!matcher.is_empty_pattern());

        matcher.update_query("");
        assert!(matcher.is_empty_pattern());
    }
}
