//! Shared planning engine for the line-based editing commands (`edit`,
//! `insert`, `delete-lines`).
//!
//! File mode (`subcommands.rs`) and session mode (`event_loop.rs` API
//! handlers) are two front ends for one contract. Before this module they
//! were independent implementations, and semantics drifted: the uniqueness
//! guard, multi-line matching, CRLF preservation, and append-past-EOF
//! newline handling each existed in one copy but not the other
//! (OV-00279/280/284/285 — see OV-00298).
//!
//! The engine plans an edit as a [`Splice`] — a char-offset range to
//! replace and its replacement text — against the full buffer/file
//! content. Char offsets apply directly to a `ropey::Rope` (session mode)
//! and convert to byte offsets for `String` splicing (file mode, via
//! [`apply_splice`]). All validation and error wording lives here so both
//! modes stay byte-for-byte identical.
//!
//! Lines are 1-indexed at this API, matching the CLI surface.

use std::fmt;

/// A planned text replacement: replace chars `[start_char, end_char)` with
/// `text`. Offsets are CHAR offsets into the content the plan was computed
/// from (rope-compatible).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
}

/// Where to insert lines (1-indexed, CLI semantics).
/// `After(0)` inserts before the first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertAt {
    After(usize),
    Before(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    EmptyOld,
    MultiLineOldWithLine,
    LineOutOfRange {
        line: usize,
        line_count: usize,
    },
    NotFound {
        old: String,
    },
    NotFoundOnLine {
        line: usize,
        old: String,
        line_content: String,
    },
    AmbiguousOnLine {
        line: usize,
        old: String,
        count: usize,
    },
    /// `lines` holds the 1-indexed line and trimmed content of each match.
    Ambiguous {
        old: String,
        count: usize,
        lines: Vec<(usize, String)>,
    },
    AfterOutOfRange {
        line: usize,
        line_count: usize,
    },
    BeforeOutOfRange {
        line: usize,
        line_count: usize,
    },
    OneIndexed {
        from: usize,
        to: usize,
    },
    RangeOutOfRange {
        from: usize,
        to: usize,
        line_count: usize,
    },
    InvertedRange {
        from: usize,
        to: usize,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOld => write!(f, "--old must not be empty"),
            Self::MultiLineOldWithLine => {
                write!(f, "Multi-line --old cannot be combined with --line")
            }
            Self::LineOutOfRange { line, line_count } => {
                write!(
                    f,
                    "Line {} out of range (file has {} lines)",
                    line, line_count
                )
            }
            Self::NotFound { old } => write!(f, "Text not found in file: {:?}", old),
            Self::NotFoundOnLine {
                line,
                old,
                line_content,
            } => write!(
                f,
                "Text not found on line {}: {:?}\nLine content: {:?}",
                line, old, line_content
            ),
            Self::AmbiguousOnLine { line, old, count } => write!(
                f,
                "Text {:?} found {} times on line {}. Be more specific.",
                old, count, line
            ),
            Self::Ambiguous { old, count, lines } => {
                let listing: Vec<String> = lines
                    .iter()
                    .map(|(line, content)| format!("  line {}: {}", line, content))
                    .collect();
                write!(
                    f,
                    "Text {:?} found {} times. Use --line to specify which occurrence:\n{}",
                    old,
                    count,
                    listing.join("\n")
                )
            }
            Self::AfterOutOfRange { line, line_count } => write!(
                f,
                "Line {} out of range (file has {} lines). Use --after 0 to insert at start.",
                line, line_count
            ),
            Self::BeforeOutOfRange { line, line_count } => {
                write!(
                    f,
                    "Line {} out of range (file has {} lines)",
                    line, line_count
                )
            }
            Self::OneIndexed { from, to } => write!(
                f,
                "Line numbers are 1-indexed (got from={}, to={})",
                from, to
            ),
            Self::RangeOutOfRange {
                from,
                to,
                line_count,
            } => write!(
                f,
                "Line range {}-{} out of range (file has {} lines)",
                from, to, line_count
            ),
            Self::InvertedRange { from, to } => {
                write!(f, "--from ({}) must be <= --to ({})", from, to)
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// The separator to join lines with when composing inserted text: preserves
/// CRLF files instead of rewriting line endings (OV-00280). Mixed-ending
/// files are normalized to the dominant style for NEW lines only — the
/// splice model never touches untouched lines.
pub fn line_separator(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Number of logical lines. Empty content has 0 lines (nothing to address);
/// a lone `"\n"` has 1. This is `str::lines` semantics — no phantom
/// trailing line, unlike `Rope::len_lines`.
fn line_count(content: &str) -> usize {
    content.lines().count()
}

/// Char offset of the start of 0-indexed line `idx`. `idx == number of
/// lines` yields the offset one past the final terminator (== content char
/// length when the content ends with a newline).
fn line_start_char(content: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (char_pos, ch) in content.chars().enumerate() {
        if ch == '\n' {
            seen += 1;
            if seen == idx {
                return char_pos + 1;
            }
        }
    }
    content.chars().count()
}

/// The content of 0-indexed line `idx` without its terminator (`\n` or
/// `\r\n`), plus the char offset of the line start.
fn line_content_and_start(content: &str, idx: usize) -> (String, usize) {
    let start = line_start_char(content, idx);
    let line: String = content
        .chars()
        .skip(start)
        .take_while(|&c| c != '\n')
        .collect();
    let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
    (line, start)
}

/// Char offsets of each occurrence of `needle` in `haystack`.
/// (`str::find` returns byte offsets; rope operations need chars.)
fn find_char_positions(haystack: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut byte_start = 0;
    while let Some(rel) = haystack[byte_start..].find(needle) {
        let abs_byte = byte_start + rel;
        out.push(haystack[..abs_byte].chars().count());
        byte_start = abs_byte + needle.len();
    }
    out
}

/// Plans a unique find-and-replace. `line` is 1-indexed; multi-line `old`
/// (containing `\n`) is matched against the whole content and cannot be
/// combined with `line`.
pub fn plan_edit(
    content: &str,
    line: Option<usize>,
    old: &str,
    new: &str,
) -> Result<(Splice, usize), PlanError> {
    if old.is_empty() {
        return Err(PlanError::EmptyOld);
    }
    let old_chars = old.chars().count();
    let total = line_count(content);

    if old.contains('\n') {
        if line.is_some() {
            return Err(PlanError::MultiLineOldWithLine);
        }
        let matches = find_char_positions(content, old);
        return match matches.len() {
            0 => Err(PlanError::NotFound {
                old: old.to_string(),
            }),
            1 => {
                let start = matches[0];
                let match_line = content.chars().take(start).filter(|&c| c == '\n').count() + 1;
                Ok((
                    Splice {
                        start_char: start,
                        end_char: start + old_chars,
                        text: new.to_string(),
                    },
                    match_line,
                ))
            }
            count => {
                let lines = matches
                    .iter()
                    .map(|&start| {
                        let line_idx = content.chars().take(start).filter(|&c| c == '\n').count();
                        let (line_content, _) = line_content_and_start(content, line_idx);
                        (line_idx + 1, line_content.trim().to_string())
                    })
                    .collect();
                Err(PlanError::Ambiguous {
                    old: old.to_string(),
                    count,
                    lines,
                })
            }
        };
    }

    if let Some(line_num) = line {
        if line_num == 0 || line_num > total {
            return Err(PlanError::LineOutOfRange {
                line: line_num,
                line_count: total,
            });
        }
        let (line_content, line_start) = line_content_and_start(content, line_num - 1);
        let matches = find_char_positions(&line_content, old);
        return match matches.len() {
            0 => Err(PlanError::NotFoundOnLine {
                line: line_num,
                old: old.to_string(),
                line_content,
            }),
            1 => {
                let start = line_start + matches[0];
                Ok((
                    Splice {
                        start_char: start,
                        end_char: start + old_chars,
                        text: new.to_string(),
                    },
                    line_num,
                ))
            }
            count => Err(PlanError::AmbiguousOnLine {
                line: line_num,
                old: old.to_string(),
                count,
            }),
        };
    }

    // No line given: single-line needle, searched per line across the file.
    let mut matches: Vec<(usize, usize, String)> = Vec::new(); // (line 1-idx, abs char, content)
    for idx in 0..total {
        let (line_content, line_start) = line_content_and_start(content, idx);
        for col in find_char_positions(&line_content, old) {
            matches.push((idx + 1, line_start + col, line_content.clone()));
        }
    }
    match matches.len() {
        0 => Err(PlanError::NotFound {
            old: old.to_string(),
        }),
        1 => {
            let (match_line, start, _) = matches.remove(0);
            Ok((
                Splice {
                    start_char: start,
                    end_char: start + old_chars,
                    text: new.to_string(),
                },
                match_line,
            ))
        }
        count => Err(PlanError::Ambiguous {
            old: old.to_string(),
            count,
            lines: matches
                .into_iter()
                .map(|(line, _, content)| (line, content.trim().to_string()))
                .collect(),
        }),
    }
}

/// Plans a line insertion. Returns the splice and the number of lines
/// inserted. Preserves the file's line-ending style and its (missing)
/// trailing newline: appending past a terminator-less last line opens a
/// new line instead of splicing into the old one (OV-00279).
pub fn plan_insert(content: &str, at: InsertAt, text: &str) -> Result<(Splice, usize), PlanError> {
    let total = line_count(content);
    let after = match at {
        InsertAt::After(n) => {
            if n > total {
                return Err(PlanError::AfterOutOfRange {
                    line: n,
                    line_count: total,
                });
            }
            n
        }
        InsertAt::Before(n) => {
            if n == 0 || n > total + 1 {
                return Err(PlanError::BeforeOutOfRange {
                    line: n,
                    line_count: total,
                });
            }
            n - 1
        }
    };

    let sep = line_separator(content);
    let insert_lines: Vec<&str> = text.lines().collect();
    let insert_count = insert_lines.len();
    let joined = insert_lines.join(sep);

    let content_chars = content.chars().count();
    let splice = if after == total {
        // Append at end of content.
        if content.is_empty() {
            Splice {
                start_char: 0,
                end_char: 0,
                text: joined,
            }
        } else if content.ends_with('\n') {
            Splice {
                start_char: content_chars,
                end_char: content_chars,
                text: format!("{}{}", joined, sep),
            }
        } else {
            // Terminator-less last line: open a new line, keep the file's
            // missing trailing newline (OV-00279).
            Splice {
                start_char: content_chars,
                end_char: content_chars,
                text: format!("{}{}", sep, joined),
            }
        }
    } else {
        let start = line_start_char(content, after);
        Splice {
            start_char: start,
            end_char: start,
            text: format!("{}{}", joined, sep),
        }
    };

    Ok((splice, insert_count))
}

/// Plans a 1-indexed inclusive line-range deletion. Returns the splice and
/// the number of lines deleted.
pub fn plan_delete_lines(
    content: &str,
    from: usize,
    to: usize,
) -> Result<(Splice, usize), PlanError> {
    if from == 0 || to == 0 {
        return Err(PlanError::OneIndexed { from, to });
    }
    let total = line_count(content);
    if from > total || to > total {
        return Err(PlanError::RangeOutOfRange {
            from,
            to,
            line_count: total,
        });
    }
    if from > to {
        return Err(PlanError::InvertedRange { from, to });
    }

    let mut start = line_start_char(content, from - 1);
    let end = if to == total {
        content.chars().count()
    } else {
        line_start_char(content, to)
    };

    // Deleting through a terminator-less final line must also remove the
    // separator BEFORE the deleted block, or the result gains a trailing
    // newline the file never had.
    if to == total && !content.ends_with('\n') && from > 1 {
        let prefix: Vec<char> = content.chars().take(start).collect();
        if prefix.last() == Some(&'\n') {
            start -= 1;
            if prefix.len() >= 2 && prefix[prefix.len() - 2] == '\r' {
                start -= 1;
            }
        }
    }

    Ok((
        Splice {
            start_char: start,
            end_char: end,
            text: String::new(),
        },
        to - from + 1,
    ))
}

/// Applies a splice to string content (file mode). Char offsets are
/// converted to byte offsets here; rope-backed callers use the char
/// offsets directly.
pub fn apply_splice(content: &str, splice: &Splice) -> String {
    let byte_of = |char_idx: usize| -> usize {
        content
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(content.len())
    };
    let start = byte_of(splice.start_char);
    let end = byte_of(splice.end_char);
    let mut out = String::with_capacity(content.len() + splice.text.len());
    out.push_str(&content[..start]);
    out.push_str(&splice.text);
    out.push_str(&content[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(content: &str, line: Option<usize>, old: &str, new: &str) -> Result<String, String> {
        plan_edit(content, line, old, new)
            .map(|(splice, _)| apply_splice(content, &splice))
            .map_err(|e| e.to_string())
    }

    fn insert(content: &str, at: InsertAt, text: &str) -> Result<String, String> {
        plan_insert(content, at, text)
            .map(|(splice, _)| apply_splice(content, &splice))
            .map_err(|e| e.to_string())
    }

    fn delete(content: &str, from: usize, to: usize) -> Result<String, String> {
        plan_delete_lines(content, from, to)
            .map(|(splice, _)| apply_splice(content, &splice))
            .map_err(|e| e.to_string())
    }

    // ── plan_edit ───────────────────────────────────────────────────────────

    #[test]
    fn edit_unique_whole_file() {
        assert_eq!(edit("a foo b\n", None, "foo", "bar").unwrap(), "a bar b\n");
    }

    #[test]
    fn edit_not_found() {
        assert!(edit("abc\n", None, "zzz", "y")
            .unwrap_err()
            .contains("not found"));
    }

    #[test]
    fn edit_ambiguous_lists_lines() {
        let err = edit("foo\nx foo\n", None, "foo", "y").unwrap_err();
        assert!(err.contains("found 2 times"), "{err}");
        assert!(err.contains("line 1: foo"), "{err}");
        assert!(err.contains("line 2: x foo"), "{err}");
    }

    #[test]
    fn edit_line_scoped() {
        assert_eq!(
            edit("foo\nfoo\n", Some(2), "foo", "bar").unwrap(),
            "foo\nbar\n"
        );
    }

    #[test]
    fn edit_line_scoped_ambiguous() {
        let err = edit("foo foo\n", Some(1), "foo", "y").unwrap_err();
        assert!(err.contains("found 2 times on line 1"), "{err}");
    }

    #[test]
    fn edit_line_out_of_range() {
        let err = edit("a\n", Some(3), "a", "b").unwrap_err();
        assert!(err.contains("Line 3 out of range"), "{err}");
    }

    #[test]
    fn edit_multiline_old() {
        assert_eq!(
            edit("alpha\nbeta\ngamma", None, "alpha\nbeta", "X").unwrap(),
            "X\ngamma"
        );
    }

    #[test]
    fn edit_multiline_old_with_line_rejected() {
        let err = edit("a\nb\n", Some(1), "a\nb", "X").unwrap_err();
        assert!(err.contains("Multi-line"), "{err}");
    }

    #[test]
    fn edit_empty_old_rejected() {
        assert!(edit("a\n", None, "", "b").unwrap_err().contains("--old"));
    }

    #[test]
    fn edit_preserves_crlf() {
        assert_eq!(
            edit("one\r\ntwo\r\n", Some(2), "two", "TWO").unwrap(),
            "one\r\nTWO\r\n"
        );
    }

    #[test]
    fn edit_multibyte_positions() {
        // Multibyte chars before the match must not skew the splice.
        assert_eq!(
            edit("héllo wörld\n", None, "wörld", "world").unwrap(),
            "héllo world\n"
        );
    }

    // ── plan_insert ─────────────────────────────────────────────────────────

    #[test]
    fn insert_after_zero_prepends() {
        assert_eq!(
            insert("a\nb\n", InsertAt::After(0), "X").unwrap(),
            "X\na\nb\n"
        );
    }

    #[test]
    fn insert_after_middle() {
        assert_eq!(
            insert("a\nb\n", InsertAt::After(1), "X").unwrap(),
            "a\nX\nb\n"
        );
    }

    #[test]
    fn insert_after_last_with_trailing_newline() {
        assert_eq!(
            insert("a\nb\n", InsertAt::After(2), "X").unwrap(),
            "a\nb\nX\n"
        );
    }

    #[test]
    fn insert_after_last_without_trailing_newline() {
        // OV-00279: opens a new line, keeps the missing trailing newline.
        assert_eq!(insert("a\nb", InsertAt::After(2), "X").unwrap(), "a\nb\nX");
    }

    #[test]
    fn insert_into_empty_content() {
        assert_eq!(insert("", InsertAt::After(0), "X").unwrap(), "X");
    }

    #[test]
    fn insert_before_first() {
        assert_eq!(insert("a\n", InsertAt::Before(1), "X").unwrap(), "X\na\n");
    }

    #[test]
    fn insert_before_past_end_is_append() {
        assert_eq!(insert("a\nb", InsertAt::Before(3), "X").unwrap(), "a\nb\nX");
    }

    #[test]
    fn insert_multi_line_text() {
        assert_eq!(
            insert("a\nb\n", InsertAt::After(1), "X\nY").unwrap(),
            "a\nX\nY\nb\n"
        );
    }

    #[test]
    fn insert_preserves_crlf() {
        assert_eq!(
            insert("a\r\nb\r\n", InsertAt::After(1), "X").unwrap(),
            "a\r\nX\r\nb\r\n"
        );
    }

    #[test]
    fn insert_after_out_of_range() {
        let err = insert("a\n", InsertAt::After(5), "X").unwrap_err();
        assert!(err.contains("--after 0"), "{err}");
    }

    #[test]
    fn insert_before_zero_rejected() {
        assert!(insert("a\n", InsertAt::Before(0), "X").is_err());
    }

    // ── plan_delete_lines ───────────────────────────────────────────────────

    #[test]
    fn delete_middle_line() {
        assert_eq!(delete("a\nb\nc\n", 2, 2).unwrap(), "a\nc\n");
    }

    #[test]
    fn delete_last_line_with_trailing_newline() {
        assert_eq!(delete("a\nb\nc\n", 3, 3).unwrap(), "a\nb\n");
    }

    #[test]
    fn delete_last_line_without_trailing_newline() {
        // Removing the last line also removes the separator before it.
        assert_eq!(delete("a\nb\nc", 3, 3).unwrap(), "a\nb");
    }

    #[test]
    fn delete_all_lines() {
        assert_eq!(delete("a\nb\nc", 1, 3).unwrap(), "");
        assert_eq!(delete("a\nb\nc\n", 1, 3).unwrap(), "");
    }

    #[test]
    fn delete_crlf_last_line_without_trailing_newline() {
        assert_eq!(delete("a\r\nb\r\nc", 3, 3).unwrap(), "a\r\nb");
    }

    #[test]
    fn delete_zero_index_rejected() {
        assert!(delete("a\n", 0, 1).unwrap_err().contains("1-indexed"));
    }

    #[test]
    fn delete_out_of_range_rejected() {
        assert!(delete("a\n", 1, 5).unwrap_err().contains("out of range"));
    }

    #[test]
    fn delete_inverted_rejected() {
        assert!(delete("a\nb\n", 2, 1).unwrap_err().contains("must be <="));
    }

    // ── find_char_positions (moved from event_loop.rs, OV-00243) ───────────

    #[test]
    fn find_char_positions_returns_char_offsets_not_bytes() {
        // `é` is 2 bytes in UTF-8 but 1 char. `str::find` returns byte offsets;
        // this helper must convert them to char offsets so they're safe to feed
        // into CharCol.
        assert_eq!(find_char_positions("é bar", "bar"), vec![2]);
        assert_eq!(find_char_positions("café bar baz", "ba"), vec![5, 9]);
        assert_eq!(find_char_positions("ascii only", "only"), vec![6]);
        assert_eq!(find_char_positions("nope", "missing"), Vec::<usize>::new());
        assert_eq!(find_char_positions("anything", ""), Vec::<usize>::new());
    }

    #[test]
    fn find_char_positions_handles_multi_byte_grapheme_prefix() {
        // Family emoji is 1 grapheme but 25 bytes / 7 chars. The byte offset
        // of "x" is 25; the char offset is 7.
        let s = "👨\u{200d}👩\u{200d}👧\u{200d}👦x";
        assert_eq!(find_char_positions(s, "x"), vec![7]);
    }

    // ── splice/rope equivalence ─────────────────────────────────────────────

    #[test]
    fn splice_char_offsets_match_rope_semantics() {
        // The same char offsets applied to a rope must produce the same
        // result as apply_splice on the string.
        let cases = [
            ("héllo wörld\n", None, "wörld", "w"),
            ("aaa\nbbb\nccc", None, "bbb\nccc", "X"),
            ("日本\nabc\n", Some(2), "abc", "アイ"),
        ];
        for (content, line, old, new) in cases {
            let (splice, _) = plan_edit(content, line, old, new).unwrap();
            let via_string = apply_splice(content, &splice);
            let mut rope = ropey::Rope::from_str(content);
            rope.remove(splice.start_char..splice.end_char);
            rope.insert(splice.start_char, &splice.text);
            assert_eq!(via_string, rope.to_string(), "case {content:?}");
        }
    }
}
