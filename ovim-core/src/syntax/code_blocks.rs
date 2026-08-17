//! Code block syntax highlighting for markdown files
//!
//! This module provides syntax highlighting for fenced code blocks within markdown.
//! It parses the markdown tree to find code blocks, extracts language info strings,
//! creates temporary highlighters for each language, and caches the results.
//!
//! Info strings resolve through a [`LanguageCatalog`], so fences of
//! plugin-registered languages highlight the same way built-ins do.

use crate::language_catalog::LanguageCatalog;

use super::highlighter::SyntaxHighlighter;
use super::theme::HighlightGroup;
use std::ops::Range;
use tree_sitter::Tree;

/// A single code block with its language-specific highlights
#[derive(Debug)]
pub struct CodeBlock {
    /// First line of code content (0-indexed, inside the block, not the fence)
    pub line_start: usize,
    /// Last line of code content (exclusive)
    pub line_end: usize,
    /// Language id resolved from the info string. Production code only
    /// consumes the `highlights` derived from this language, but tests assert
    /// on it directly to confirm the info-string-to-language mapping works.
    #[cfg_attr(not(test), allow(dead_code))]
    pub language_id: String,
    /// Syntax highlights for each line within the block
    /// Index 0 = line_start, Index 1 = line_start + 1, etc.
    pub highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
}

/// Cache for code block syntax highlighting in markdown files
#[derive(Debug, Default)]
pub struct CodeBlockCache {
    /// All code blocks found in the document
    blocks: Vec<CodeBlock>,
    /// Version counter for cache invalidation
    version: u64,
}

impl CodeBlockCache {
    /// Creates a new empty cache
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the cache from a markdown parse tree
    ///
    /// Walks the tree to find `fenced_code_block` nodes, extracts language info,
    /// parses code content with language-specific highlighter, and caches results.
    /// Info strings resolve against `catalog`, so plugin languages participate.
    pub fn update_from_tree(
        &mut self,
        tree: &Tree,
        source: &str,
        version: u64,
        catalog: &LanguageCatalog,
    ) {
        self.blocks.clear();
        self.version = version;

        let root = tree.root_node();
        let mut cursor = root.walk();

        // Find all fenced_code_block nodes
        self.visit_node(&mut cursor, source, catalog);
        // Tree traversal is document-ordered today, but lookup relies on this
        // invariant and plugin grammars may produce different node layouts.
        self.blocks.sort_by_key(|block| block.line_start);
    }

    /// Recursively visit nodes to find fenced code blocks
    fn visit_node(
        &mut self,
        cursor: &mut tree_sitter::TreeCursor,
        source: &str,
        catalog: &LanguageCatalog,
    ) {
        let node = cursor.node();

        if node.kind() == "fenced_code_block" {
            self.process_code_block(node, source, catalog);
        }

        // Visit children
        if cursor.goto_first_child() {
            loop {
                self.visit_node(cursor, source, catalog);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    /// Process a single fenced_code_block node
    fn process_code_block(
        &mut self,
        node: tree_sitter::Node,
        source: &str,
        catalog: &LanguageCatalog,
    ) {
        let mut info_string: Option<&str> = None;
        let mut code_content: Option<&str> = None;
        let mut code_start_line: Option<usize> = None;

        // Iterate through children to find info_string and code_fence_content
        let mut child_cursor = node.walk();
        if child_cursor.goto_first_child() {
            loop {
                let child = child_cursor.node();
                match child.kind() {
                    "info_string" => {
                        // Extract the language identifier
                        // info_string may contain "language" child or just text
                        let text = &source[child.byte_range()];
                        info_string = Some(text.trim());
                    }
                    "code_fence_content" => {
                        code_content = Some(&source[child.byte_range()]);
                        code_start_line = Some(child.start_position().row);
                    }
                    _ => {}
                }
                if !child_cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        // Need both language and code content
        let Some(info) = info_string else { return };
        let Some(code) = code_content else { return };
        let Some(start_line) = code_start_line else {
            return;
        };

        // Skip empty code blocks
        if code.trim().is_empty() {
            return;
        }

        // The info string may carry attributes after the language name
        // (e.g. "rust {.line-numbers}", "python title=foo"). The language is the
        // first whitespace/comma-delimited token; using the whole string would
        // fail to match and leave the block unhighlighted.
        let lang_token = info.split([' ', '\t', ',']).next().unwrap_or("").trim();

        // Try to get a language from the info string
        let Some(language) = catalog.detect_from_info_string(lang_token) else {
            return;
        };
        let Some(syntax) = language.syntax.as_ref() else {
            return;
        };

        // Create a temporary highlighter for this language
        let Ok(mut highlighter) = SyntaxHighlighter::from_definition(language.id(), syntax) else {
            return;
        };

        // Parse the code content
        highlighter.parse(code);

        // Get highlights for all lines
        let highlights = highlighter.highlights_for_all_lines(code);

        // Calculate line range
        let line_count = code.lines().count();
        let line_end = start_line + line_count;

        self.blocks.push(CodeBlock {
            line_start: start_line,
            line_end,
            language_id: language.id().to_string(),
            highlights,
        });
    }

    /// Gets highlights for a specific line if it's inside a code block
    ///
    /// Returns None if the line is not inside any code block.
    /// The returned highlights are ready to use (column ranges are correct).
    pub fn highlights_for_line(
        &self,
        line_idx: usize,
    ) -> Option<&Vec<(Range<usize>, HighlightGroup)>> {
        let block = self.block_for_line(line_idx)?;
        block.highlights.get(line_idx - block.line_start)
    }

    /// Returns the cache version
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Checks if a line is inside any code block
    pub fn is_line_in_code_block(&self, line_idx: usize) -> bool {
        self.block_for_line(line_idx).is_some()
    }

    /// Finds the only block that could contain `line_idx` in O(log n).
    fn block_for_line(&self, line_idx: usize) -> Option<&CodeBlock> {
        let insertion = self
            .blocks
            .partition_point(|block| block.line_start <= line_idx);
        let block = self.blocks.get(insertion.checked_sub(1)?)?;
        (line_idx < block.line_end).then_some(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn parse_markdown(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_md::LANGUAGE.into())
            .expect("Failed to set markdown language");
        parser.parse(source, None).expect("Failed to parse")
    }

    fn cache_for(source: &str, catalog: &LanguageCatalog) -> CodeBlockCache {
        let tree = parse_markdown(source);
        let mut cache = CodeBlockCache::new();
        cache.update_from_tree(&tree, source, 1, catalog);
        cache
    }

    #[test]
    fn test_rust_code_block_detection() {
        let source = r#"# Heading

```rust
fn main() {
    println!("Hello");
}
```

Some text.
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        assert_eq!(cache.blocks.len(), 1);
        assert_eq!(cache.blocks[0].language_id, "rust");
        assert_eq!(cache.blocks[0].line_start, 3); // Line after ```rust
    }

    #[test]
    fn test_highlights_for_line() {
        let source = r#"# Test

```rust
let x = 42;
```
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        // Line 3 is "let x = 42;" - should have highlights
        let highlights = cache.highlights_for_line(3);
        assert!(highlights.is_some());

        // Line 1 is "# Test" - not in code block
        let highlights = cache.highlights_for_line(1);
        assert!(highlights.is_none());
    }

    #[test]
    fn block_lookup_handles_boundaries_and_gaps() {
        let source = r#"```rust
let first = 1;
```

plain text

```python
second = 2
third = 3
```
"#;
        let cache = cache_for(source, &LanguageCatalog::built_in());

        assert!(cache.is_line_in_code_block(1));
        assert!(!cache.is_line_in_code_block(2));
        assert!(!cache.is_line_in_code_block(5));
        assert!(cache.is_line_in_code_block(7));
        assert!(cache.is_line_in_code_block(8));
        assert!(!cache.is_line_in_code_block(9));
        assert!(cache.highlights_for_line(7).is_some());
        assert!(cache.highlights_for_line(5).is_none());
    }

    #[test]
    fn test_multiple_code_blocks() {
        let source = r#"# Multiple

```bash
echo "hello"
```

```python
print("world")
```
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        assert_eq!(cache.blocks.len(), 2);
        assert_eq!(cache.blocks[0].language_id, "bash");
        assert_eq!(cache.blocks[1].language_id, "python");
    }

    #[test]
    fn test_alias_info_string_resolves() {
        let source = r#"```js
const x = 42;
```
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        assert_eq!(cache.blocks.len(), 1);
        assert_eq!(cache.blocks[0].language_id, "javascript");
    }

    #[test]
    fn test_plugin_language_fence_highlights() {
        use crate::language_catalog::{LanguageDefinition, RegistrationOwner, SyntaxDefinition};
        use crate::language_config::LanguageConfig;
        use crate::syntax::{Language, LanguageRegistry};

        // A plugin-style registration reusing a built-in grammar under a
        // dynamic id, as `register_dynamic` would produce.
        let catalog = LanguageCatalog::built_in();
        catalog.insert_for_tests(Arc::new(LanguageDefinition {
            config: LanguageConfig {
                id: "nulatest".into(),
                name: "Nulatest".into(),
                extensions: vec!["nulatest".into()],
                filenames: Vec::new(),
                path_filenames: Vec::new(),
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
            lsp_language_id: "nulatest".into(),
            syntax: Some(SyntaxDefinition {
                language: LanguageRegistry::get_tree_sitter_language(Language::Rust),
                highlights: Arc::from(LanguageRegistry::get_highlight_query(Language::Rust)),
            }),
            owner: RegistrationOwner::UserConfig {
                source: "/tmp/init.lua".into(),
            },
            source: "/tmp".into(),
        }));

        let source = r#"# Docs

```nulatest
fn main() {}
```
"#;

        let cache = cache_for(source, &catalog);

        assert_eq!(cache.blocks.len(), 1);
        assert_eq!(cache.blocks[0].language_id, "nulatest");
        assert!(cache
            .highlights_for_line(3)
            .is_some_and(|line| !line.is_empty()));
    }

    #[test]
    fn test_unknown_language_skipped() {
        let source = r#"```unknown
some code
```
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        // Unknown language should be skipped
        assert_eq!(cache.blocks.len(), 0);
    }

    #[test]
    fn test_empty_code_block_skipped() {
        let source = r#"```rust
```
"#;

        let cache = cache_for(source, &LanguageCatalog::built_in());

        // Empty code block should be skipped
        assert_eq!(cache.blocks.len(), 0);
    }
}
