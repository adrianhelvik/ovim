//! Per-line rendering cache.
//!
//! Caches the expensive per-line rendering output (tab expansion, horizontal
//! viewport slicing, highlight computation) to avoid recomputation when only
//! the cursor moves. The cache is invalidated when the buffer content changes,
//! the viewport shifts, or the window resizes.

use ratatui::text::Line;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ChatBubbleCacheKey {
    pub conversation_id: u64,
    pub node_id: u64,
    pub panel_width: usize,
    pub selected: bool,
    pub allow_edits: bool,
    pub thinking_expanded: bool,
    pub child_count: usize,
    pub branch_position: Option<(usize, usize)>,
    pub theme_hash: u64,
    pub terminal_image_support: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedChatImage {
    pub row: usize,
    pub x: u16,
    pub width: u16,
    pub height: u16,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedChatBubble {
    pub lines: Vec<Line<'static>>,
    pub images: Vec<CachedChatImage>,
}

/// Key identifying a cached rendered line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LineCacheKey {
    /// Stable buffer identity (`Buffer::id`). Must NOT be the buffer index:
    /// two different buffers can occupy the same index over time (e.g. a
    /// walkthrough replacing its presentation buffer), and both start at
    /// version 0, so index+version cannot tell them apart.
    buffer_id: u64,
    /// Logical line index in the buffer
    line_idx: usize,
    /// Buffer version when this line was rendered
    buffer_version: usize,
    /// Horizontal scroll offset (display columns)
    h_offset: usize,
    /// Available text width (columns)
    text_width: usize,
    /// Whether wrap mode was enabled
    wrap: bool,
    /// Tab width setting
    tab_width: usize,
    /// Whether markdown conceal was active for this render
    markdown_conceal: bool,
    /// Per-line decoration fingerprint. Replaces the previous global
    /// `decoration_generation` so an LSP push that touches one line doesn't
    /// invalidate the cached render for every other stable line. Computed
    /// from the projected decorations on this line via
    /// `ProjectedDecorations::line_hash`.
    decoration_hash: u64,
}

/// A cached rendered line (before soft-wrap splitting).
#[derive(Debug, Clone)]
struct CachedLine {
    /// The rendered styled line (pre-wrap)
    line: Line<'static>,
    /// Whether this line had any special highlighting when cached
    /// (visual selection, search, cursorline, yank flash).
    /// Lines with transient highlighting are NOT cached because
    /// they change every frame.
    is_stable: bool,
}

/// Per-line rendering cache that avoids recomputing expensive rendering
/// for unchanged lines.
///
/// # Invalidation strategy
///
/// - **Buffer edit**: The entire cache is cleared when `buffer_version` changes.
///   Fine-grained per-line invalidation would require tracking which lines
///   shifted, which isn't worth the complexity for a first pass.
/// - **Scroll/resize**: Cleared when `h_offset` or `text_width` changes
///   (detected via the key including these values).
/// - **Cursor move**: Only the cursor line and previous cursor line are
///   excluded from caching (they have transient cursorline highlighting).
/// - **Visual selection / search / yank flash**: Lines with these overlays
///   are rendered fresh each frame (marked `is_stable: false`).
pub struct LineRenderCache {
    entries: HashMap<usize, (LineCacheKey, CachedLine)>,
    chat_bubbles: HashMap<ChatBubbleCacheKey, CachedChatBubble>,
    /// Buffer version from the last render pass
    last_buffer_version: usize,
    /// Stable buffer identity from the last render pass. A buffer swap keeps
    /// the version at 0 (fresh buffers), so identity must be checked too.
    last_buffer_id: u64,
    /// Capacity limit to prevent unbounded growth
    max_entries: usize,
    /// Stats: cache hits this frame
    pub hits: usize,
    /// Stats: cache misses this frame
    pub misses: usize,
    pub chat_hits: usize,
    pub chat_misses: usize,
}

impl Default for LineRenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LineRenderCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(256),
            chat_bubbles: HashMap::with_capacity(128),
            last_buffer_version: usize::MAX, // force miss on first frame
            last_buffer_id: u64::MAX,
            max_entries: 1024,
            hits: 0,
            misses: 0,
            chat_hits: 0,
            chat_misses: 0,
        }
    }

    /// Clear the entire cache (e.g., on buffer edit or resize).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Reset per-frame stats.
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.chat_hits = 0;
        self.chat_misses = 0;
    }

    pub(crate) fn get_chat_bubble(&mut self, key: &ChatBubbleCacheKey) -> Option<CachedChatBubble> {
        let cached = self.chat_bubbles.get(key).cloned();
        if cached.is_some() {
            self.chat_hits += 1;
        } else {
            self.chat_misses += 1;
        }
        cached
    }

    pub(crate) fn insert_chat_bubble(&mut self, key: ChatBubbleCacheKey, bubble: CachedChatBubble) {
        const MAX_CHAT_BUBBLES: usize = 512;
        if self.chat_bubbles.len() >= MAX_CHAT_BUBBLES {
            self.chat_bubbles.clear();
        }
        self.chat_bubbles.insert(key, bubble);
    }

    /// Check if a rendered line is cached and still valid.
    ///
    /// Returns `None` if:
    /// - The line was never cached
    /// - The buffer version changed since it was cached
    /// - The viewport parameters changed
    /// - The cached entry had transient highlighting
    pub fn get(
        &mut self,
        buffer_id: u64,
        line_idx: usize,
        buffer_version: usize,
        h_offset: usize,
        text_width: usize,
        wrap: bool,
        tab_width: usize,
        markdown_conceal: bool,
        decoration_hash: u64,
    ) -> Option<&Line<'static>> {
        // Fast path: if the buffer identity or version changed, invalidate
        // everything. Version alone is not enough: replacing a buffer with a
        // freshly created one (both at version 0) must not reuse old lines.
        if buffer_version != self.last_buffer_version || buffer_id != self.last_buffer_id {
            self.clear();
            self.last_buffer_version = buffer_version;
            self.last_buffer_id = buffer_id;
            self.misses += 1;
            return None;
        }

        let key = LineCacheKey {
            buffer_id,
            line_idx,
            buffer_version,
            h_offset,
            text_width,
            wrap,
            tab_width,
            markdown_conceal,
            decoration_hash,
        };

        if let Some((cached_key, cached)) = self.entries.get(&line_idx) {
            if *cached_key == key && cached.is_stable {
                self.hits += 1;
                return Some(&cached.line);
            }
        }
        self.misses += 1;
        None
    }

    /// Store a rendered line in the cache.
    ///
    /// `is_stable` should be `false` for lines with transient overlays
    /// (cursor line, visual selection, search highlights, yank flash).
    pub fn put(
        &mut self,
        buffer_id: u64,
        line_idx: usize,
        buffer_version: usize,
        h_offset: usize,
        text_width: usize,
        wrap: bool,
        tab_width: usize,
        markdown_conceal: bool,
        decoration_hash: u64,
        line: Line<'static>,
        is_stable: bool,
    ) {
        // Evict if over capacity — keep entries near the current viewport
        // instead of clearing everything (which causes a full cache-miss storm
        // on the next frame).
        if self.entries.len() >= self.max_entries {
            let center = line_idx;
            let keep_radius = self.max_entries / 2;
            let lo = center.saturating_sub(keep_radius);
            let hi = center.saturating_add(keep_radius);
            self.entries.retain(|&idx, _| idx >= lo && idx <= hi);
        }

        let key = LineCacheKey {
            buffer_id,
            line_idx,
            buffer_version,
            h_offset,
            text_width,
            wrap,
            tab_width,
            markdown_conceal,
            decoration_hash,
        };
        self.entries
            .insert(line_idx, (key, CachedLine { line, is_stable }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn make_line(text: &str) -> Line<'static> {
        Line::from(vec![Span::raw(text.to_string())])
    }

    #[test]
    fn cache_hit() {
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1; // sync version
        cache.last_buffer_id = 1;
        cache.put(1, 0, 1, 0, 80, false, 4, false, 0, make_line("hello"), true);

        let result = cache.get(1, 0, 1, 0, 80, false, 4, false, 0);
        assert!(result.is_some());
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 0);
    }

    #[test]
    fn cache_miss_version_change() {
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;
        cache.put(1, 0, 1, 0, 80, false, 4, false, 0, make_line("hello"), true);

        // Buffer version changed
        let result = cache.get(1, 0, 2, 0, 80, false, 4, false, 0);
        assert!(result.is_none());
        assert_eq!(cache.misses, 1);
    }

    #[test]
    fn cache_miss_buffer_identity_change() {
        // Two fresh buffers swapped at the same index share version 0; the
        // stable buffer id must still invalidate the cache between them.
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 0;
        cache.last_buffer_id = 1;
        cache.put(1, 0, 0, 0, 80, false, 4, false, 0, make_line("stale"), true);

        let result = cache.get(2, 0, 0, 0, 80, false, 4, false, 0);
        assert!(result.is_none());
        assert!(cache.get(2, 0, 0, 0, 80, false, 4, false, 0).is_none());
    }

    #[test]
    fn cache_miss_viewport_change() {
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;
        cache.put(1, 0, 1, 0, 80, false, 4, false, 0, make_line("hello"), true);

        // h_offset changed
        let result = cache.get(1, 0, 1, 5, 80, false, 4, false, 0);
        assert!(result.is_none());
    }

    #[test]
    fn unstable_lines_not_cached() {
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;
        // Store with is_stable=false (e.g., cursor line)
        cache.put(
            1,
            0,
            1,
            0,
            80,
            false,
            4,
            false,
            0,
            make_line("cursor"),
            false,
        );

        let result = cache.get(1, 0, 1, 0, 80, false, 4, false, 0);
        assert!(result.is_none()); // Should not hit
    }

    #[test]
    fn eviction_keeps_nearby_lines() {
        let mut cache = LineRenderCache::new();
        cache.max_entries = 10; // small cap for test
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;

        // Fill cache with lines 0..10
        for i in 0..10 {
            cache.put(1, i, 1, 0, 80, false, 4, false, 0, make_line("x"), true);
        }
        assert_eq!(cache.entries.len(), 10);

        // Insert line 8 — should evict lines far from 8 (keep 3..13)
        cache.put(1, 8, 1, 0, 80, false, 4, false, 0, make_line("new"), true);

        // Lines near 8 should survive, line 0 should be evicted
        assert!(cache.entries.contains_key(&8));
        assert!(cache.entries.contains_key(&5));
        assert!(!cache.entries.contains_key(&0));
        assert!(!cache.entries.contains_key(&1));
        assert!(!cache.entries.contains_key(&2));
    }

    #[test]
    fn per_line_decoration_hash_isolates_invalidation() {
        // Two lines cached. Only line 0's decoration hash changes — line 1
        // should still hit. Demonstrates that an LSP push touching one line
        // no longer invalidates the entire cache (the regression Fix #6
        // addressed).
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;
        cache.put(1, 0, 1, 0, 80, false, 4, false, 100, make_line("a"), true);
        cache.put(1, 1, 1, 0, 80, false, 4, false, 200, make_line("b"), true);

        // Line 0's hash changed (e.g. new diagnostic).
        assert!(cache.get(1, 0, 1, 0, 80, false, 4, false, 101).is_none());
        // Line 1 is untouched — should still hit with its original hash.
        assert!(cache.get(1, 1, 1, 0, 80, false, 4, false, 200).is_some());
    }

    #[test]
    fn cache_miss_decoration_hash_change() {
        let mut cache = LineRenderCache::new();
        cache.last_buffer_version = 1;
        cache.last_buffer_id = 1;
        cache.put(1, 0, 1, 0, 80, false, 4, false, 42, make_line("x"), true);

        // Same line, different decoration hash — should miss.
        assert!(cache.get(1, 0, 1, 0, 80, false, 4, false, 43).is_none());
        // Same line, same hash — should hit.
        assert!(cache.get(1, 0, 1, 0, 80, false, 4, false, 42).is_some());
    }

    #[test]
    fn stats_reset() {
        let mut cache = LineRenderCache::new();
        cache.hits = 10;
        cache.misses = 5;
        cache.reset_stats();
        assert_eq!(cache.hits, 0);
        assert_eq!(cache.misses, 0);
    }
}
