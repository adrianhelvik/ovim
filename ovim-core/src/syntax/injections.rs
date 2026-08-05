//! Embedded-language injection highlighting.
//!
//! Some grammars (Astro's frontmatter/script/style blocks, HTML/JS
//! interpolations) split embedded regions out into raw-text nodes rather
//! than highlighting them directly — the region needs to be re-parsed with
//! a different grammar entirely. Grammars that need this ship a
//! `injections.scm` query (`LanguageRegistry::get_injection_query`) that
//! marks each such region with a `#set! injection.language "..."`
//! directive; this module walks that query, parses each captured region
//! with the named language's own highlighter, and reports the results back
//! in the outer buffer's line/column coordinate space so callers can
//! overlay them onto the base highlights.

use crate::language_catalog::LanguageCatalog;

use super::highlighter::SyntaxHighlighter;
use super::theme::HighlightGroup;
use std::ops::Range;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor, Tree};

/// A single embedded-language region with its own highlights, already
/// translated into the outer document's line/column coordinates.
#[derive(Debug)]
struct InjectedRegion {
    /// First line covered by this region (0-indexed).
    line_start: usize,
    /// One past the last line covered by this region.
    line_end: usize,
    /// Per-line highlights, index 0 == `line_start`.
    highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
}

/// Cache of embedded-language injection highlights for a single buffer.
#[derive(Debug, Default)]
pub struct InjectionCache {
    regions: Vec<InjectedRegion>,
    version: u64,
}

impl InjectionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the cache from the host tree by running `injection_query`
    /// against it, re-parsing each captured region with the language named
    /// in its `#set! injection.language` directive.
    pub fn update_from_tree(
        &mut self,
        tree: &Tree,
        source: &str,
        injection_query: &Query,
        version: u64,
        catalog: &LanguageCatalog,
    ) {
        self.regions.clear();
        self.version = version;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(injection_query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            let language_name = injection_query
                .property_settings(m.pattern_index)
                .iter()
                .find(|prop| &*prop.key == "injection.language")
                .and_then(|prop| prop.value.as_deref());
            let Some(language_name) = language_name else {
                continue;
            };
            let Some(language) = catalog.detect_from_info_string(language_name) else {
                continue;
            };
            let Some(syntax) = language.syntax.as_ref() else {
                continue;
            };

            for capture in m.captures {
                let node = capture.node;
                let byte_range = node.byte_range();
                if byte_range.is_empty() {
                    continue;
                }
                let content = &source[byte_range];
                if content.trim().is_empty() {
                    continue;
                }

                let Ok(mut highlighter) = SyntaxHighlighter::from_definition(language.id(), syntax)
                else {
                    continue;
                };
                highlighter.parse(content);
                let local_highlights = highlighter.highlights_for_all_lines(content);
                if local_highlights.is_empty() {
                    continue;
                }

                // `Point::column` is a byte offset within the row here, since
                // parsing runs over UTF-8 bytes throughout — same convention
                // the local highlighter's own byte ranges use, so the two
                // compose with plain addition.
                let start = node.start_position();
                let line_start = start.row;
                let line_end = line_start + local_highlights.len();

                let highlights = local_highlights
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let col_offset = if i == 0 { start.column } else { 0 };
                        line.into_iter()
                            .map(|(range, group)| {
                                (range.start + col_offset..range.end + col_offset, group)
                            })
                            .collect()
                    })
                    .collect();

                self.regions.push(InjectedRegion {
                    line_start,
                    line_end,
                    highlights,
                });
            }
        }
    }

    /// Injected highlights for `line_idx`, if any injection covers it.
    /// Column ranges are in the outer document's byte-offset-within-line
    /// space, ready to overlay onto that line's base highlights.
    pub fn highlights_for_line(
        &self,
        line_idx: usize,
    ) -> Option<&Vec<(Range<usize>, HighlightGroup)>> {
        for region in &self.regions {
            if line_idx >= region.line_start && line_idx < region.line_end {
                let rel = line_idx - region.line_start;
                if rel < region.highlights.len() {
                    return Some(&region.highlights[rel]);
                }
            }
        }
        None
    }

    /// Returns the cache version.
    pub fn version(&self) -> u64 {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Language, LanguageRegistry};

    fn injection_cache_for(source: &str) -> InjectionCache {
        let ts_language = LanguageRegistry::get_tree_sitter_language(Language::Astro);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let injection_query = Query::new(
            &ts_language,
            LanguageRegistry::get_injection_query(Language::Astro).unwrap(),
        )
        .unwrap();

        let mut cache = InjectionCache::new();
        cache.update_from_tree(
            &tree,
            source,
            &injection_query,
            1,
            &LanguageCatalog::built_in(),
        );
        cache
    }

    #[test]
    fn frontmatter_js_is_highlighted() {
        let source = "---\nconst title = \"Hello\";\n---\n\n<h1>{title}</h1>\n";
        let cache = injection_cache_for(source);

        // Line 1 is `const title = "Hello";` inside the frontmatter block.
        let highlights = cache
            .highlights_for_line(1)
            .expect("frontmatter line should have injected highlights");
        assert!(highlights
            .iter()
            .any(|(_, group)| *group == HighlightGroup::Keyword));
        assert!(highlights
            .iter()
            .any(|(_, group)| *group == HighlightGroup::String));
    }

    #[test]
    fn script_element_content_is_highlighted() {
        let source = "---\n---\n<script>\nconst x = 1;\n</script>\n";
        let cache = injection_cache_for(source);

        let highlights = cache
            .highlights_for_line(3)
            .expect("script content line should have injected highlights");
        assert!(highlights
            .iter()
            .any(|(_, group)| *group == HighlightGroup::Keyword));
    }

    #[test]
    fn html_line_outside_injection_has_no_entry() {
        let source = "---\n---\n<h1>Hi</h1>\n";
        let cache = injection_cache_for(source);
        assert!(cache.highlights_for_line(2).is_none());
    }
}
