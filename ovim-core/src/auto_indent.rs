//! Structural indentation planning.
//!
//! This module deliberately separates two concerns:
//!
//! - [`IndentOptions`](crate::indentation::IndentOptions) decides how a visual
//!   column is encoded as tabs and spaces.
//! - This module decides which visual column a line belongs at.
//!
//! The structural pass is intentionally conservative. It understands paired
//! delimiters and the comment/string syntax of ovim's built-in languages, but
//! leaves full language formatting to the LSP formatter. In particular,
//! delimiters inside comments and literals never affect surrounding lines.

use crate::buffer::Buffer;
use crate::indentation::{leading_str, IndentOptions};
use crate::syntax::{Language, LanguageRegistry};

/// One immutable line edit in an auto-indent operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedIndent {
    pub(crate) line: usize,
    pub(crate) leading_chars: usize,
    pub(crate) replacement: Option<String>,
    pub(crate) cursor_col: usize,
}

/// Plan a complete reindent before mutating the buffer.
///
/// Scanning always begins at the start of the document. That makes `==` and a
/// selected range observe the same structural context as `=G`.
pub(crate) fn plan(
    buffer: &Buffer,
    start_line: usize,
    end_line: usize,
    options: IndentOptions,
) -> Vec<PlannedIndent> {
    let options = options.normalized();
    let end_line = end_line.min(buffer.line_count());
    if start_line >= end_line {
        return Vec::new();
    }

    let language = buffer
        .file_path()
        .and_then(LanguageRegistry::detect_from_path);
    let profile = LexicalProfile::for_language(language);
    let mut lexer = LexState::default();
    let mut depth = 0usize;
    let mut result = Vec::with_capacity(end_line - start_line);

    for line_idx in 0..end_line {
        let Some(line) = buffer.line_text(line_idx) else {
            continue;
        };
        let scan = scan_line(&line, profile, &mut lexer);
        let target_depth = depth.saturating_sub(scan.leading_closers);

        if line_idx >= start_line {
            let current_prefix = leading_str(&line);
            let target_width = target_depth * options.shift_width;

            // Whitespace-only lines and multiline literal bodies are content,
            // not layout. Preserve them byte-for-byte.
            let desired = if !scan.has_code || scan.literal_continuation {
                current_prefix.to_string()
            } else {
                options.encode_indent(target_width)
            };
            let replacement = (desired != current_prefix).then_some(desired.clone());

            result.push(PlannedIndent {
                line: line_idx,
                leading_chars: current_prefix.chars().count(),
                replacement,
                cursor_col: desired.chars().count(),
            });
        }

        for delimiter in scan.delimiters {
            match delimiter {
                Delimiter::Open(_) => depth += 1,
                Delimiter::Close(_) => depth = depth.saturating_sub(1),
            }
        }
    }

    result
}

/// Return the last structural opening delimiter before an insertion point.
///
/// Earlier lines are scanned only to establish multiline comment/literal
/// state. Delimiters inside those regions are therefore invisible here just
/// as they are to [`plan`].
pub(crate) fn opening_delimiter_at_end(
    buffer: &Buffer,
    line_idx: usize,
    text_before_cursor: &str,
) -> Option<char> {
    let language = buffer
        .file_path()
        .and_then(LanguageRegistry::detect_from_path);
    let profile = LexicalProfile::for_language(language);
    let mut lexer = LexState::default();

    for preceding_line in 0..line_idx.min(buffer.line_count()) {
        if let Some(line) = buffer.line_text(preceding_line) {
            scan_line(&line, profile, &mut lexer);
        }
    }

    let scan = scan_line(text_before_cursor, profile, &mut lexer);
    match scan.delimiters.last() {
        Some(Delimiter::Open(opening)) => Some(*opening),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delimiter {
    Open(char),
    Close(char),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LineScan {
    delimiters: Vec<Delimiter>,
    leading_closers: usize,
    has_code: bool,
    literal_continuation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MultilineContext {
    BlockComment {
        start: &'static str,
        end: &'static str,
        depth: usize,
    },
    Literal {
        end: String,
    },
}

#[derive(Debug, Default)]
struct LexState {
    multiline: Option<MultilineContext>,
}

#[derive(Debug, Clone, Copy)]
struct LexicalProfile {
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
    backtick_literals: bool,
    python_triples: bool,
    rust_raw_literals: bool,
    rust_lifetimes: bool,
    lua_long_literals: bool,
}

const SLASH_LINE: &[&str] = &["//"];
const HASH_LINE: &[&str] = &["#"];
const DASH_LINE: &[&str] = &["--"];
const SEMICOLON_LINE: &[&str] = &[";"];
const HCL_LINE: &[&str] = &["//", "#"];
const SLASH_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
const LUA_BLOCK: &[(&str, &str)] = &[("--[[", "]]")];
const HTML_BLOCK: &[(&str, &str)] = &[("<!--", "-->")];
const NO_LINE_COMMENTS: &[&str] = &[];
const NO_BLOCK_COMMENTS: &[(&str, &str)] = &[];

impl LexicalProfile {
    fn for_language(language: Option<Language>) -> Self {
        let mut profile = Self {
            line_comments: SLASH_LINE,
            block_comments: SLASH_BLOCK,
            backtick_literals: false,
            python_triples: false,
            rust_raw_literals: false,
            rust_lifetimes: false,
            lua_long_literals: false,
        };

        match language {
            Some(Language::Rust) => {
                profile.rust_raw_literals = true;
                profile.rust_lifetimes = true;
            }
            Some(Language::JavaScript | Language::TypeScript | Language::Tsx) => {
                profile.backtick_literals = true;
            }
            Some(Language::Python) => {
                profile.line_comments = HASH_LINE;
                profile.block_comments = NO_BLOCK_COMMENTS;
                profile.python_triples = true;
            }
            Some(
                Language::Ruby
                | Language::Bash
                | Language::Dockerfile
                | Language::Yaml
                | Language::Toml,
            ) => {
                profile.line_comments = HASH_LINE;
                profile.block_comments = NO_BLOCK_COMMENTS;
            }
            Some(Language::Lua) => {
                profile.line_comments = DASH_LINE;
                profile.block_comments = LUA_BLOCK;
                profile.lua_long_literals = true;
            }
            Some(Language::Sql) => {
                profile.line_comments = DASH_LINE;
                profile.block_comments = SLASH_BLOCK;
            }
            Some(Language::Terraform | Language::Hcl) => {
                profile.line_comments = HCL_LINE;
            }
            Some(Language::Html | Language::Markdown) => {
                profile.line_comments = NO_LINE_COMMENTS;
                profile.block_comments = HTML_BLOCK;
            }
            Some(Language::TreeSitterQuery) => {
                profile.line_comments = SEMICOLON_LINE;
                profile.block_comments = NO_BLOCK_COMMENTS;
            }
            _ => {}
        }

        profile
    }
}

fn scan_line(line: &str, profile: LexicalProfile, state: &mut LexState) -> LineScan {
    let bytes = line.as_bytes();
    let mut scan = LineScan {
        literal_continuation: matches!(state.multiline, Some(MultilineContext::Literal { .. })),
        ..LineScan::default()
    };
    let mut leading = true;
    let mut index = 0usize;

    while index < bytes.len() {
        if let Some(context) = state.multiline.take() {
            match context {
                MultilineContext::BlockComment {
                    start,
                    end,
                    mut depth,
                } => {
                    scan.has_code = true;
                    leading = false;
                    while index < bytes.len() {
                        if starts_with(bytes, index, end) {
                            depth -= 1;
                            index += end.len();
                            if depth == 0 {
                                break;
                            }
                        } else if starts_with(bytes, index, start) {
                            depth += 1;
                            index += start.len();
                        } else {
                            index += 1;
                        }
                    }
                    if depth > 0 {
                        state.multiline =
                            Some(MultilineContext::BlockComment { start, end, depth });
                    }
                }
                MultilineContext::Literal { end } => {
                    scan.has_code = true;
                    leading = false;
                    if let Some(offset) = find_bytes(&bytes[index..], end.as_bytes()) {
                        index += offset + end.len();
                    } else {
                        state.multiline = Some(MultilineContext::Literal { end });
                        break;
                    }
                }
            }
            continue;
        }

        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }

        if let Some((start, end)) = profile
            .block_comments
            .iter()
            .copied()
            .find(|(start, _)| starts_with(bytes, index, start))
        {
            scan.has_code = true;
            leading = false;
            index += start.len();
            state.multiline = Some(MultilineContext::BlockComment {
                start,
                end,
                depth: 1,
            });
            continue;
        }

        if profile
            .line_comments
            .iter()
            .any(|marker| starts_with(bytes, index, marker))
        {
            scan.has_code = true;
            break;
        }

        if profile.rust_raw_literals {
            if let Some((opening_len, end)) = rust_raw_literal(bytes, index) {
                scan.has_code = true;
                leading = false;
                index += opening_len;
                if let Some(offset) = find_bytes(&bytes[index..], end.as_bytes()) {
                    index += offset + end.len();
                } else {
                    state.multiline = Some(MultilineContext::Literal { end });
                }
                continue;
            }
        }

        if profile.python_triples {
            let marker = if starts_with(bytes, index, "\"\"\"") {
                Some("\"\"\"")
            } else if starts_with(bytes, index, "'''") {
                Some("'''")
            } else {
                None
            };
            if let Some(marker) = marker {
                scan.has_code = true;
                leading = false;
                index += marker.len();
                if let Some(offset) = find_bytes(&bytes[index..], marker.as_bytes()) {
                    index += offset + marker.len();
                } else {
                    state.multiline = Some(MultilineContext::Literal {
                        end: marker.to_string(),
                    });
                }
                continue;
            }
        }

        if profile.lua_long_literals && starts_with(bytes, index, "[[") {
            scan.has_code = true;
            leading = false;
            index += 2;
            if let Some(offset) = find_bytes(&bytes[index..], b"]]") {
                index += offset + 2;
            } else {
                state.multiline = Some(MultilineContext::Literal {
                    end: "]]".to_string(),
                });
            }
            continue;
        }

        if bytes[index] == b'\''
            || bytes[index] == b'"'
            || (profile.backtick_literals && bytes[index] == b'`')
        {
            let quote = bytes[index];
            scan.has_code = true;
            leading = false;
            if let Some(end) = quoted_literal_end(bytes, index, quote) {
                index = end;
            } else if quote == b'\'' && profile.rust_lifetimes {
                // A Rust lifetime (`'a`) is not a character literal. Treat the
                // apostrophe as ordinary syntax so a later `{` remains visible.
                index += 1;
            } else if quote == b'`' {
                state.multiline = Some(MultilineContext::Literal {
                    end: "`".to_string(),
                });
                break;
            } else {
                // An unclosed single-line literal owns the rest of this line;
                // delimiters in it cannot be structural.
                break;
            }
            continue;
        }

        match bytes[index] {
            byte @ (b'{' | b'(' | b'[') => {
                scan.has_code = true;
                scan.delimiters.push(Delimiter::Open(char::from(byte)));
                leading = false;
            }
            byte @ (b'}' | b')' | b']') => {
                scan.has_code = true;
                scan.delimiters.push(Delimiter::Close(char::from(byte)));
                if leading {
                    scan.leading_closers += 1;
                }
            }
            _ => {
                scan.has_code = true;
                leading = false;
            }
        }
        index += 1;
    }

    scan
}

fn starts_with(bytes: &[u8], index: usize, pattern: &str) -> bool {
    bytes.get(index..index + pattern.len()) == Some(pattern.as_bytes())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn quoted_literal_end(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn rust_raw_literal(bytes: &[u8], index: usize) -> Option<(usize, String)> {
    if index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_') {
        return None;
    }

    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') || bytes.get(cursor) == Some(&b'c') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;

    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }

    let hash_count = cursor - hashes_start;
    cursor += 1;
    Some((cursor - index, format!("\"{}", "#".repeat(hash_count))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scans(lines: &[&str], language: Option<Language>) -> Vec<LineScan> {
        let profile = LexicalProfile::for_language(language);
        let mut state = LexState::default();
        lines
            .iter()
            .map(|line| scan_line(line, profile, &mut state))
            .collect()
    }

    #[test]
    fn ignores_delimiters_in_strings_and_comments() {
        let result = scans(
            &["fn main() {", "let s = \"}\"; // {", "/* [ */", "}"],
            Some(Language::Rust),
        );

        assert_eq!(
            result[0].delimiters,
            vec![
                Delimiter::Open('('),
                Delimiter::Close(')'),
                Delimiter::Open('{')
            ]
        );
        assert!(result[1].delimiters.is_empty());
        assert!(result[2].delimiters.is_empty());
        assert_eq!(result[3].delimiters, vec![Delimiter::Close('}')]);
    }

    #[test]
    fn protects_multiline_literal_bodies() {
        let result = scans(
            &[
                "let text = r#\"{",
                "  literal indentation",
                "}\"#;",
                "call();",
            ],
            Some(Language::Rust),
        );

        assert!(!result[0].literal_continuation);
        assert!(result[1].literal_continuation);
        assert!(result[2].literal_continuation);
        assert!(!result[3].literal_continuation);
        assert!(result[..3].iter().all(|line| line.delimiters.is_empty()));
        assert_eq!(
            result[3].delimiters,
            vec![Delimiter::Open('('), Delimiter::Close(')')]
        );
    }

    #[test]
    fn uses_hash_comments_only_for_relevant_languages() {
        let python = scans(&["# {"], Some(Language::Python));
        let rust = scans(&["# {"], Some(Language::Rust));

        assert!(python[0].delimiters.is_empty());
        assert_eq!(rust[0].delimiters, vec![Delimiter::Open('{')]);
    }

    #[test]
    fn rust_lifetimes_do_not_hide_following_delimiters() {
        let result = scans(&["fn get<'a>() {"], Some(Language::Rust));

        assert_eq!(
            result[0].delimiters,
            vec![
                Delimiter::Open('('),
                Delimiter::Close(')'),
                Delimiter::Open('{')
            ]
        );
    }

    #[test]
    fn finds_opening_delimiter_before_a_trailing_comment() {
        let mut buffer = Buffer::new_from_str("fn main() { // reason\n");
        buffer.set_file_path("/tmp/main.rs".to_string());

        assert_eq!(
            opening_delimiter_at_end(&buffer, 0, "fn main() { // reason"),
            Some('{')
        );
    }

    #[test]
    fn ignores_opening_delimiter_in_a_comment() {
        let mut buffer = Buffer::new_from_str("// {\n");
        buffer.set_file_path("/tmp/main.rs".to_string());

        assert_eq!(opening_delimiter_at_end(&buffer, 0, "// {"), None);
    }
}
