/// Maps logical lines to visual (wrapped) lines for soft wrap rendering.
///
/// Each logical line may span multiple visual rows when its content
/// exceeds the available width. This structure precomputes the mapping
/// so rendering and scrolling can work in visual-line space.
///
/// Uses [`crate::wrap::visual_line_count`] as the single source of truth
/// for wrap computation, ensuring agreement with the renderer's
/// `split_line_into_rows`.
#[derive(Debug, Clone)]
pub struct WrapMap {
    /// Number of visual lines each logical line occupies (minimum 1)
    visual_counts: Vec<u16>,
    /// Cumulative visual line offset for each logical line (prefix sum)
    /// visual_offsets[i] = total visual lines before line i
    visual_offsets: Vec<usize>,
    /// Total visual lines across all logical lines
    total_visual_lines: usize,
    /// The wrap width used to compute this map
    wrap_width: usize,
    /// Tab width for column calculations
    tab_width: usize,
    /// Buffer version when this map was built (for invalidation)
    buffer_version: usize,
    /// The cursor's logical line at build time, when markdown conceal affects
    /// layout (`Some(line)`), else `None`. The renderer reveals (does not
    /// conceal) the cursor line so editing isn't blind, so that one line keeps
    /// its raw width in the map while every other line is concealed. Moving the
    /// cursor to a different line therefore changes the layout and must
    /// invalidate the map. `None` when conceal is inactive, so plain buffers
    /// never rebuild on vertical cursor movement.
    conceal_cursor_line: Option<usize>,
}

impl WrapMap {
    /// Creates a new WrapMap by computing visual line counts for all lines.
    ///
    /// `line_text` returns the text of a given line index (without trailing newline).
    pub fn new<F>(
        line_count: usize,
        wrap_width: usize,
        tab_width: usize,
        buffer_version: usize,
        line_text: F,
    ) -> Self
    where
        F: Fn(usize) -> String,
    {
        Self::new_with_decorations(
            line_count,
            wrap_width,
            tab_width,
            buffer_version,
            line_text,
            |_| Vec::new(),
        )
    }

    /// Creates a new WrapMap that accounts for inline decoration widths.
    ///
    /// `inline_widths` returns `(char_idx, display_width)` pairs for each line,
    /// representing inline decorations (e.g. inlay hints) that add display width.
    pub fn new_with_decorations<F, D>(
        line_count: usize,
        wrap_width: usize,
        tab_width: usize,
        buffer_version: usize,
        line_text: F,
        inline_widths: D,
    ) -> Self
    where
        F: Fn(usize) -> String,
        D: Fn(usize) -> Vec<(usize, usize)>,
    {
        let width = wrap_width.max(1);
        let mut visual_counts = Vec::with_capacity(line_count);
        let mut visual_offsets = Vec::with_capacity(line_count);
        let mut total = 0;

        for i in 0..line_count {
            visual_offsets.push(total);
            let text = line_text(i);
            let decs = inline_widths(i);
            let count =
                crate::wrap::visual_line_count_with_decorations(&text, width, tab_width, &decs)
                    as u16;
            visual_counts.push(count);
            total += count as usize;
        }

        Self {
            visual_counts,
            visual_offsets,
            total_visual_lines: total,
            wrap_width: width,
            tab_width,
            buffer_version,
            conceal_cursor_line: None,
        }
    }

    /// The cursor line this map was built against for markdown conceal, or
    /// `None` if conceal did not affect layout. See [`set_conceal_cursor_line`].
    pub fn conceal_cursor_line(&self) -> Option<usize> {
        self.conceal_cursor_line
    }

    /// Records which logical line was the (revealed) cursor line when conceal
    /// was applied to the rest of the buffer. Used for invalidation only.
    pub fn set_conceal_cursor_line(&mut self, line: Option<usize>) {
        self.conceal_cursor_line = line;
    }

    /// Returns the buffer version this map was built for
    pub fn buffer_version(&self) -> usize {
        self.buffer_version
    }

    /// Updates the stored buffer version without rebuilding.
    pub fn set_buffer_version(&mut self, version: usize) {
        self.buffer_version = version;
    }

    /// Returns the number of visual lines for a given logical line.
    pub fn visual_lines_for(&self, line: usize) -> u16 {
        self.visual_counts.get(line).copied().unwrap_or(1)
    }

    /// Returns the first visual row index for a given logical line.
    /// For out-of-bounds lines, returns total_visual_lines (one past last row).
    pub fn logical_to_visual(&self, line: usize) -> usize {
        self.visual_offsets
            .get(line)
            .copied()
            .unwrap_or(self.total_visual_lines)
    }

    /// Absolute visual row drawn at the very top of a viewport whose top
    /// logical line is `scroll_offset` with `scroll_subrow` of that line's
    /// wrapped rows hidden above the top edge.
    ///
    /// This is the single source of truth for the viewport's visual-row origin:
    /// `logical_to_visual(scroll_offset) + scroll_subrow`. The buffer renderer
    /// (which skips `scroll_subrow` rows of the top line) and the cursor/overlay
    /// screen-row math must both derive from it, or they drift — omitting the
    /// sub-row term draws the cursor `scroll_subrow` rows too low (OV-00019).
    pub fn viewport_top_visual_row(&self, scroll_offset: usize, scroll_subrow: usize) -> usize {
        self.logical_to_visual(scroll_offset) + scroll_subrow
    }

    /// Converts a visual row index to (logical_line, sub_line) within that line.
    pub fn visual_to_logical(&self, visual_row: usize) -> (usize, usize) {
        // Binary search for the logical line containing this visual row
        let line = match self.visual_offsets.binary_search(&visual_row) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let sub_line =
            visual_row.saturating_sub(self.visual_offsets.get(line).copied().unwrap_or(0));
        (line, sub_line)
    }

    /// Total number of visual lines across all logical lines.
    pub fn total_visual_lines(&self) -> usize {
        self.total_visual_lines
    }

    /// The wrap width this map was computed for.
    pub fn wrap_width(&self) -> usize {
        self.wrap_width
    }

    /// Number of logical lines in this map.
    pub fn line_count(&self) -> usize {
        self.visual_counts.len()
    }

    // NOTE: there used to be an `invalidate_line` here for incremental
    // single-line recomputation. It was removed (OV-00263 / OV-00191) — it had
    // no production callers (the renderer always full-rebuilds via
    // `ensure_wrap_map` → `rebuild_with_decorations`), it ignored inline
    // decoration widths, and its `isize` offset arithmetic could theoretically
    // underflow. Incremental invalidation (OV-00015) should be (re)built
    // decoration-aware from scratch when it's actually wired up.

    /// Rebuild the entire map (e.g., after resize or wrap toggle).
    ///
    /// `line_text` returns the text of a given line index (without trailing newline).
    pub fn rebuild<F>(
        &mut self,
        line_count: usize,
        wrap_width: usize,
        tab_width: usize,
        buffer_version: usize,
        line_text: F,
    ) where
        F: Fn(usize) -> String,
    {
        self.rebuild_with_decorations(
            line_count,
            wrap_width,
            tab_width,
            buffer_version,
            line_text,
            |_| Vec::new(),
        );
    }

    /// Rebuild with inline decoration widths.
    pub fn rebuild_with_decorations<F, D>(
        &mut self,
        line_count: usize,
        wrap_width: usize,
        tab_width: usize,
        buffer_version: usize,
        line_text: F,
        inline_widths: D,
    ) where
        F: Fn(usize) -> String,
        D: Fn(usize) -> Vec<(usize, usize)>,
    {
        let width = wrap_width.max(1);
        self.wrap_width = width;
        self.tab_width = tab_width;
        self.buffer_version = buffer_version;
        self.conceal_cursor_line = None;
        self.visual_counts.clear();
        self.visual_counts.reserve(line_count);
        self.visual_offsets.clear();
        self.visual_offsets.reserve(line_count);
        let mut total = 0;

        for i in 0..line_count {
            self.visual_offsets.push(total);
            let text = line_text(i);
            let decs = inline_widths(i);
            let count =
                crate::wrap::visual_line_count_with_decorations(&text, width, tab_width, &decs)
                    as u16;
            self.visual_counts.push(count);
            total += count as usize;
        }
        self.total_visual_lines = total;
    }

    /// Maps a cursor position (line, display_col) to a visual position (visual_row, visual_col).
    ///
    /// Requires the line text to properly compute wrap points for wide chars.
    pub fn cursor_to_visual(&self, line: usize, col: usize, line_text: &str) -> (usize, usize) {
        self.cursor_to_visual_with_decorations(line, col, line_text, &[])
    }

    /// Like [`cursor_to_visual`] but accounts for inline decoration widths.
    ///
    /// Simulates the same walk as [`crate::wrap::compute_wrap_points_with_decorations`],
    /// adding decoration widths column-by-column so mid-decoration wraps are
    /// tracked correctly.
    ///
    /// `col` is a **flat display column** — the sum of content widths
    /// (characters plus decorations) from the line start, *without* padding
    /// from wide-char pushes. This matches how callers compute it:
    /// `expanded_col + inline_offset`.
    pub fn cursor_to_visual_with_decorations(
        &self,
        line: usize,
        col: usize,
        line_text: &str,
        inline_widths: &[(usize, usize)],
    ) -> (usize, usize) {
        let base_row = self.logical_to_visual(line);
        let (sub_line, row_col) = crate::wrap::visual_position_for_flat_col(
            line_text,
            col,
            self.wrap_width,
            self.tab_width,
            inline_widths,
        );
        (base_row + sub_line, row_col)
    }

    /// Simpler cursor_to_visual that works like the old API when line text isn't available.
    /// Uses simple division — less accurate for lines with wide chars at wrap boundaries.
    pub fn cursor_to_visual_simple(&self, line: usize, col: usize) -> (usize, usize) {
        let base_row = self.logical_to_visual(line);
        let sub_line = col / self.wrap_width;
        let visual_col = col % self.wrap_width;
        (base_row + sub_line, visual_col)
    }

    /// Returns the absolute display-column range for a wrapped sub-line.
    ///
    /// The start and end are in global display columns on the source line, where
    /// `sub_line` is the index of the wrapped visual segment.
    /// Returns `None` if `sub_line` is out of range.
    pub fn sub_line_display_range(
        &self,
        line_text: &str,
        sub_line: usize,
    ) -> Option<(usize, usize)> {
        // Self-contained walk mirroring `compute_wrap_points_with_decorations`
        // (flat tab stops, tabs consumable column-by-column, wide chars pushed
        // whole): row starts are recorded in flat display columns, so a row
        // boundary can fall in the middle of a tab's expanded spaces.
        let max_width = self.wrap_width.max(1);
        let tab_width = self.tab_width.max(1);
        let mut starts = vec![0usize];
        let mut flat_col = 0usize;
        let mut row_col = 0usize;

        for ch in line_text.chars() {
            if ch == '\t' {
                let ch_width = tab_width - (flat_col % tab_width);
                for _ in 0..ch_width {
                    if row_col >= max_width {
                        starts.push(flat_col);
                        row_col = 0;
                    }
                    flat_col += 1;
                    row_col += 1;
                }
            } else {
                let ch_width = crate::display::char_display_width(ch);
                if row_col + ch_width > max_width {
                    starts.push(flat_col);
                    row_col = 0;
                }
                flat_col += ch_width;
                row_col += ch_width;
            }
        }

        if sub_line >= starts.len() {
            return None;
        }

        let start = starts[sub_line];
        let end = if sub_line + 1 < starts.len() {
            starts[sub_line + 1]
        } else {
            flat_col
        };

        Some((start, end))
    }

    /// Counts total visual lines from `start_line` to `end_line` (exclusive).
    pub fn visual_lines_in_range(&self, start_line: usize, end_line: usize) -> usize {
        let start = self.visual_offsets.get(start_line).copied().unwrap_or(0);
        let end = if end_line >= self.visual_counts.len() {
            self.total_visual_lines
        } else {
            self.visual_offsets
                .get(end_line)
                .copied()
                .unwrap_or(self.total_visual_lines)
        };
        end.saturating_sub(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text<'a>(lines: &'a [&'a str]) -> impl Fn(usize) -> String + 'a {
        move |i| {
            if i < lines.len() {
                lines[i].to_string()
            } else {
                String::new()
            }
        }
    }

    #[test]
    fn test_single_line_fits() {
        let map = WrapMap::new(1, 80, 4, 0, make_text(&["a".repeat(40).as_str()]));
        assert_eq!(map.visual_lines_for(0), 1);
        assert_eq!(map.total_visual_lines(), 1);
    }

    #[test]
    fn test_line_exactly_fits() {
        let text = "a".repeat(80);
        let map = WrapMap::new(1, 80, 4, 0, make_text(&[text.as_str()]));
        assert_eq!(map.visual_lines_for(0), 1);
    }

    #[test]
    fn test_line_wraps_once() {
        let text = "a".repeat(81);
        let map = WrapMap::new(1, 80, 4, 0, make_text(&[text.as_str()]));
        assert_eq!(map.visual_lines_for(0), 2);
        assert_eq!(map.total_visual_lines(), 2);
    }

    #[test]
    fn test_empty_line() {
        let map = WrapMap::new(1, 80, 4, 0, make_text(&[""]));
        assert_eq!(map.visual_lines_for(0), 1);
    }

    #[test]
    fn test_multiple_lines() {
        let l0 = "a".repeat(40);
        let l1 = "a".repeat(160);
        let map = WrapMap::new(3, 80, 4, 0, make_text(&[l0.as_str(), l1.as_str(), ""]));
        assert_eq!(map.visual_lines_for(0), 1);
        assert_eq!(map.visual_lines_for(1), 2);
        assert_eq!(map.visual_lines_for(2), 1);
        assert_eq!(map.total_visual_lines(), 4);
    }

    #[test]
    fn test_logical_to_visual() {
        let l0 = "a".repeat(40);
        let l1 = "a".repeat(160);
        let map = WrapMap::new(3, 80, 4, 0, make_text(&[l0.as_str(), l1.as_str(), ""]));
        assert_eq!(map.logical_to_visual(0), 0);
        assert_eq!(map.logical_to_visual(1), 1);
        assert_eq!(map.logical_to_visual(2), 3);
    }

    #[test]
    fn test_visual_to_logical() {
        let l0 = "a".repeat(40);
        let l1 = "a".repeat(160);
        let map = WrapMap::new(3, 80, 4, 0, make_text(&[l0.as_str(), l1.as_str(), ""]));
        assert_eq!(map.visual_to_logical(0), (0, 0));
        assert_eq!(map.visual_to_logical(1), (1, 0));
        assert_eq!(map.visual_to_logical(2), (1, 1));
        assert_eq!(map.visual_to_logical(3), (2, 0));
    }

    #[test]
    fn test_cursor_to_visual_simple() {
        let l0 = "a".repeat(200);
        let l1 = "a".repeat(40);
        let map = WrapMap::new(2, 80, 4, 0, make_text(&[l0.as_str(), l1.as_str()]));
        // Line 0: 200 ASCII chars -> 3 visual lines
        // Cursor at col 85 -> sub_line 1, visual_col 5
        let (row, col) = map.cursor_to_visual_simple(0, 85);
        assert_eq!(row, 1);
        assert_eq!(col, 5);
    }

    #[test]
    fn test_roundtrip() {
        let lines: Vec<String> = vec![
            "a".repeat(80),
            "a".repeat(161),
            "a".repeat(50),
            "a".repeat(240),
            String::new(),
        ];
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let map = WrapMap::new(5, 80, 4, 0, make_text(&refs));
        for line in 0..5 {
            let visual = map.logical_to_visual(line);
            let (got_line, got_sub) = map.visual_to_logical(visual);
            assert_eq!(got_line, line);
            assert_eq!(got_sub, 0);
        }
    }

    #[test]
    fn test_visual_lines_in_range() {
        let l0 = "a".repeat(40);
        let l1 = "a".repeat(160);
        let l3 = "a".repeat(80);
        let map = WrapMap::new(
            4,
            80,
            4,
            0,
            make_text(&[l0.as_str(), l1.as_str(), "", l3.as_str()]),
        );
        // Lines: 1 + 2 + 1 + 1 = 5 total
        assert_eq!(map.visual_lines_in_range(0, 4), 5);
        assert_eq!(map.visual_lines_in_range(1, 3), 3); // 2 + 1
        assert_eq!(map.visual_lines_in_range(0, 1), 1);
    }

    #[test]
    fn test_wide_chars_increase_row_count() {
        // This is the key test: wide chars at wrap boundaries cause more rows
        // than a naïve div_ceil calculation would predict.
        // Width 3: "世世世" = 6 display cols, but each 世 (width 2) gets its own row
        // because 2+2 = 4 > 3
        let map = WrapMap::new(1, 3, 4, 0, make_text(&["世世世"]));
        assert_eq!(map.visual_lines_for(0), 3); // not 2!
        assert_eq!(map.total_visual_lines(), 3);
    }

    // ---- cursor_to_visual tests ----

    #[test]
    fn test_cursor_to_visual_no_wrap() {
        let map = WrapMap::new(1, 80, 4, 0, make_text(&["hello"]));
        // No wrapping, col maps directly
        assert_eq!(map.cursor_to_visual(0, 0, "hello"), (0, 0));
        assert_eq!(map.cursor_to_visual(0, 3, "hello"), (0, 3));
    }

    #[test]
    fn test_cursor_to_visual_ascii_wrap() {
        // 10 chars, wrap at 5 → wrap_point at char 5
        // Row 0: "abcde" (display cols 0..5), Row 1: "fghij" (display cols 5..10)
        let text = "abcdefghij";
        let map = WrapMap::new(1, 5, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 2);

        // Col 0-4 → sub_line 0
        assert_eq!(map.cursor_to_visual(0, 0, text), (0, 0));
        assert_eq!(map.cursor_to_visual(0, 4, text), (0, 4));
        // Col 5+ → sub_line 1
        assert_eq!(map.cursor_to_visual(0, 5, text), (1, 0));
        assert_eq!(map.cursor_to_visual(0, 9, text), (1, 4));
    }

    #[test]
    fn test_cursor_to_visual_multiple_wraps() {
        // 15 chars, wrap at 5 → 3 visual rows
        let text = "aaaaabbbbbccccc";
        let map = WrapMap::new(1, 5, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 3);

        assert_eq!(map.cursor_to_visual(0, 0, text), (0, 0));
        assert_eq!(map.cursor_to_visual(0, 4, text), (0, 4));
        assert_eq!(map.cursor_to_visual(0, 5, text), (1, 0));
        assert_eq!(map.cursor_to_visual(0, 9, text), (1, 4));
        assert_eq!(map.cursor_to_visual(0, 10, text), (2, 0));
        assert_eq!(map.cursor_to_visual(0, 14, text), (2, 4));
    }

    #[test]
    fn test_cursor_to_visual_wide_chars() {
        // "世世世" with wrap_width=3
        // 世 = width 2, so each gets its own row (2+2=4 > 3)
        // wrap_points at char 1 and char 2
        // Row 0: 世 (display cols 0..2), Row 1: 世 (display cols 2..4), Row 2: 世
        let text = "世世世";
        let map = WrapMap::new(1, 3, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 3);

        assert_eq!(map.cursor_to_visual(0, 0, text), (0, 0)); // first 世
        assert_eq!(map.cursor_to_visual(0, 2, text), (1, 0)); // second 世
        assert_eq!(map.cursor_to_visual(0, 4, text), (2, 0)); // third 世
    }

    #[test]
    fn test_cursor_to_visual_mixed_ascii_wide() {
        // "ab世cd" wrap_width=4
        // a(1) b(1) → 2, 世(2) → 4, fits! c(1) → 5 > 4, wraps
        // Row 0: "ab世" (cols 0-3), Row 1: "cd" (cols 4-5)
        let text = "ab世cd";
        let map = WrapMap::new(1, 4, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 2);

        assert_eq!(map.cursor_to_visual(0, 0, text), (0, 0)); // a
        assert_eq!(map.cursor_to_visual(0, 1, text), (0, 1)); // b
        assert_eq!(map.cursor_to_visual(0, 2, text), (0, 2)); // 世 (starts at display col 2)
        assert_eq!(map.cursor_to_visual(0, 4, text), (1, 0)); // c (wrapped)
        assert_eq!(map.cursor_to_visual(0, 5, text), (1, 1)); // d
    }

    #[test]
    fn test_cursor_to_visual_with_tabs() {
        // "a\tb" with tab_width=4, wrap_width=6
        // a = 1 col, \t = 3 cols (4 - 1%4 = 3), b = 1 col → total 5, fits
        let text = "a\tb";
        let map = WrapMap::new(1, 6, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 1);
        assert_eq!(map.cursor_to_visual(0, 0, text), (0, 0));
        assert_eq!(map.cursor_to_visual(0, 4, text), (0, 4)); // b at display col 4
    }

    #[test]
    fn test_cursor_to_visual_col_at_wrap_boundary() {
        // 10 chars, wrap at 5
        // Col 5 is the first col of the second row
        let text = "abcdefghij";
        let map = WrapMap::new(1, 5, 4, 0, make_text(&[text]));
        // Col exactly at boundary goes to next row
        assert_eq!(map.cursor_to_visual(0, 5, text), (1, 0));
    }

    #[test]
    fn test_cursor_to_visual_second_line() {
        // Two lines: first wraps, second doesn't
        let l0 = "a".repeat(10);
        let l1 = "bbb";
        let map = WrapMap::new(2, 5, 4, 0, make_text(&[&l0, l1]));
        // Line 0: 2 visual rows (base_row 0)
        // Line 1: 1 visual row (base_row 2)
        assert_eq!(map.cursor_to_visual(1, 0, l1), (2, 0));
        assert_eq!(map.cursor_to_visual(1, 2, l1), (2, 2));
    }

    #[test]
    fn test_cursor_to_visual_insert_at_exact_wrap_boundary() {
        // Line exactly fills wrap_width — insert cursor at col == wrap_width
        // should map to (next_row, 0), not (same_row, wrap_width)
        let text = "abcde"; // 5 chars, wrap_width = 5 → no wrap points
        let map = WrapMap::new(1, 5, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 1);
        // Insert mode cursor one past the last char
        assert_eq!(map.cursor_to_visual(0, 5, text), (1, 0));
    }

    #[test]
    fn test_cursor_to_visual_insert_at_wrapped_segment_boundary() {
        // 10 chars, wrap at 5. Last segment "fghij" exactly fills row 1.
        // Insert cursor at col 10 should go to (row 2, col 0).
        let text = "abcdefghij";
        let map = WrapMap::new(1, 5, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 2);
        assert_eq!(map.cursor_to_visual(0, 10, text), (2, 0));
    }

    #[test]
    fn test_sub_line_display_range_ascii_wrap() {
        let map = WrapMap::new(1, 5, 4, 0, make_text(&["abcdefghij"]));
        assert_eq!(map.sub_line_display_range("abcdefghij", 0), Some((0, 5)));
        assert_eq!(map.sub_line_display_range("abcdefghij", 1), Some((5, 10)));
        assert_eq!(map.sub_line_display_range("abcdefghij", 2), None);
    }

    #[test]
    fn test_sub_line_display_range_wide_chars() {
        // "世世世" with width 3 -> each wide char (2 cols) is wrapped separately.
        let map = WrapMap::new(1, 3, 4, 0, make_text(&["世世世"]));
        assert_eq!(map.sub_line_display_range("世世世", 0), Some((0, 2)));
        assert_eq!(map.sub_line_display_range("世世世", 1), Some((2, 4)));
        assert_eq!(map.sub_line_display_range("世世世", 2), Some((4, 6)));
    }

    #[test]
    fn test_sub_line_display_range_tabs() {
        // Wrap width 4, tab width 4: "\ta" -> [tab(4), "a"] -> wrap between entries
        let map = WrapMap::new(1, 4, 4, 0, make_text(&["\ta"]));
        assert_eq!(map.sub_line_display_range("\ta", 0), Some((0, 4)));
        assert_eq!(map.sub_line_display_range("\ta", 1), Some((4, 5)));
        assert_eq!(map.sub_line_display_range("", 0), Some((0, 0)));
        assert_eq!(map.sub_line_display_range("", 1), None);
    }

    // ---- Bug reproduction: wide char at wrap boundary ----

    #[test]
    fn test_cursor_to_visual_wide_char_pushed_to_next_row() {
        // "aaa世" with wrap_width=4
        // 'a'(1) + 'a'(1) + 'a'(1) = 3, then 世(2) needs 2 but 3+2=5 > 4
        // So 世 is pushed to next row with 1 col of padding on row 0.
        // Row 0: "aaa " (3 content + 1 pad), Row 1: "世  " (2 content + 2 pad)
        let text = "aaa世";
        let map = WrapMap::new(1, 4, 4, 0, make_text(&[text]));
        assert_eq!(map.visual_lines_for(0), 2);

        // cursor_to_visual should place 世 on row 1, col 0
        // display_col for 世 = 3 (after three 1-wide 'a' chars)
        // Simple division: 3/4 = row 0, 3%4 = col 3  ← WRONG (that's the padding)
        // Correct: row 1, col 0 (世 was pushed to next row)
        assert_eq!(map.cursor_to_visual(0, 3, text), (1, 0));
    }

    #[test]
    fn test_cursor_to_visual_wide_char_at_boundary_multi_line() {
        // Line 0: "aaa世" wraps to 2 visual rows at width 4
        // Line 1: "hello" fits in 1 visual row
        // Cursor on line 1 should be at visual row 2 (0-indexed)
        let text0 = "aaa世";
        let text1 = "hello";
        let map = WrapMap::new(2, 4, 4, 0, make_text(&[text0, text1]));
        assert_eq!(map.visual_lines_for(0), 2);
        assert_eq!(map.visual_lines_for(1), 2); // "hello" = 5 chars > 4 width
        assert_eq!(map.logical_to_visual(1), 2); // line 1 starts at visual row 2
    }

    // ---- Bug reproduction: decoration spanning multiple rows ----

    #[test]
    fn test_cursor_to_visual_decoration_spanning_rows() {
        // "ab" at width 4, decoration "123456" (6 cols) at char 1.
        // Rendered: "a123456b"
        // Row 0: "a123" (4 cols), Row 1: "456b" (4 cols) → 2 rows
        // Cursor at char 'b' (char_idx 1, display col = 1 + 6 = 7)
        // should be at row 1, display col 7 - 4 = 3
        let text = "ab";
        let decs = vec![(1, 6)]; // 6-col decoration at char 1
        let map = WrapMap::new_with_decorations(1, 4, 4, 0, make_text(&[text]), |_| decs.clone());
        // With decoration: total display = 1 + 6 + 1 = 8, at width 4 = 2 rows
        assert_eq!(map.visual_lines_for(0), 2);

        // Cursor on 'b' (char 1) — display col after decoration = 1 + 6 = 7
        let (row, col) = map.cursor_to_visual_with_decorations(0, 7, text, &decs);
        assert_eq!((row, col), (1, 3), "cursor on 'b' after 6-col decoration");
    }

    #[test]
    fn test_cursor_to_visual_large_decoration_many_rows() {
        // "ab" at width 3, decoration "1234567" (7 cols) at char 1.
        // Rendered: "a1234567b" = 9 display cols at width 3
        // Row 0: "a12" (3 cols), Row 1: "345" (3 cols), Row 2: "67b" (3 cols)
        // = 3 rows
        let text = "ab";
        let decs = vec![(1, 7)]; // 7-col decoration at char 1
        let map = WrapMap::new_with_decorations(1, 3, 4, 0, make_text(&[text]), |_| decs.clone());
        assert_eq!(map.visual_lines_for(0), 3);

        // Cursor on 'a' (display col 0) → row 0, col 0
        assert_eq!(
            map.cursor_to_visual_with_decorations(0, 0, text, &decs),
            (0, 0),
        );

        // Cursor on 'b' (display col = 1 + 7 = 8) → row 2, col 2
        assert_eq!(
            map.cursor_to_visual_with_decorations(0, 8, text, &decs),
            (2, 2),
            "cursor on 'b' after 7-col decoration spanning 3 rows",
        );
    }
}
