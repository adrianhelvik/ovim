use super::Buffer;
use crate::syntax::{
    CodeBlockCache, HighlightGroup, Language, LanguageRegistry, SyntaxHighlighter,
};
use std::borrow::Cow;
use std::ops::Range;

/// Finds inline code spans (single backtick `code`) in a line of text.
/// Returns ranges covering the content between backticks (including the backticks).
/// Handles escaped backticks and double-backtick (`` ` ``) spans.
fn find_inline_code_spans(line: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    // Build (char, byte_offset) pairs to track both char and byte positions
    let indexed: Vec<(usize, char)> = line.char_indices().collect();
    let len = indexed.len();
    let mut i = 0;

    while i < len {
        let (_, ch) = indexed[i];
        // Skip escaped backticks
        if ch == '\\' {
            i += 2;
            continue;
        }

        if ch == '`' {
            // Count consecutive backticks for the opening delimiter
            let open_byte_start = indexed[i].0;
            let mut backtick_count = 0;
            while i < len && indexed[i].1 == '`' {
                backtick_count += 1;
                i += 1;
            }

            // Find matching closing delimiter (same number of backticks)
            let content_start = i;
            let mut found_close = false;
            while i < len {
                if indexed[i].1 == '`' {
                    let mut close_count = 0;
                    while i < len && indexed[i].1 == '`' {
                        close_count += 1;
                        i += 1;
                    }
                    if close_count == backtick_count {
                        // Found matching close - span covers open backticks through close backticks
                        // Use byte position after the last closing backtick
                        let close_byte_end = if i < len { indexed[i].0 } else { line.len() };
                        spans.push(open_byte_start..close_byte_end);
                        found_close = true;
                        break;
                    }
                    // Not matching, continue searching
                } else {
                    i += 1;
                }
            }

            if !found_close {
                // No closing delimiter found, the backticks are literal
                i = content_start;
            }
        } else {
            i += 1;
        }
    }

    spans
}

/// Per-line syntax highlights: maps character ranges to highlight groups
pub type LineHighlights = Vec<Vec<(Range<usize>, HighlightGroup)>>;

/// Overlay higher-priority spans without discarding uncovered base syntax.
/// LSP semantic tokens are sorted and non-overlapping by protocol; each token
/// splits any intersecting Tree-sitter span and replaces only that interval.
fn overlay_line_highlights(
    base: &[(Range<usize>, HighlightGroup)],
    overlays: &[(Range<usize>, HighlightGroup)],
) -> Vec<(Range<usize>, HighlightGroup)> {
    let mut merged = Vec::with_capacity(base.len() + overlays.len());

    for (base_range, group) in base {
        let mut remaining = vec![base_range.clone()];
        for (overlay_range, _) in overlays {
            if overlay_range.start >= overlay_range.end {
                continue;
            }
            let mut next = Vec::with_capacity(remaining.len() + 1);
            for range in remaining {
                if overlay_range.end <= range.start || overlay_range.start >= range.end {
                    next.push(range);
                    continue;
                }
                if range.start < overlay_range.start {
                    next.push(range.start..overlay_range.start.min(range.end));
                }
                if overlay_range.end < range.end {
                    next.push(overlay_range.end.max(range.start)..range.end);
                }
            }
            remaining = next;
        }
        merged.extend(remaining.into_iter().map(|range| (range, *group)));
    }

    merged.extend(
        overlays
            .iter()
            .filter(|(range, _)| range.start < range.end)
            .cloned(),
    );
    merged.sort_by_key(|(range, _)| (range.start, range.end));
    merged
}

/// Large file threshold in lines - files above this disable expensive features
const LARGE_FILE_LINES: usize = 50_000;

/// Large file threshold in bytes - files above this disable expensive features
const LARGE_FILE_BYTES: usize = 5 * 1024 * 1024; // 5MB

/// Convert byte ranges to (start_line, end_line) pairs against the given
/// rope, clamped to `[0, line_count]`. Overlapping/adjacent line ranges are
/// merged so the caller doesn't re-query the same lines twice.
fn byte_ranges_to_line_ranges_owned(
    rope: &ropey::Rope,
    ranges: &[std::ops::Range<usize>],
    line_count: usize,
) -> Vec<(usize, usize)> {
    if ranges.is_empty() || line_count == 0 {
        return Vec::new();
    }
    let total_bytes = rope.len_bytes();
    let mut line_ranges: Vec<(usize, usize)> = ranges
        .iter()
        .map(|r| {
            let start_byte = r.start.min(total_bytes);
            let end_byte = r.end.min(total_bytes);
            let start_line = rope.byte_to_line(start_byte).min(line_count);
            // Include the line where end_byte lands by adding 1 (half-open).
            let end_line = (rope.byte_to_line(end_byte) + 1).min(line_count);
            (start_line, end_line.max(start_line))
        })
        .filter(|(s, e)| e > s)
        .collect();
    if line_ranges.is_empty() {
        return Vec::new();
    }
    line_ranges.sort_by_key(|&(s, _)| s);
    // Merge overlapping/adjacent ranges.
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(line_ranges.len());
    for (s, e) in line_ranges {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }
    merged
}

impl Buffer {
    /// Checks if this is a large file (exceeds line or byte threshold)
    pub fn is_large_file(&self) -> bool {
        let line_count = self.line_count();
        let byte_count = self.rope().len_bytes();

        line_count > LARGE_FILE_LINES || byte_count > LARGE_FILE_BYTES
    }

    /// Gets the large file threshold for lines
    pub fn large_file_line_threshold() -> usize {
        LARGE_FILE_LINES
    }

    /// Gets the large file threshold for bytes
    pub fn large_file_byte_threshold() -> usize {
        LARGE_FILE_BYTES
    }

    /// Enables syntax highlighting for this buffer based on file path
    /// Automatically skips large files for performance
    pub fn enable_syntax_highlighting(&mut self) {
        let Some(path) = self.file_path.clone() else {
            return;
        };
        self.enable_syntax_highlighting_for_path(&path);
    }

    /// Enables syntax highlighting using a source path without associating the
    /// buffer with that file. This is used by immutable virtual views such as
    /// walkthrough snapshots, which must never become save or LSP targets.
    pub fn enable_syntax_highlighting_for_path(&mut self, path: &str) {
        // Don't enable syntax for large files
        if self.is_large_file() {
            // Note: Syntax highlighting disabled for large files - don't print to stderr
            // This avoids interrupting user output
            return;
        }

        if let Some(definition) = self.language_catalog.detect(path) {
            if let Some(syntax) = &definition.syntax {
                if let Ok(mut highlighter) =
                    SyntaxHighlighter::from_definition(definition.id(), syntax)
                {
                    highlighter.parse_rope(&self.rope);
                    self.build_highlight_cache_from_rope(&highlighter);
                    self.syntax = Some(highlighter);
                    self.version += 1;
                }
            }
        } else if let Some(lang) = LanguageRegistry::detect_from_path(path) {
            if let Ok(mut highlighter) = SyntaxHighlighter::new(lang) {
                // Parse and build the cache directly from the rope — no
                // intermediate `String` copy of the entire buffer.
                highlighter.parse_rope(&self.rope);
                self.build_highlight_cache_from_rope(&highlighter);

                self.syntax = Some(highlighter);
                self.version += 1;
            }
        }
    }

    /// Checks if syntax highlighting should be initialized (lazy loading)
    /// Returns true if the buffer has a file path with supported language but no syntax yet
    pub fn should_init_syntax(&self) -> bool {
        // Don't initialize syntax for large files
        if self.is_large_file() {
            return false;
        }

        // Already has syntax or a background task is computing it
        if self.syntax.is_some() || self.syntax_loading {
            return false;
        }

        if let Some(ref path) = self.file_path {
            self.language_catalog.detect(path).is_some()
                || LanguageRegistry::detect_from_path(path).is_some()
        } else {
            false
        }
    }

    /// Marks that a background task is computing initial syntax highlights.
    /// Prevents `should_init_syntax()` from returning true again until the task completes.
    pub fn mark_syntax_loading(&mut self) {
        self.syntax_loading = true;
    }

    /// Clears the background syntax-loading flag without applying highlights.
    pub fn clear_syntax_loading(&mut self) {
        self.syntax_loading = false;
    }

    /// Returns the current highlight version counter.
    /// Used to check if the buffer has been edited since a background parse started.
    pub fn highlight_version(&self) -> u64 {
        self.highlight_version
    }

    /// Applies pre-computed syntax highlights from a background task.
    /// Returns true if the highlights were applied (version matched).
    ///
    /// After installing the cached highlights (so the next render is styled),
    /// creates a SyntaxHighlighter on the main thread for future incremental updates.
    pub fn apply_background_syntax(
        &mut self,
        lang: Language,
        highlights: LineHighlights,
        version: u64,
    ) -> bool {
        self.syntax_loading = false;

        // Only apply if buffer hasn't been edited since the background parse started
        if self.highlight_version != version {
            return false;
        }

        // Install cached highlights so the next render shows styled text
        self.cached_highlights = Some(highlights);

        // Create a SyntaxHighlighter on the main thread for future incremental updates.
        // This parses the current content, which is fast (~5ms for typical files) and
        // doesn't cause FOUC since highlights are already cached above.
        if let Ok(mut highlighter) = SyntaxHighlighter::new(lang) {
            highlighter.parse_rope(&self.rope);

            // For markdown files, also build code block cache. The code block
            // cache builder still consumes `&str`, so allocate once here. (A
            // future change can move it to a rope-aware path; for now the
            // markdown path is rarer than the hot edit path.)
            if highlighter.supports_code_fences() {
                if let Some(tree) = highlighter.tree() {
                    let source = self.rope.to_string();
                    let mut cache = crate::syntax::CodeBlockCache::new();
                    cache.update_from_tree(
                        tree,
                        &source,
                        self.highlight_version,
                        &self.language_catalog,
                    );
                    self.code_block_cache = Some(cache);
                }
            }

            self.syntax = Some(highlighter);
            self.pending_rehighlight = false;
            self.version += 1;
        }

        true
    }

    /// Rope-aware highlight cache builder. Avoids the per-build `String`
    /// copy of the entire buffer the previous `&str` path required.
    pub(super) fn build_highlight_cache_from_rope(&mut self, highlighter: &SyntaxHighlighter) {
        self.cached_highlights = Some(highlighter.highlights_for_all_lines_rope(&self.rope));

        // For markdown-style files, also build code block cache. The
        // code-block builder still needs `&str` for its tree walk; allocate
        // only on the markdown path (much rarer than per-edit rehighlight).
        if highlighter.supports_code_fences() {
            if let Some(tree) = highlighter.tree() {
                let source = self.rope.to_string();
                let mut cache = CodeBlockCache::new();
                cache.update_from_tree(
                    tree,
                    &source,
                    self.highlight_version,
                    &self.language_catalog,
                );
                self.code_block_cache = Some(cache);
            }
        }
    }

    /// Shifts highlights after an insertion.
    ///
    /// `byte_col` is a **byte offset** within the line — the highlight cache
    /// stores ranges in byte offsets, so all arithmetic here is byte-based.
    pub(super) fn shift_highlights_for_insertion(
        &mut self,
        line: usize,
        byte_col: usize,
        text: &str,
    ) {
        let Some(ref mut cache) = self.cached_highlights else {
            return; // No cache to shift
        };

        if line >= cache.len() {
            return;
        }

        // Check if insertion contains newlines
        let newline_count = text.matches('\n').count();

        if newline_count == 0 {
            // Single-line insertion: shift highlights on the same line
            // Use byte length since highlight cache stores byte offsets
            let insert_byte_len = text.len();

            for (range, _) in &mut cache[line] {
                if range.start >= byte_col {
                    // Highlight starts after insertion point: shift right
                    range.start += insert_byte_len;
                    range.end += insert_byte_len;
                } else if range.end > byte_col {
                    // Highlight contains insertion point: extend end
                    range.end += insert_byte_len;
                }
            }
        } else {
            // Multi-line insertion: handle line splits and shifts
            let lines: Vec<&str> = text.split('\n').collect();
            // Use byte length since highlight cache stores byte offsets
            let last_line_len = lines.last().map(|s| s.len()).unwrap_or(0);

            // Split the current line's highlights at the insertion point
            let current_line_highlights = cache[line].clone();
            let mut before_insert = Vec::new();
            let mut after_insert = Vec::new();

            for (range, group) in current_line_highlights {
                if range.end <= byte_col {
                    // Entirely before insertion
                    before_insert.push((range, group));
                } else if range.start >= byte_col {
                    // Entirely after insertion: will move to new line
                    // Adjust column position (relative to start of new line)
                    let new_start = range.start - byte_col + last_line_len;
                    let new_end = range.end - byte_col + last_line_len;
                    after_insert.push((new_start..new_end, group));
                } else {
                    // Spans insertion point: keep the before part only
                    before_insert.push((range.start..byte_col, group));
                    // The after part would be on the new line, but it's cut off
                    // (We can't split highlights perfectly without re-parsing)
                }
            }

            // Update current line with highlights before insertion
            cache[line] = before_insert;

            // Insert new empty lines for the newlines in the inserted text
            for _ in 0..newline_count {
                cache.insert(line + 1, Vec::new());
            }

            // The last new line gets the highlights that were after the insertion
            if line + newline_count < cache.len() {
                cache[line + newline_count] = after_insert;
            }
        }
    }

    /// Creates a tree-sitter InputEdit for an insertion operation.
    /// This enables incremental parsing instead of full re-parse.
    ///
    /// `byte_col` is a **byte offset** within the line — NOT a char index.
    /// The caller must convert char indices to byte offsets before calling.
    /// Tree-sitter `Point.column` requires byte offsets.
    pub(super) fn create_ts_insert_edit(
        &self,
        line: usize,
        byte_col: usize,
        text: &str,
    ) -> Option<tree_sitter::InputEdit> {
        // Compute absolute byte position: byte offset of line start + byte_col
        let line_start_byte = self.rope.char_to_byte(self.rope.line_to_char(line));
        let start_byte =
            (line_start_byte + byte_col).min(self.rope.char_to_byte(self.rope.len_chars()));
        let old_end_byte = start_byte;
        let new_end_byte = start_byte + text.len();

        // Tree-sitter Point.column is a byte offset within the line
        let start_position = tree_sitter::Point {
            row: line,
            column: byte_col,
        };

        // For insertions, old_end == start
        let old_end_position = start_position;

        // Calculate new_end position based on newlines in inserted text
        let newline_count = text.matches('\n').count();
        let new_end_position = if newline_count == 0 {
            tree_sitter::Point {
                row: line,
                column: byte_col + text.len(),
            }
        } else {
            let last_line = text.split('\n').next_back().unwrap_or("");
            tree_sitter::Point {
                row: line + newline_count,
                column: last_line.len(),
            }
        };

        Some(tree_sitter::InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        })
    }

    /// Creates a tree-sitter InputEdit for a deletion operation.
    /// This enables incremental parsing instead of full re-parse.
    ///
    /// `start_byte_col` / `end_byte_col` are **byte offsets** within their
    /// respective lines — this is what tree-sitter `Point.column` expects.
    pub(super) fn create_ts_delete_edit(
        &self,
        start_line: usize,
        start_byte_col: usize,
        end_line: usize,
        end_byte_col: usize,
        deleted_text: &str,
    ) -> Option<tree_sitter::InputEdit> {
        let start_line_byte = self.rope.char_to_byte(self.rope.line_to_char(start_line));
        let start_byte =
            (start_line_byte + start_byte_col).min(self.rope.char_to_byte(self.rope.len_chars()));
        let old_end_byte = start_byte + deleted_text.len();
        let new_end_byte = start_byte;

        let start_position = tree_sitter::Point {
            row: start_line,
            column: start_byte_col,
        };

        let old_end_position = tree_sitter::Point {
            row: end_line,
            column: end_byte_col,
        };

        // For deletions, new_end == start
        let new_end_position = start_position;

        Some(tree_sitter::InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        })
    }

    /// Applies an incremental tree-sitter edit to the syntax highlighter
    /// This is much faster than full re-parse for small edits
    pub(super) fn apply_incremental_syntax_edit(&mut self, edit: tree_sitter::InputEdit) {
        if let Some(ref mut syntax) = self.syntax {
            // Re-parse directly from the rope — avoids the per-keystroke
            // `rope.to_string()` allocation. Tree-sitter still reuses the
            // existing tree via `update_rope`.
            syntax.update_rope(edit, &self.rope);

            // Keep stale highlights until new ones are calculated
            // This prevents flashing (no highlights) during typing
            // The pending_rehighlight flag ensures fresh highlights will be computed
        }
    }

    /// Shifts highlights after a deletion.
    ///
    /// `start_byte_col` / `end_byte_col` are **byte offsets** within their
    /// respective lines — the highlight cache stores ranges in byte offsets.
    pub(super) fn shift_highlights_for_deletion(
        &mut self,
        start_line: usize,
        start_byte_col: usize,
        end_line: usize,
        end_byte_col: usize,
    ) {
        let Some(ref mut cache) = self.cached_highlights else {
            return; // No cache to shift
        };

        if start_line >= cache.len() {
            return;
        }

        if start_line == end_line {
            // Single-line deletion
            if start_line >= cache.len() {
                return;
            }

            let deleted_bytes = end_byte_col.saturating_sub(start_byte_col);
            let highlights = &mut cache[start_line];

            // Filter and adjust highlights
            highlights.retain_mut(|(range, _)| {
                if range.end <= start_byte_col {
                    // Before deletion: keep as-is
                    true
                } else if range.start >= end_byte_col {
                    // After deletion: shift left
                    range.start = range.start.saturating_sub(deleted_bytes);
                    range.end = range.end.saturating_sub(deleted_bytes);
                    true
                } else if range.start >= start_byte_col && range.end <= end_byte_col {
                    // Entirely within deletion: remove
                    false
                } else if range.start < start_byte_col && range.end > end_byte_col {
                    // Contains deletion: shrink
                    range.end = start_byte_col + (range.end - end_byte_col);
                    true
                } else if range.start < start_byte_col {
                    // Starts before, ends within deletion
                    range.end = start_byte_col;
                    true
                } else {
                    // Starts within, ends after deletion
                    range.start = start_byte_col;
                    range.end = start_byte_col + (range.end - end_byte_col);
                    true
                }
            });
        } else {
            // Multi-line deletion
            let deleted_lines = end_line - start_line;

            // Get highlights from end of deletion range that survive
            let surviving_highlights = if end_line < cache.len() {
                cache[end_line]
                    .iter()
                    .filter_map(|(range, group)| {
                        if range.start >= end_byte_col {
                            // After deletion point: shift to start line
                            let new_start = start_byte_col + (range.start - end_byte_col);
                            let new_end = start_byte_col + (range.end - end_byte_col);
                            Some((new_start..new_end, *group))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            // Trim start line highlights
            if start_line < cache.len() {
                cache[start_line].retain(|(range, _)| range.end <= start_byte_col);
                // Add surviving highlights from end line
                cache[start_line].extend(surviving_highlights);
            }

            // Remove deleted lines
            if start_line + 1 < cache.len() {
                let end = (start_line + deleted_lines + 1).min(cache.len());
                cache.drain(start_line + 1..end);
            }
        }
    }

    /// Gets syntax highlights for a specific line
    ///
    /// Returns a borrowed view of the cached highlight list when possible. The
    /// hot rendering path is per-visible-line, so avoiding a Vec clone here is
    /// material — the common case (non-markdown, no semantic overlay) returns
    /// `Cow::Borrowed`. Markdown files that need to overlay inline code spans
    /// pay one allocation on top of the cache, same as before.
    ///
    /// Priority order:
    /// 1. Code block cache (for markdown code fences with known languages)
    /// 2. Tree-sitter cached highlights
    /// 3. Semantic highlights overlaid only on their covered ranges
    /// 4. Inline code overlay (for markdown: backtick `code` spans)
    pub fn highlights_for_line(
        &self,
        line_idx: usize,
    ) -> Cow<'_, [(Range<usize>, HighlightGroup)]> {
        // For markdown: check code block cache first (language-specific highlighting)
        if let Some(ref code_cache) = self.code_block_cache {
            if let Some(highlights) = code_cache.highlights_for_line(line_idx) {
                return Cow::Borrowed(highlights.as_slice());
            }
        }

        // Get base highlights from tree-sitter cache (borrowed when no overlay needed)
        let base: &[(Range<usize>, HighlightGroup)] = self
            .cached_highlights
            .as_ref()
            .and_then(|cache| cache.get(line_idx))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // For markdown files NOT inside fenced code blocks, overlay inline `code` spans
        if let Some(ref code_cache) = self.code_block_cache {
            if !code_cache.is_line_in_code_block(line_idx) {
                let inline_spans = if let Some(line_text) = self.rope.line(line_idx).as_str() {
                    find_inline_code_spans(line_text)
                } else {
                    // Fallback for lines that cross chunk boundaries
                    let line_text: String = self.rope.line(line_idx).chars().collect();
                    find_inline_code_spans(&line_text)
                };
                if !inline_spans.is_empty() {
                    let mut highlights = base.to_vec();
                    for span in inline_spans {
                        highlights.push((span, HighlightGroup::MarkupRaw));
                    }
                    if let Some(semantic) = self
                        .semantic_highlights
                        .as_ref()
                        .and_then(|lines| lines.get(line_idx))
                        .filter(|spans| !spans.is_empty())
                    {
                        return Cow::Owned(overlay_line_highlights(&highlights, semantic));
                    }
                    return Cow::Owned(highlights);
                }
            }
        }

        if let Some(semantic) = self
            .semantic_highlights
            .as_ref()
            .and_then(|lines| lines.get(line_idx))
            .filter(|spans| !spans.is_empty())
        {
            return Cow::Owned(overlay_line_highlights(base, semantic));
        }

        Cow::Borrowed(base)
    }

    /// Sets semantic highlights decoded from LSP semantic tokens
    pub fn set_semantic_highlights(
        &mut self,
        highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
    ) {
        self.semantic_highlights = Some(highlights);
    }

    /// Clears semantic highlights (e.g., when LSP disconnects)
    pub fn clear_semantic_highlights(&mut self) {
        self.semantic_highlights = None;
    }

    /// Checks if semantic highlights are available
    pub fn has_semantic_highlights(&self) -> bool {
        self.semantic_highlights.is_some()
    }

    /// Decodes LSP semantic tokens into highlight spans
    /// The legend provides the mapping from token type indices to names
    pub fn decode_semantic_tokens(
        &mut self,
        tokens: &lsp_types::SemanticTokens,
        legend: &lsp_types::SemanticTokensLegend,
    ) {
        let line_count = self.line_count();
        let mut highlights: Vec<Vec<(Range<usize>, HighlightGroup)>> = vec![Vec::new(); line_count];

        // Semantic tokens use relative positions (delta encoding)
        let mut current_line: u32 = 0;
        let mut current_char: u32 = 0;

        for token in &tokens.data {
            // Update position based on deltas
            if token.delta_line > 0 {
                current_line += token.delta_line;
                current_char = token.delta_start;
            } else {
                current_char += token.delta_start;
            }

            let line = current_line as usize;
            if line >= line_count {
                break;
            }

            let start_col = current_char as usize;
            let end_col = start_col + token.length as usize;
            let token_type = token.token_type as usize;

            // Map token type to HighlightGroup
            let highlight_group = if token_type < legend.token_types.len() {
                Self::lsp_token_type_to_highlight_group(legend.token_types[token_type].as_str())
            } else {
                HighlightGroup::Other
            };

            highlights[line].push((start_col..end_col, highlight_group));
        }

        self.semantic_highlights = Some(highlights);
    }

    /// Maps LSP semantic token type names to HighlightGroup
    fn lsp_token_type_to_highlight_group(token_type: &str) -> HighlightGroup {
        match token_type {
            "namespace" | "module" => HighlightGroup::Type,
            "type" | "class" | "enum" | "interface" | "struct" | "typeParameter" => {
                HighlightGroup::Type
            }
            "parameter" => HighlightGroup::Parameter,
            "variable" | "property" | "enumMember" => HighlightGroup::Variable,
            "function" | "method" | "member" => HighlightGroup::Function,
            "macro" | "decorator" => HighlightGroup::Macro,
            "keyword" | "modifier" => HighlightGroup::Keyword,
            "comment" => HighlightGroup::Comment,
            "string" | "regexp" => HighlightGroup::String,
            "number" => HighlightGroup::Number,
            "operator" => HighlightGroup::Operator,
            "label" | "event" => HighlightGroup::Label,
            _ => HighlightGroup::Other,
        }
    }

    /// Checks if syntax highlighting is enabled
    pub fn has_syntax_highlighting(&self) -> bool {
        self.syntax.is_some()
    }

    /// Returns a reference to the treesitter syntax tree, if available.
    pub fn syntax_tree(&self) -> Option<&tree_sitter::Tree> {
        self.syntax.as_ref().and_then(|s| s.tree())
    }

    /// Checks if re-highlighting is needed
    pub fn needs_rehighlight(&self) -> bool {
        self.pending_rehighlight && self.syntax.is_some()
    }

    /// Rebuilds highlight cache from the existing syntax highlighter
    /// This uses the incrementally-updated parse tree, so it's fast!
    /// The tree-sitter parse tree was already updated incrementally via `update()`,
    /// so this just queries it for highlights (no re-parsing needed).
    ///
    /// When a previous cache build exists, only the line ranges that changed
    /// structurally (per `Tree::changed_ranges`) are re-queried; lines that
    /// didn't change keep their existing cache entries. Falls back to a full
    /// rebuild for the very first build and when the buffer's line count
    /// shifted (insertion/deletion of full lines makes patching brittle).
    pub fn rebuild_highlight_cache(&mut self) -> Option<u64> {
        if !self.needs_rehighlight() {
            return None;
        }

        let supports_code_fences = self.syntax.as_ref()?.supports_code_fences();
        let version = self.highlight_version;

        let new_line_count = self.line_count();
        let can_patch = self
            .cached_highlights
            .as_ref()
            .map(|c| c.len() == new_line_count)
            .unwrap_or(false);

        let dirty_ranges_opt: Option<Vec<std::ops::Range<usize>>> = if can_patch {
            self.syntax
                .as_ref()
                .and_then(|s| s.dirty_ranges_since_last_build())
        } else {
            None
        };

        match dirty_ranges_opt {
            Some(ref ranges) if !ranges.is_empty() => {
                // Incremental patch: re-query only the affected line ranges.
                let line_ranges =
                    byte_ranges_to_line_ranges_owned(&self.rope, ranges, new_line_count);
                if let Some(syntax) = self.syntax.as_ref() {
                    if let Some(cache) = self.cached_highlights.as_mut() {
                        for (start_line, end_line) in line_ranges {
                            let highlights = syntax
                                .highlights_for_line_range_rope(&self.rope, start_line, end_line);
                            for (i, h) in highlights.into_iter().enumerate() {
                                let idx = start_line + i;
                                if idx < cache.len() {
                                    cache[idx] = h;
                                }
                            }
                        }
                    }
                }
            }
            Some(_) => {
                // No changes since baseline — keep existing cache.
            }
            None => {
                // First build or line-count shift: full rebuild.
                let syntax = self.syntax.as_ref()?;
                let highlights = syntax.highlights_for_all_lines_rope(&self.rope);
                self.cached_highlights = Some(highlights);
            }
        }

        // For markdown-style files, also rebuild code block cache. Still
        // &str-based — markdown is the rarer path; the hot path is
        // non-markdown edits.
        if supports_code_fences {
            if let Some(syntax) = self.syntax.as_ref() {
                if let Some(tree) = syntax.tree() {
                    let content = self.rope.to_string();
                    let mut cache = CodeBlockCache::new();
                    cache.update_from_tree(tree, &content, version, &self.language_catalog);
                    self.code_block_cache = Some(cache);
                }
            }
        }

        // Refresh the baseline so the next call can be incremental.
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.mark_cache_built();
        }

        self.pending_rehighlight = false;

        Some(version)
    }

    /// Rebuilds highlight cache for only the viewport lines (start_line..end_line).
    /// Does NOT clear pending_rehighlight — the full rebuild is still needed for off-screen content.
    pub fn rebuild_viewport_highlight_cache(&mut self, start_line: usize, end_line: usize) {
        let syntax = match self.syntax.as_ref() {
            Some(s) => s,
            None => return,
        };

        let line_count = self.line_count();
        let actual_end = end_line.min(line_count);
        if start_line >= actual_end {
            return;
        }

        // Query just the visible range, streaming bytes from the rope (no
        // whole-buffer String copy per keystroke).
        let viewport_highlights =
            syntax.highlights_for_line_range_rope(&self.rope, start_line, actual_end);

        // Ensure the cache exists and is the right size
        let cache = self
            .cached_highlights
            .get_or_insert_with(|| vec![Vec::new(); line_count]);
        // Resize if needed (buffer may have grown/shrunk)
        cache.resize_with(line_count, Vec::new);

        // Overwrite only the viewport portion
        for (i, highlights) in viewport_highlights.into_iter().enumerate() {
            let line_idx = start_line + i;
            if line_idx < cache.len() {
                cache[line_idx] = highlights;
            }
        }

        // For markdown-style files, also rebuild the code block cache so that
        // language-specific highlighting inside fenced code blocks is
        // available immediately (not deferred to the debounced full rebuild).
        if syntax.supports_code_fences() && self.code_block_cache.is_none() {
            if let Some(tree) = syntax.tree() {
                let content = self.rope.to_string();
                let mut cb_cache = CodeBlockCache::new();
                cb_cache.update_from_tree(
                    tree,
                    &content,
                    self.highlight_version,
                    &self.language_catalog,
                );
                self.code_block_cache = Some(cb_cache);
            }
        }
    }

    /// Applies re-highlighted results if version matches
    pub fn apply_highlights(
        &mut self,
        highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
        version: u64,
    ) -> bool {
        // Only apply if version matches (buffer hasn't changed since re-parse started)
        if self.highlight_version == version {
            self.cached_highlights = Some(highlights);
            self.pending_rehighlight = false;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_overlay_splits_only_intersecting_syntax_ranges() {
        let base = vec![
            (0..12, HighlightGroup::String),
            (14..20, HighlightGroup::Comment),
        ];
        let semantic = vec![(3..6, HighlightGroup::Variable)];

        assert_eq!(
            overlay_line_highlights(&base, &semantic),
            vec![
                (0..3, HighlightGroup::String),
                (3..6, HighlightGroup::Variable),
                (6..12, HighlightGroup::String),
                (14..20, HighlightGroup::Comment),
            ]
        );
    }

    #[test]
    fn highlights_for_line_preserves_uncovered_tree_sitter_spans() {
        let mut buffer = Buffer::new();
        buffer.cached_highlights = Some(vec![vec![(0..16, HighlightGroup::String)]]);
        buffer.set_semantic_highlights(vec![vec![(6..10, HighlightGroup::Variable)]]);

        assert_eq!(
            buffer.highlights_for_line(0).as_ref(),
            &[
                (0..6, HighlightGroup::String),
                (6..10, HighlightGroup::Variable),
                (10..16, HighlightGroup::String),
            ]
        );
    }

    /// Regression test: the per-keystroke rehighlight paths used to call
    /// `SyntaxHighlighter::language()`, which panicked for plugin-registered
    /// languages (no `Language` enum variant).
    #[test]
    fn rehighlighting_a_dynamic_language_buffer_does_not_panic() {
        use crate::language_catalog::SyntaxDefinition;
        use crate::syntax::LanguageRegistry;

        // A built-in grammar under a plugin-style id: from_definition maps
        // "nula-like" to no enum variant, exactly like a dynamic language.
        let definition = SyntaxDefinition {
            language: LanguageRegistry::get_tree_sitter_language(Language::Rust),
            highlights: std::sync::Arc::from(LanguageRegistry::get_highlight_query(Language::Rust)),
        };
        let mut buffer = Buffer::new_from_str("fn main() {\n  lets x = 1\n}\n");
        let mut highlighter = SyntaxHighlighter::from_definition("nula-like", &definition).unwrap();
        highlighter.parse_rope(&buffer.rope);
        buffer.build_highlight_cache_from_rope(&highlighter);
        buffer.syntax = Some(highlighter);

        buffer.rebuild_viewport_highlight_cache(0, buffer.line_count());
        buffer.pending_rehighlight = true;
        assert!(buffer.rebuild_highlight_cache().is_some());
        assert!(buffer
            .cached_highlights
            .as_ref()
            .is_some_and(|cache| cache.iter().any(|line| !line.is_empty())));
    }
}
