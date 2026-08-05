use super::languages::{Language, LanguageRegistry};
use super::theme::HighlightGroup;
use ropey::Rope;
use std::ops::Range;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, Tree};

/// Tree-sitter [`tree_sitter::TextProvider`] backed by a [`Rope`].
///
/// Used by the rope-aware highlight queries so we don't have to copy the
/// entire buffer into a `String` just to satisfy the query cursor's text
/// matching predicates (`#eq?`, `#match?`). Most highlight queries don't
/// invoke this — but when they do we walk rope chunks directly.
struct RopeTextProvider<'a> {
    rope: &'a Rope,
}

struct RopeChunkIter<'a> {
    rope: &'a Rope,
    pos: usize,
    end: usize,
}

impl<'a> Iterator for RopeChunkIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.end {
            return None;
        }
        let (chunk, chunk_byte_idx, _, _) = self.rope.chunk_at_byte(self.pos);
        let chunk_end = chunk_byte_idx + chunk.len();
        let local_start = self.pos - chunk_byte_idx;
        let take_end = chunk_end.min(self.end);
        let local_end = take_end - chunk_byte_idx;
        self.pos = take_end;
        Some(&chunk.as_bytes()[local_start..local_end])
    }
}

impl<'a> tree_sitter::TextProvider<&'a [u8]> for RopeTextProvider<'a> {
    type I = RopeChunkIter<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let range = node.byte_range();
        let end = range.end.min(self.rope.len_bytes());
        let pos = range.start.min(end);
        RopeChunkIter {
            rope: self.rope,
            pos,
            end,
        }
    }
}

/// Build a tree-sitter chunk callback that streams bytes from a rope.
///
/// The closure is what we hand to `Parser::parse_with_options` so we can
/// parse a rope without first materialising it into one big `String`. Each
/// call returns the *suffix* of the chunk containing `byte` — tree-sitter
/// will keep calling with advancing offsets until it gets an empty slice.
/// Return `(line_start_bytes, line_end_bytes)` mirroring the semantics of
/// `String::lines()` — a trailing newline does NOT add a phantom empty line,
/// so the returned vectors are indexable identically to the existing
/// `cached_highlights` cache built from the `&str` path.
fn rope_line_byte_offsets(rope: &Rope) -> (Vec<usize>, Vec<usize>) {
    let total_bytes = rope.len_bytes();
    let raw_lines = rope.len_lines();
    // Drop the phantom trailing empty line when the buffer ends with '\n',
    // matching `String::lines()` behaviour.
    let effective_lines = if total_bytes > 0 && rope.byte(total_bytes - 1) == b'\n' && raw_lines > 0
    {
        raw_lines - 1
    } else {
        raw_lines
    };

    let mut line_start_bytes = Vec::with_capacity(effective_lines);
    let mut line_end_bytes = Vec::with_capacity(effective_lines);
    for i in 0..effective_lines {
        let start = rope.line_to_byte(i);
        // For all but the last effective line, the next line's start - 1 is
        // the byte position of the terminating '\n'; trim it off so byte
        // offsets are line-content-relative.
        let raw_next = if i + 1 < raw_lines {
            rope.line_to_byte(i + 1)
        } else {
            total_bytes
        };
        let end = if raw_next > start && rope.byte(raw_next - 1) == b'\n' {
            raw_next - 1
        } else {
            raw_next
        };
        line_start_bytes.push(start);
        line_end_bytes.push(end);
    }
    (line_start_bytes, line_end_bytes)
}

/// Update a `(start, end)` dirty byte range with a new tree-sitter
/// [`tree_sitter::InputEdit`]. Output coordinates are in the **new** tree.
///
/// Three transforms compose:
/// 1. Pre-existing dirty bytes that sat entirely after the edit shift by
///    `new_end_byte - old_end_byte`.
/// 2. Pre-existing dirty bytes that overlapped the edit get absorbed by it.
/// 3. The edit itself contributes `start_byte..new_end_byte`.
///
/// We return the bounding box of all three. Over-querying is fine — under-
/// querying would leave stale highlights.
fn expand_dirty_range(
    current: Option<(usize, usize)>,
    edit: &tree_sitter::InputEdit,
) -> Option<(usize, usize)> {
    let edit_start = edit.start_byte;
    let edit_new_end = edit.new_end_byte;
    let edit_old_end = edit.old_end_byte;
    let edit_delta = edit_new_end as isize - edit_old_end as isize;

    let (cur_s, cur_e) = match current {
        Some(r) => r,
        None => return Some((edit_start, edit_new_end)),
    };

    let shift = |b: usize| -> usize {
        if b > edit_old_end {
            ((b as isize) + edit_delta).max(edit_new_end as isize) as usize
        } else if b > edit_start {
            // Within the edit window — collapse to the new end of the edit.
            edit_new_end
        } else {
            b
        }
    };

    let new_s = shift(cur_s).min(edit_start);
    let new_e = shift(cur_e).max(edit_new_end);
    Some((new_s, new_e))
}

fn rope_chunk_callback<'r>(
    rope: &'r Rope,
) -> impl FnMut(usize, tree_sitter::Point) -> &'r [u8] + 'r {
    let total_bytes = rope.len_bytes();
    move |byte: usize, _pos: tree_sitter::Point| -> &'r [u8] {
        if byte >= total_bytes {
            return b"";
        }
        let (chunk, chunk_byte_idx, _, _) = rope.chunk_at_byte(byte);
        let offset = byte - chunk_byte_idx;
        &chunk.as_bytes()[offset..]
    }
}

/// Syntax highlighter using tree-sitter
pub struct SyntaxHighlighter {
    language_id: String,
    /// Whether the grammar produces markdown-style `fenced_code_block` nodes,
    /// i.e. buffers of this language carry embedded code-block highlighting.
    supports_code_fences: bool,
    parser: Parser,
    tree: Option<Tree>,
    query: Query,
    capture_names: Vec<String>,
    /// Query that locates embedded-language regions (e.g. Astro's
    /// frontmatter/script/style blocks) that the top-level `query` leaves
    /// unhighlighted because they belong to a different grammar entirely.
    injection_query: Option<Query>,
    /// Snapshot of `tree` at the time of the last full highlight cache build
    /// (see `mark_cache_built`). Each `update_rope`/`update` call applies the
    /// same `InputEdit` to this snapshot so its byte coordinates stay in sync
    /// with the rope, while its STRUCTURE remains frozen. Comparing it to the
    /// current `tree` via `Tree::changed_ranges` then yields the byte ranges
    /// whose tree STRUCTURE changed since the last rebuild — useful for
    /// catching propagation (e.g. an unclosed string re-highlights lines
    /// below).
    prev_tree: Option<Tree>,
    /// Accumulated byte range covering every edit since the last
    /// `mark_cache_built`. Stored in CURRENT tree byte coordinates and
    /// expanded/shifted on each `update_rope` to account for new edits.
    /// Catches byte-content changes that don't shift tree structure
    /// (e.g. `1` → `10`), which `prev_tree.changed_ranges(...)` misses.
    dirty_byte_range: Option<(usize, usize)>,
}

impl SyntaxHighlighter {
    /// Creates a new syntax highlighter for the given language
    pub fn new(language: Language) -> Result<Self, String> {
        let ts_language = LanguageRegistry::get_tree_sitter_language(language);
        let query_source = LanguageRegistry::get_highlight_query(language);
        let injection_query_source = LanguageRegistry::get_injection_query(language);

        Self::from_parts(
            format!("{:?}", language).to_ascii_lowercase(),
            ts_language,
            query_source,
            injection_query_source,
        )
    }

    pub fn from_definition(
        language_id: &str,
        syntax: &crate::language_catalog::SyntaxDefinition,
    ) -> Result<Self, String> {
        Self::from_parts(
            language_id.to_string(),
            syntax.language.clone(),
            &syntax.highlights,
            None,
        )
    }

    fn from_parts(
        language_id: String,
        ts_language: tree_sitter::Language,
        query_source: &str,
        injection_query_source: Option<&str>,
    ) -> Result<Self, String> {
        let supports_code_fences = ts_language.id_for_node_kind("fenced_code_block", true) != 0;
        let mut parser = Parser::new();
        parser
            .set_language(&ts_language)
            .map_err(|e| format!("Failed to set language: {}", e))?;

        let query = Query::new(&ts_language, query_source)
            .map_err(|e| format!("Failed to create query: {}", e))?;

        let capture_names = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let injection_query = injection_query_source
            .map(|source| {
                Query::new(&ts_language, source)
                    .map_err(|e| format!("Failed to create injection query: {}", e))
            })
            .transpose()?;

        Ok(Self {
            language_id,
            supports_code_fences,
            parser,
            tree: None,
            query,
            capture_names,
            injection_query,
            prev_tree: None,
            dirty_byte_range: None,
        })
    }

    /// Parses the given source code
    pub fn parse(&mut self, source: &str) {
        self.tree = self.parser.parse(source, None);
        self.prev_tree = None;
        self.dirty_byte_range = None;
    }

    /// Updates the syntax tree after an edit
    pub fn update(&mut self, edit: tree_sitter::InputEdit, source: &str) {
        if let Some(ref mut tree) = self.tree {
            tree.edit(&edit);
            if let Some(ref mut prev) = self.prev_tree {
                prev.edit(&edit);
            }
            self.dirty_byte_range = expand_dirty_range(self.dirty_byte_range, &edit);
            self.tree = self.parser.parse(source, Some(tree));
        } else {
            self.parse(source);
        }
    }

    /// Parse directly from a [`Rope`] without materialising the whole buffer
    /// as a `String`. Tree-sitter pulls bytes via a chunk callback that walks
    /// the rope.
    pub fn parse_rope(&mut self, rope: &Rope) {
        let mut callback = rope_chunk_callback(rope);
        self.tree = self.parser.parse_with_options(&mut callback, None, None);
        self.prev_tree = None;
        self.dirty_byte_range = None;
    }

    /// Update the syntax tree after an edit, parsing the new content directly
    /// from a [`Rope`]. Skips the `rope.to_string()` copy the `&str` overload
    /// requires.
    pub fn update_rope(&mut self, edit: tree_sitter::InputEdit, rope: &Rope) {
        if let Some(ref mut tree) = self.tree {
            tree.edit(&edit);
            if let Some(ref mut prev) = self.prev_tree {
                prev.edit(&edit);
            }
            self.dirty_byte_range = expand_dirty_range(self.dirty_byte_range, &edit);
            let mut callback = rope_chunk_callback(rope);
            self.tree = self
                .parser
                .parse_with_options(&mut callback, Some(tree), None);
        } else {
            self.parse_rope(rope);
        }
    }

    /// Byte ranges that need re-querying since the last
    /// [`Self::mark_cache_built`] (or initial parse).
    ///
    /// Combines two sources of truth:
    /// * **Edited byte ranges** — tracked from `InputEdit`s so we catch
    ///   byte-content changes that don't shift tree structure (`1` → `10`).
    ///   `Tree::changed_ranges` returns an empty Vec in that case, which is
    ///   why we can't rely on it alone.
    /// * **`Tree::changed_ranges`** — catches structural propagation beyond
    ///   the literal edit point (e.g. opening a string mid-buffer re-styles
    ///   the rest of the line until a closing quote).
    ///
    /// Returns `None` when there's no baseline yet (first build) — callers
    /// should fall back to a full rebuild.
    pub fn dirty_ranges_since_last_build(&self) -> Option<Vec<std::ops::Range<usize>>> {
        let cur = self.tree.as_ref()?;
        let prev = self.prev_tree.as_ref()?;
        let mut ranges: Vec<std::ops::Range<usize>> = prev
            .changed_ranges(cur)
            .map(|r| r.start_byte..r.end_byte)
            .collect();
        if let Some((s, e)) = self.dirty_byte_range {
            ranges.push(s..e);
        }
        Some(ranges)
    }

    /// Mark the current tree as the baseline for future
    /// [`Self::dirty_ranges_since_last_build`] calls. Call this after a full
    /// highlight cache rebuild so subsequent rebuilds can skip unchanged
    /// regions.
    pub fn mark_cache_built(&mut self) {
        self.prev_tree = self.tree.clone();
        self.dirty_byte_range = None;
    }

    /// Rope-aware analogue of [`Self::highlights_for_all_lines`].
    ///
    /// The line index space matches `String::lines()` — i.e. a trailing
    /// newline does NOT produce a phantom empty line, so the returned Vec is
    /// indexable identically to the existing `cached_highlights` cache.
    pub fn highlights_for_all_lines_rope(
        &self,
        rope: &Rope,
    ) -> Vec<Vec<(Range<usize>, HighlightGroup)>> {
        let Some(ref tree) = self.tree else {
            return Vec::new();
        };

        let (line_start_bytes, line_end_bytes) = rope_line_byte_offsets(rope);
        let line_count = line_start_bytes.len();
        if line_count == 0 {
            return Vec::new();
        }
        let mut line_highlights: Vec<Vec<(Range<usize>, HighlightGroup)>> =
            vec![Vec::new(); line_count];

        let mut cursor = QueryCursor::new();
        let text_provider = RopeTextProvider { rope };
        let mut matches = cursor.matches(&self.query, tree.root_node(), text_provider);

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_byte = node.start_byte();
                let end_byte = node.end_byte();

                let capture_name = &self.capture_names[capture.index as usize];
                let group = Self::capture_to_highlight_group(capture_name);

                let first_line = line_end_bytes.partition_point(|&line_end| line_end <= start_byte);

                for line_idx in first_line..line_count {
                    let line_start_byte = line_start_bytes[line_idx];
                    if line_start_byte >= end_byte {
                        break;
                    }
                    let col_start = start_byte.saturating_sub(line_start_byte);
                    let line_end_byte = line_end_bytes[line_idx];
                    let line_len = line_end_byte - line_start_byte;
                    let col_end = if end_byte <= line_end_byte {
                        end_byte - line_start_byte
                    } else {
                        line_len
                    };
                    line_highlights[line_idx].push((col_start..col_end, group));
                }
            }
        }

        for highlights in &mut line_highlights {
            highlights.sort_by_key(|(range, _)| range.start);
        }

        line_highlights
    }

    /// Rope-aware analogue of [`Self::highlights_for_line_range`].
    pub fn highlights_for_line_range_rope(
        &self,
        rope: &Rope,
        start_line: usize,
        end_line: usize,
    ) -> Vec<Vec<(Range<usize>, HighlightGroup)>> {
        let Some(ref tree) = self.tree else {
            return Vec::new();
        };

        let range_len = end_line.saturating_sub(start_line);
        if range_len == 0 {
            return Vec::new();
        }

        let (line_start_bytes, line_end_bytes_all) = rope_line_byte_offsets(rope);
        let line_count = line_start_bytes.len();
        let actual_end = end_line.min(line_count);
        let actual_len = actual_end.saturating_sub(start_line);
        if actual_len == 0 {
            return Vec::new();
        }

        let mut line_highlights: Vec<Vec<(Range<usize>, HighlightGroup)>> =
            vec![Vec::new(); actual_len];

        let mut cursor = QueryCursor::new();
        cursor.set_point_range(
            tree_sitter::Point {
                row: start_line,
                column: 0,
            }..tree_sitter::Point {
                row: actual_end,
                column: 0,
            },
        );

        // Slice of line_end_bytes covering the requested range — used for
        // binary-search-based capture distribution like the &str path.
        let viewport_line_end_bytes: Vec<usize> =
            line_end_bytes_all[start_line..actual_end].to_vec();

        let text_provider = RopeTextProvider { rope };
        let mut matches = cursor.matches(&self.query, tree.root_node(), text_provider);

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_byte = node.start_byte();
                let end_byte = node.end_byte();

                let capture_name = &self.capture_names[capture.index as usize];
                let group = Self::capture_to_highlight_group(capture_name);

                let first_rel =
                    viewport_line_end_bytes.partition_point(|&line_end| line_end <= start_byte);

                for (rel_idx, highlights) in line_highlights
                    .iter_mut()
                    .enumerate()
                    .take(actual_len)
                    .skip(first_rel)
                {
                    let line_idx = start_line + rel_idx;
                    let line_start_byte = line_start_bytes[line_idx];
                    if line_start_byte >= end_byte {
                        break;
                    }
                    let col_start = start_byte.saturating_sub(line_start_byte);
                    let line_end_byte = line_end_bytes_all[line_idx];
                    let line_len = line_end_byte - line_start_byte;
                    let col_end = if end_byte <= line_end_byte {
                        end_byte - line_start_byte
                    } else {
                        line_len
                    };
                    highlights.push((col_start..col_end, group));
                }
            }
        }

        for highlights in &mut line_highlights {
            highlights.sort_by_key(|(range, _)| range.start);
        }

        line_highlights
    }

    /// Gets highlights for all lines at once (more efficient than per-line)
    /// This queries the syntax tree ONCE and distributes highlights to lines
    pub fn highlights_for_all_lines(
        &self,
        source: &str,
    ) -> Vec<Vec<(Range<usize>, HighlightGroup)>> {
        let Some(ref tree) = self.tree else {
            return Vec::new();
        };

        let lines: Vec<&str> = source.lines().collect();
        let mut line_highlights: Vec<Vec<(Range<usize>, HighlightGroup)>> =
            vec![Vec::new(); lines.len()];

        // Calculate line byte offsets once
        let mut line_start_bytes = Vec::with_capacity(lines.len());
        let mut offset = 0;
        for line in &lines {
            line_start_bytes.push(offset);
            offset += line.len() + 1; // +1 for newline
        }

        // Query the tree ONCE for all matches
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source.as_bytes());

        // Precompute line end bytes for binary search
        let line_end_bytes: Vec<usize> = lines
            .iter()
            .zip(line_start_bytes.iter())
            .map(|(line, &start)| start + line.len())
            .collect();

        // Distribute captures to lines using binary search
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_byte = node.start_byte();
                let end_byte = node.end_byte();

                // Get capture name and highlight group
                let capture_name = &self.capture_names[capture.index as usize];
                let group = Self::capture_to_highlight_group(capture_name);

                // Binary search to find the first line that could overlap.
                // A line overlaps if its end byte > start_byte.
                let first_line = line_end_bytes.partition_point(|&line_end| line_end <= start_byte);

                // Walk forward from there until lines no longer overlap
                for line_idx in first_line..lines.len() {
                    let line_start_byte = line_start_bytes[line_idx];

                    // Past the capture — no more lines can overlap
                    if line_start_byte >= end_byte {
                        break;
                    }

                    // Convert to column range relative to line start
                    let col_start = start_byte.saturating_sub(line_start_byte);
                    let line_len = lines[line_idx].len();
                    let line_end_byte = line_start_byte + line_len;
                    let col_end = if end_byte <= line_end_byte {
                        end_byte - line_start_byte
                    } else {
                        line_len
                    };

                    line_highlights[line_idx].push((col_start..col_end, group));
                }
            }
        }

        // Sort each line's highlights by start position
        for highlights in &mut line_highlights {
            highlights.sort_by_key(|(range, _)| range.start);
        }

        line_highlights
    }

    /// Gets highlights for a range of lines (viewport-aware query)
    /// Uses `set_point_range()` to restrict tree-sitter to only the visible lines.
    /// Returns a Vec indexed from 0 = start_line, with length (end_line - start_line).
    pub fn highlights_for_line_range(
        &self,
        source: &str,
        start_line: usize,
        end_line: usize,
    ) -> Vec<Vec<(Range<usize>, HighlightGroup)>> {
        let Some(ref tree) = self.tree else {
            return Vec::new();
        };

        let range_len = end_line.saturating_sub(start_line);
        if range_len == 0 {
            return Vec::new();
        }

        let lines: Vec<&str> = source.lines().collect();
        let actual_end = end_line.min(lines.len());
        let actual_len = actual_end.saturating_sub(start_line);
        if actual_len == 0 {
            return Vec::new();
        }

        let mut line_highlights: Vec<Vec<(Range<usize>, HighlightGroup)>> =
            vec![Vec::new(); actual_len];

        // Calculate byte offsets for lines in range
        let mut line_start_bytes = Vec::with_capacity(lines.len());
        let mut offset = 0;
        for line in &lines {
            line_start_bytes.push(offset);
            offset += line.len() + 1; // +1 for newline
        }

        // Restrict query cursor to the viewport line range
        let mut cursor = QueryCursor::new();
        cursor.set_point_range(
            tree_sitter::Point {
                row: start_line,
                column: 0,
            }..tree_sitter::Point {
                row: actual_end,
                column: 0,
            },
        );

        // Precompute line end bytes for the range (binary search target)
        let line_end_bytes: Vec<usize> = (start_line..actual_end)
            .map(|i| line_start_bytes[i] + lines[i].len())
            .collect();

        let mut matches = cursor.matches(&self.query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_byte = node.start_byte();
                let end_byte = node.end_byte();

                let capture_name = &self.capture_names[capture.index as usize];
                let group = Self::capture_to_highlight_group(capture_name);

                // Binary search within the viewport range to find first overlapping line.
                // A line overlaps if its end byte > start_byte.
                let first_rel = line_end_bytes.partition_point(|&line_end| line_end <= start_byte);

                for (rel_idx, highlights) in line_highlights
                    .iter_mut()
                    .enumerate()
                    .take(actual_len)
                    .skip(first_rel)
                {
                    let line_idx = start_line + rel_idx;
                    let line_start_byte = line_start_bytes[line_idx];

                    if line_start_byte >= end_byte {
                        break;
                    }

                    let col_start = start_byte.saturating_sub(line_start_byte);
                    let line_len = lines[line_idx].len();
                    let line_end_byte = line_start_byte + line_len;
                    let col_end = if end_byte <= line_end_byte {
                        end_byte - line_start_byte
                    } else {
                        line_len
                    };

                    highlights.push((col_start..col_end, group));
                }
            }
        }

        // Sort each line's highlights by start position
        for highlights in &mut line_highlights {
            highlights.sort_by_key(|(range, _)| range.start);
        }

        line_highlights
    }

    /// Gets highlights for a specific line.
    ///
    /// For bulk operations, use `highlights_for_all_lines()` which is much faster.
    /// This method scans bytes (not lines) to find the target line offset,
    /// which is O(byte offset to that line) but avoids collecting all lines.
    pub fn highlights_for_line(
        &self,
        line_idx: usize,
        source: &str,
    ) -> Vec<(Range<usize>, HighlightGroup)> {
        let Some(ref tree) = self.tree else {
            return Vec::new();
        };

        let mut highlights = Vec::new();
        let mut cursor = QueryCursor::new();

        // Find the byte offset of line_idx by scanning for newlines in the byte slice.
        // This is O(byte offset) but avoids allocating a Vec<&str> for all lines.
        let bytes = source.as_bytes();
        let mut line_start_byte = 0;
        for _ in 0..line_idx {
            match bytes[line_start_byte..].iter().position(|&b| b == b'\n') {
                Some(pos) => line_start_byte += pos + 1,
                None => return Vec::new(), // line_idx out of bounds
            }
        }
        // Find the end of this line
        let line_end_byte = match bytes[line_start_byte..].iter().position(|&b| b == b'\n') {
            Some(pos) => line_start_byte + pos,
            None => source.len(),
        };

        let line_len = line_end_byte - line_start_byte;

        // Restrict query to just this line
        cursor.set_point_range(
            tree_sitter::Point {
                row: line_idx,
                column: 0,
            }..tree_sitter::Point {
                row: line_idx + 1,
                column: 0,
            },
        );

        let mut matches = cursor.matches(&self.query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_byte = node.start_byte();
                let end_byte = node.end_byte();

                if start_byte < line_end_byte && end_byte > line_start_byte {
                    let capture_name = &self.capture_names[capture.index as usize];
                    let group = Self::capture_to_highlight_group(capture_name);

                    let col_start = start_byte.saturating_sub(line_start_byte);
                    let col_end = if end_byte <= line_end_byte {
                        end_byte - line_start_byte
                    } else {
                        line_len
                    };

                    highlights.push((col_start..col_end, group));
                }
            }
        }

        highlights.sort_by_key(|(range, _)| range.start);
        highlights
    }

    /// Converts a tree-sitter capture name to a highlight group
    /// Supports hierarchical names with specific handling (e.g., "type.builtin" -> TypeBuiltin)
    fn capture_to_highlight_group(name: &str) -> HighlightGroup {
        // Try exact match first (most common cases)
        match name {
            "keyword" => return HighlightGroup::Keyword,
            "function" => return HighlightGroup::Function,
            "type" => return HighlightGroup::Type,
            "type.builtin" => return HighlightGroup::TypeBuiltin,
            "string" => return HighlightGroup::String,
            "number" => return HighlightGroup::Number,
            "comment" => return HighlightGroup::Comment,
            "operator" => return HighlightGroup::Operator,
            "variable" => return HighlightGroup::Variable,
            "variable.builtin" => return HighlightGroup::VariableBuiltin,
            "macro" => return HighlightGroup::Macro,
            "constant" => return HighlightGroup::Constant,
            "property" => return HighlightGroup::Property,
            "parameter" => return HighlightGroup::Parameter,
            "label" => return HighlightGroup::Label,
            "punctuation" => return HighlightGroup::Punctuation,
            "punctuation.delimiter" => return HighlightGroup::PunctuationDelimiter,
            "punctuation.bracket" => return HighlightGroup::Punctuation,
            "tag" => return HighlightGroup::Tag,
            "tag.delimiter" => return HighlightGroup::PunctuationDelimiter,
            "constructor" => return HighlightGroup::Constructor,
            // Markup-specific captures for markdown
            "markup.italic" => return HighlightGroup::MarkupItalic,
            "markup.bold" => return HighlightGroup::MarkupBold,
            "markup.heading" => return HighlightGroup::MarkupHeading,
            "markup.raw" => return HighlightGroup::MarkupRaw,
            _ => {}
        }

        // If no exact match, try hierarchical fallback
        // For "comment.documentation", try "comment"
        if let Some(base) = name.split('.').next() {
            match base {
                "keyword" => return HighlightGroup::Keyword,
                "function" => return HighlightGroup::Function,
                "type" => return HighlightGroup::Type,
                "string" => return HighlightGroup::String,
                "number" => return HighlightGroup::Number,
                "comment" => return HighlightGroup::Comment,
                "operator" => return HighlightGroup::Operator,
                "variable" => return HighlightGroup::Variable,
                "macro" => return HighlightGroup::Macro,
                "constant" => return HighlightGroup::Constant,
                "property" => return HighlightGroup::Property,
                "parameter" => return HighlightGroup::Parameter,
                "label" => return HighlightGroup::Label,
                "punctuation" => return HighlightGroup::Punctuation,
                "tag" => return HighlightGroup::Tag,
                "constructor" => return HighlightGroup::Constructor,
                // Markup fallback - e.g., "markup.emphasis" -> markup
                "markup" => return HighlightGroup::MarkupItalic,
                _ => {}
            }
        }

        HighlightGroup::Other
    }

    /// Stable lowercase language identifier ("rust", "markdown", or a
    /// plugin-registered id). Dynamic languages have no `Language` enum
    /// variant, so identity checks must go through this id.
    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    /// True when this grammar produces markdown-style `fenced_code_block`
    /// nodes. Derived from the grammar itself, so markdown-like plugin
    /// languages get embedded code-block highlighting too.
    pub fn supports_code_fences(&self) -> bool {
        self.supports_code_fences
    }

    /// Gets the parse tree (if parsed)
    /// Used for extracting code blocks from markdown
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Query that locates embedded-language regions (e.g. Astro's
    /// frontmatter/script/style blocks). `None` for grammars with no such
    /// split, i.e. most languages.
    pub fn injection_query(&self) -> Option<&Query> {
        self.injection_query.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fence_support_is_derived_from_the_grammar() {
        assert!(SyntaxHighlighter::new(Language::Markdown)
            .unwrap()
            .supports_code_fences());
        assert!(!SyntaxHighlighter::new(Language::Rust)
            .unwrap()
            .supports_code_fences());
    }

    #[test]
    fn test_groovy_highlighter_parses_gradle_build_script_syntax() {
        let mut highlighter =
            SyntaxHighlighter::new(Language::Groovy).expect("Groovy highlighter should be created");
        let source = r#"
class Foo {
  int count = 0

  void greet(String who) {
    if (this.count != null) {
      println "hello ${who}"
    }
  }
}
"#;

        highlighter.parse(source);
        let highlights = highlighter.highlights_for_all_lines(source);
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Keyword));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Type));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::String));
    }

    #[test]
    fn test_astro_highlighter_parses_component_syntax() {
        let mut highlighter =
            SyntaxHighlighter::new(Language::Astro).expect("Astro highlighter should be created");
        let source = r#"---
import Layout from "../layouts/Layout.astro";
const title = "Hello";
---

<Layout title={title}>
  <h1 class="heading">{title}</h1>
</Layout>
"#;

        highlighter.parse(source);
        let tree = highlighter.tree().expect("Astro parse tree");
        assert!(!tree.root_node().has_error(), "{}", tree.root_node());

        let highlights = highlighter.highlights_for_all_lines(source);
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Tag));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Property));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::String));
    }

    #[test]
    fn test_wgsl_highlighter_parses_standard_and_bevy_syntax() {
        let mut highlighter =
            SyntaxHighlighter::new(Language::Wgsl).expect("WGSL highlighter should be created");
        let source = r#"#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

// Fullscreen shader
@group(0) @binding(0) var screen_tex: texture_2d<f32>;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let color = textureLoad(screen_tex, vec2<i32>(in.position.xy), 0);
    return color;
}
"#;

        highlighter.parse(source);
        let tree = highlighter.tree().expect("WGSL parse tree");
        assert!(!tree.root_node().has_error(), "{}", tree.root_node());

        let highlights = highlighter.highlights_for_all_lines(source);
        assert!(highlights.iter().any(|line| !line.is_empty()));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Function));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Comment));
    }

    #[test]
    fn test_tsx_highlighter() {
        let mut h =
            SyntaxHighlighter::new(Language::Tsx).expect("TSX highlighter should be created");

        let tsx_code = r#"const Button = ({ onClick }: Props) => {
  return (
    <button className="btn" onClick={onClick}>
      Click me
    </button>
  );
};"#;

        h.parse(tsx_code);
        let highlights = h.highlights_for_all_lines(tsx_code);

        assert!(!highlights.is_empty(), "Should have highlights");
        // Check that we have highlights on multiple lines
        let non_empty_lines = highlights.iter().filter(|h| !h.is_empty()).count();
        assert!(non_empty_lines >= 3, "Should highlight multiple lines");
    }

    #[test]
    fn test_lua_highlighter() {
        let mut highlighter =
            SyntaxHighlighter::new(Language::Lua).expect("Lua highlighter should be created");
        let source = "local enabled = true\nvim.keymap.set('n', 'q', ':quit<CR>')\n-- config";

        highlighter.parse(source);
        let tree = highlighter.tree().expect("Lua parse tree");
        assert!(!tree.root_node().has_error(), "{}", tree.root_node());
        let highlights = highlighter.highlights_for_all_lines(source);
        assert!(highlights.iter().any(|line| !line.is_empty()));
        assert!(highlights
            .iter()
            .flatten()
            .any(|(_, group)| *group == HighlightGroup::Comment));
    }

    #[test]
    fn test_typescript_highlighter() {
        let mut h = SyntaxHighlighter::new(Language::TypeScript)
            .expect("TypeScript highlighter should be created");

        let ts_code = r#"interface User {
  name: string;
  age: number;
}

const user: User = { name: "Alice", age: 30 };"#;

        h.parse(ts_code);
        let highlights = h.highlights_for_all_lines(ts_code);

        assert!(!highlights.is_empty(), "Should have highlights");
    }

    #[test]
    fn test_jsx_comment_brace_not_comment_colored() {
        // Regression: JSX comment braces {/* */} should not appear as comments.
        // The `}` was highlighted as PunctuationDelimiter which shared Comment's color.
        let mut h =
            SyntaxHighlighter::new(Language::Tsx).expect("TSX highlighter should be created");

        let code = "<div>\n  {/* comment */}\n</div>";

        h.parse(code);
        let highlights = h.highlights_for_all_lines(code);

        let line1_text = code.lines().nth(1).unwrap();
        let line1 = &highlights[1];

        // The closing } should be Punctuation (not PunctuationDelimiter, which is comment-colored)
        let closing_brace_byte = line1_text.len() - 1;
        let brace_highlight = line1
            .iter()
            .filter(|(range, _)| range.contains(&closing_brace_byte))
            .min_by_key(|(range, _)| range.end - range.start)
            .map(|(_, group)| *group);
        assert_eq!(
            brace_highlight,
            Some(HighlightGroup::Punctuation),
            "Closing brace of JSX comment should be Punctuation, not PunctuationDelimiter"
        );

        // Opening { too
        let opening_brace_byte = 2; // "  {" -> byte 2
        let brace_highlight = line1
            .iter()
            .filter(|(range, _)| range.contains(&opening_brace_byte))
            .min_by_key(|(range, _)| range.end - range.start)
            .map(|(_, group)| *group);
        assert_eq!(
            brace_highlight,
            Some(HighlightGroup::Punctuation),
            "Opening brace of JSX comment should be Punctuation, not PunctuationDelimiter"
        );
    }

    #[test]
    fn test_javascript_highlighter() {
        let mut h = SyntaxHighlighter::new(Language::JavaScript)
            .expect("JavaScript highlighter should be created");

        let js_code = r#"function greet(name) {
  return `Hello, ${name}!`;
}

const result = greet("World");"#;

        h.parse(js_code);
        let highlights = h.highlights_for_all_lines(js_code);

        assert!(!highlights.is_empty(), "Should have highlights");
    }

    #[test]
    fn rust_escaped_multiline_string_highlights_every_content_line() {
        let mut h =
            SyntaxHighlighter::new(Language::Rust).expect("Rust highlighter should be created");
        let source = r#"const HELP: &str = "\
first line
second line
third line";"#;

        h.parse(source);
        let highlights = h.highlights_for_all_lines(source);
        for (line_idx, content) in [(1, "first line"), (2, "second line"), (3, "third line")] {
            assert!(
                highlights[line_idx].iter().any(|(range, group)| {
                    *group == HighlightGroup::String
                        && range.start == 0
                        && range.end >= content.len()
                }),
                "line {line_idx} should retain its complete string capture: {:?}",
                highlights[line_idx]
            );
        }
    }

    #[test]
    fn test_line_range_matches_all_lines() {
        // Verify highlights_for_line_range produces the same results as the
        // corresponding slice of highlights_for_all_lines.
        let mut h =
            SyntaxHighlighter::new(Language::Rust).expect("Rust highlighter should be created");

        let source = "fn foo() {\n    let x = 42;\n    let y = \"hello\";\n    x + y\n}\n";
        h.parse(source);

        let all = h.highlights_for_all_lines(source);

        // Query a middle range
        let range = h.highlights_for_line_range(source, 1, 4);
        assert_eq!(range.len(), 3, "Should cover lines 1..4");
        for i in 0..3 {
            assert_eq!(
                range[i],
                all[1 + i],
                "Line range[{}] should match all_lines[{}]",
                i,
                1 + i
            );
        }
    }

    #[test]
    fn test_per_line_matches_all_lines() {
        // Verify highlights_for_line produces the same results as
        // highlights_for_all_lines for each line.
        let mut h =
            SyntaxHighlighter::new(Language::Rust).expect("Rust highlighter should be created");

        let source = "use std::io;\nfn main() {\n    println!(\"hi\");\n}\n";
        h.parse(source);

        let all = h.highlights_for_all_lines(source);
        for (i, expected) in all.iter().enumerate() {
            let per_line = h.highlights_for_line(i, source);
            assert_eq!(
                &per_line, expected,
                "highlights_for_line({}) should match highlights_for_all_lines",
                i
            );
        }
    }

    #[test]
    fn test_multiline_capture_distributed_correctly() {
        // A multi-line string literal should produce highlights on every
        // line it spans — this exercises the binary search path.
        let mut h =
            SyntaxHighlighter::new(Language::Rust).expect("Rust highlighter should be created");

        let source = "let s = \"\nline2\nline3\n\";\n";
        h.parse(source);

        let all = h.highlights_for_all_lines(source);
        // The string spans lines 0-3. Each should have at least one highlight.
        for (i, line_h) in all.iter().enumerate().take(4) {
            assert!(
                !line_h.is_empty(),
                "Line {} should have highlights from the multi-line string",
                i
            );
        }
    }

    #[test]
    fn test_out_of_bounds_line_returns_empty() {
        let mut h =
            SyntaxHighlighter::new(Language::Rust).expect("Rust highlighter should be created");
        let source = "fn main() {}";
        h.parse(source);

        assert!(h.highlights_for_line(999, source).is_empty());
        assert!(h.highlights_for_line_range(source, 5, 10).is_empty());
    }

    fn groovy_has_error(src: &str) -> bool {
        let ts_language =
            LanguageRegistry::get_tree_sitter_language(crate::syntax::languages::Language::Groovy);
        let mut parser = Parser::new();
        parser.set_language(&ts_language).unwrap();
        let tree = parser.parse(src, None).unwrap();
        tree.root_node().has_error()
    }

    #[test]
    fn groovy_empty_single_quote_string_parses_without_error() {
        // OV-00307: tree-sitter-groovy's character_literal token (inherited
        // from tree-sitter-java, which only ever needs single chars) required
        // >=1 char between quotes, so it had no rule at all for Groovy's empty
        // single-quoted string `''`, an extremely common idiom (e.g. `x ?: ''`
        // as an empty-string default). Hitting it mid-file threw the parser
        // into an unrecoverable ERROR node that swallowed everything downstream
        // until an unrelated later token happened to resynchronize it. Fixed by
        // patching character_literal's repeat1 -> repeat (see
        // vendor/tree-sitter-groovy/grammar.js) and repointing the dependency
        // at the patched build via [patch.crates-io] in the workspace Cargo.toml.
        assert!(
            !groovy_has_error("x = 'a'\n"),
            "non-empty single-quoted string should parse cleanly"
        );
        assert!(
            !groovy_has_error("x = \"\"\n"),
            "empty DOUBLE-quoted string should parse cleanly (control: only single-quote was broken)"
        );
        assert!(
            !groovy_has_error("x = project.hasProperty('y') ? project.y : 'z'\n"),
            "same ternary/method-call shape with non-empty literal should parse cleanly"
        );
        assert!(
            !groovy_has_error("x = ''\n"),
            "bare empty single-quoted string alone must parse without error"
        );
        assert!(
            !groovy_has_error("x = project.hasProperty('y') ? project.y : ''\n"),
            "real-world ternary-with-empty-string-default shape must parse without error"
        );
    }

    #[test]
    fn groovy_paren_less_multi_param_closure_parses_without_error() {
        // OV-00308: Groovy closures put an explicit `params ->` prefix
        // FIRST, immediately after `{`, followed by zero or more statements
        // sharing the SAME closing brace (e.g. `{ a, b -> stmt1; stmt2 }`).
        // The grammar's `closure` rule only modeled a single trailing
        // lambda_expression/expression *after* a repeat of statements, with
        // no path for a leading arrow followed by multiple statements, and
        // no path at all for an untyped multi-param list without
        // surrounding parens. Fixed by adding `bare_closure_parameters`
        // (see vendor/tree-sitter-groovy/grammar.js).
        assert!(
            !groovy_has_error("x = { a, b -> println(a) }\n"),
            "paren-less two-param closure, single statement body, must parse cleanly"
        );
        assert!(
            !groovy_has_error("x = { a, b ->\n  println(a)\n  println(b)\n}\n"),
            "paren-less two-param closure, multiple newline-separated statements, must parse cleanly"
        );
        assert!(
            !groovy_has_error(
                "rules.all { ComponentSelection selection ->\n  selection.reject('nope')\n}\n"
            ),
            "single TYPED param without parens (Type name ->) must parse cleanly"
        );
        assert!(
            !groovy_has_error(
                "configureJavaCompiler = { name, options, projectPath ->\n  def releaseVersion = 1\n  options.foo = releaseVersion\n}\n"
            ),
            "three untyped params with multiple statements referencing them must parse cleanly"
        );
    }

    #[test]
    fn groovy_juxt_call_inside_plain_block_parses_without_error() {
        // OV-00308: Java's `statement` (used inside plain `block`s: method
        // bodies, if/while/for/try/catch bodies) never gained the
        // paren-less juxt_function_call alternative that _toplevel_statement
        // and closure already had, so `println "x"` only worked at the top
        // level or inside a Groovy closure, not inside an ordinary block
        // like a catch body.
        assert!(
            !groovy_has_error("println \"hello\"\n"),
            "paren-less call at top level must parse cleanly (regression check)"
        );
        assert!(
            !groovy_has_error(
                "try {\n  x()\n} catch (Exception e) {\n  println \"failed\"\n  e.printStackTrace()\n}\n"
            ),
            "paren-less call inside a catch block body must parse cleanly"
        );
    }

    #[test]
    fn groovy_task_declaration_shorthand_parses_without_error() {
        // OV-00308: Gradle's `task xyz(type: Foo) { ... }` shorthand needs
        // two things the grammar lacked: a juxt (paren-less) argument that
        // is itself a parenthesized call (`xyz(type: Foo)`), and a trailing
        // closure body attached to the outer juxt call. Fixed via
        // `juxt_nested_call` and an optional closure body field on
        // `juxt_function_call`.
        assert!(
            !groovy_has_error("task allDeps(type: DependencyReportTask) {}\n"),
            "task declaration with type arg and closure body must parse cleanly"
        );
        assert!(
            !groovy_has_error("task systemTestLibs(dependsOn: jar)\n"),
            "task declaration with arg and no closure body must parse cleanly"
        );
    }

    #[test]
    fn groovy_closure_invocation_chaining_parses_without_error() {
        // OV-00308: Groovy Closures are callable objects (`expr(...)` sugars
        // to `expr.call(...)`), so a call's return value can itself be
        // invoked again immediately: `foo.bar(args)()`. Java has no such
        // "call the result of a call" syntax, so primary_expression never
        // had a trailing-argument-list alternative. This single construct
        // was responsible for the largest cascading ERROR span found in a
        // real build.gradle (thousands of lines).
        assert!(
            !groovy_has_error("logTestStdout.rehydrate(delegate, owner, this)()\n"),
            "invoking the result of a method call must parse cleanly"
        );
    }

    #[test]
    fn groovy_untyped_function_parameters_parse_without_error() {
        // OV-00308: Groovy `def` functions commonly take untyped parameters
        // (dynamic typing), e.g. `def foo(eclipse, project) { ... }` -
        // unlike Java, where formal_parameter required an explicit type.
        assert!(
            !groovy_has_error("def fineTuneEclipseClasspathFile(eclipse, project) {\n  x()\n}\n"),
            "def function with untyped parameters must parse cleanly"
        );
    }
}
