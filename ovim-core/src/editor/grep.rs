//! In-process grep search using grep-searcher + grep-regex (ripgrep internals).
//!
//! Replaces the old subprocess-based `rg` invocation with direct library calls
//! for better performance (no process spawn overhead) and no external binary dependency.

use super::picker::PickerResult;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use ignore::{WalkBuilder, WalkState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Maximum number of results to prevent OOM on broad queries.
const MAX_RESULTS: usize = 5000;

/// Flush partial batches to the UI once they reach this size, so a single
/// file with thousands of matches still streams incrementally.
const GREP_BATCH_SIZE: usize = 128;

/// Grep match lines are stored for display only; cap them so a minified
/// one-line bundle doesn't allocate megabytes per match.
const MAX_CONTENT_LEN: usize = 500;

/// Number of threads for parallel directory walks.
///
/// The `ignore` crate silently defaults to 2 when unset; mirror ripgrep and
/// use available parallelism (capped, since walk throughput plateaus).
pub fn walk_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .min(12)
}

/// Trims trailing whitespace and caps the stored line for display.
fn display_content(line_content: &str) -> String {
    let trimmed = line_content.trim_end();
    if trimmed.len() <= MAX_CONTENT_LEN {
        return trimmed.to_string();
    }
    let mut end = MAX_CONTENT_LEN;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}

// -----------------------------------------------------------------
// Shared search primitives
// -----------------------------------------------------------------

/// Build a smart-case regex matcher from a query string.
///
/// Uses case-insensitive matching when the query is all lowercase.
/// Falls back to literal (escaped) matching if the query is invalid regex.
fn build_grep_matcher(query: &str) -> Result<RegexMatcher, ()> {
    let case_insensitive = query.chars().all(|c| !c.is_uppercase());

    RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .build(query)
        .or_else(|_| {
            RegexMatcherBuilder::new()
                .case_insensitive(case_insensitive)
                .build(&regex::escape(query))
        })
        .map_err(|_| ())
}

/// Build a directory walker builder with standard gitignore and hidden file settings.
pub(crate) fn build_walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);
    builder
}

/// Build a searcher that skips binary files.
fn build_searcher() -> Searcher {
    let mut searcher = Searcher::new();
    searcher.set_binary_detection(grep_searcher::BinaryDetection::quit(0));
    searcher
}

/// Extract the char-based column of the first match within a line.
fn match_column(matcher: &RegexMatcher, line_content: &str) -> usize {
    matcher
        .find(line_content.as_bytes())
        .ok()
        .flatten()
        .map(|m| line_content[..m.start()].chars().count())
        .unwrap_or(0)
}

// -----------------------------------------------------------------
// Public API
// -----------------------------------------------------------------

/// A single grep match for synchronous tool use.
#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub rel_path: String,
    pub line: usize,     // 1-indexed
    pub col: usize,      // 0-indexed
    pub content: String, // matched line content, trimmed
}

/// Synchronous grep search for tool handlers.
///
/// Uses the same matching logic as `spawn_grep_search` but returns results
/// directly (no channel, no cancel flag). Capped by `max_results`.
pub fn grep_search_sync(query: &str, base_dir: &Path, max_results: usize) -> Vec<GrepMatch> {
    if query.is_empty() {
        return Vec::new();
    }

    let matcher = match build_grep_matcher(query) {
        Ok(m) => m,
        Err(()) => return Vec::new(),
    };

    let mut results = Vec::new();

    for entry in build_walker(base_dir).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
            continue;
        }

        let file_path = entry.path().to_path_buf();

        let _ = build_searcher().search_path(
            &matcher,
            &file_path,
            UTF8(|line_num, line_content| {
                if results.len() >= max_results {
                    return Ok(false);
                }

                results.push(GrepMatch {
                    rel_path: file_path
                        .strip_prefix(base_dir)
                        .unwrap_or(&file_path)
                        .to_string_lossy()
                        .to_string(),
                    line: line_num as usize,
                    col: match_column(&matcher, line_content),
                    content: line_content.trim_end().to_string(),
                });

                Ok(true)
            }),
        );

        if results.len() >= max_results {
            break;
        }
    }

    results
}

/// Spawns an in-process grep search in a blocking thread.
///
/// Returns a channel receiver that streams batches of `PickerResult`s as
/// they're found. Files are walked and searched in parallel (like ripgrep),
/// so result order within a pass is nondeterministic — but all matches from
/// the preferred directory are sent before any match from the rest of the
/// tree, preserving local-first ranking.
///
/// The search respects `.gitignore`, skips `.git` directories, and uses
/// smart-case matching (case-insensitive when query is all lowercase).
///
/// Pass a `cancel` flag to abort the search early (e.g., when query changes).
pub fn spawn_grep_search(
    query: String,
    base_dir: PathBuf,
    preferred_dir: PathBuf,
    cancel: Arc<AtomicBool>,
) -> mpsc::Receiver<Vec<PickerResult>> {
    let (tx, rx) = mpsc::channel(256);

    tokio::task::spawn_blocking(move || {
        if query.is_empty() {
            return;
        }

        let matcher = match build_grep_matcher(&query) {
            Ok(m) => m,
            Err(()) => return,
        };

        let mut roots: Vec<(PathBuf, bool)> = Vec::new();
        if preferred_dir != base_dir && preferred_dir.starts_with(&base_dir) {
            roots.push((preferred_dir.clone(), true));
        }
        roots.push((base_dir.clone(), false));

        // Shared across worker threads; results pushed stays <= MAX_RESULTS
        // even though the counter itself can briefly overshoot.
        let result_count = Arc::new(AtomicUsize::new(0));

        for (root, is_preferred_root) in roots {
            if cancel.load(Ordering::Relaxed) || result_count.load(Ordering::Relaxed) >= MAX_RESULTS
            {
                break;
            }

            let preferred_for_filter = preferred_dir.clone();
            let base_for_filter = base_dir.clone();
            build_walker(&root)
                .threads(walk_threads())
                .filter_entry(move |entry| {
                    // For the base-dir pass, skip the preferred subtree to avoid duplicates.
                    if !is_preferred_root
                        && preferred_for_filter != base_for_filter
                        && entry.path().starts_with(&preferred_for_filter)
                    {
                        return false;
                    }
                    true
                })
                .build_parallel()
                .run(|| {
                    let matcher = matcher.clone();
                    let mut searcher = build_searcher();
                    let tx = tx.clone();
                    let cancel = cancel.clone();
                    let result_count = result_count.clone();
                    let base_dir = base_dir.clone();

                    Box::new(move |entry| {
                        if cancel.load(Ordering::Relaxed)
                            || result_count.load(Ordering::Relaxed) >= MAX_RESULTS
                        {
                            return WalkState::Quit;
                        }

                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => return WalkState::Continue,
                        };
                        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                            return WalkState::Continue;
                        }

                        let file_path = entry.path();
                        // Computed once per file with matches, not per match line.
                        let mut location: Option<(String, String)> = None;
                        let mut batch: Vec<PickerResult> = Vec::new();
                        let mut send_failed = false;

                        let _ = searcher.search_path(
                            &matcher,
                            file_path,
                            UTF8(|line_num, line_content| {
                                if cancel.load(Ordering::Relaxed) {
                                    return Ok(false);
                                }
                                if result_count.fetch_add(1, Ordering::Relaxed) >= MAX_RESULTS {
                                    return Ok(false);
                                }

                                let (location, rel_path) = location.get_or_insert_with(|| {
                                    (
                                        file_path.to_string_lossy().to_string(),
                                        file_path
                                            .strip_prefix(&base_dir)
                                            .unwrap_or(file_path)
                                            .to_string_lossy()
                                            .to_string(),
                                    )
                                });
                                let line = line_num as usize;
                                let col = match_column(&matcher, line_content);

                                batch.push(PickerResult {
                                    display: format!("{}:{}:{}", rel_path, line, col + 1),
                                    location: location.clone(),
                                    line: line.saturating_sub(1), // 0-indexed
                                    col,
                                    match_positions: Vec::new(),
                                    content: Some(display_content(line_content)),
                                });

                                if batch.len() >= GREP_BATCH_SIZE {
                                    if tx.blocking_send(std::mem::take(&mut batch)).is_err() {
                                        send_failed = true;
                                        return Ok(false);
                                    }
                                }

                                Ok(true)
                            }),
                        );

                        if !batch.is_empty() && tx.blocking_send(batch).is_err() {
                            send_failed = true;
                        }

                        if send_failed {
                            WalkState::Quit
                        } else {
                            WalkState::Continue
                        }
                    })
                });
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_grep_prefers_preferred_dir_results_first() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let preferred = root.join("preferred");
        let other = root.join("other");
        fs::create_dir_all(&preferred).unwrap();
        fs::create_dir_all(&other).unwrap();

        fs::write(preferred.join("a.txt"), "needle\n").unwrap();
        fs::write(preferred.join("b.txt"), "needle\n").unwrap();
        fs::write(other.join("c.txt"), "needle\n").unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let mut rx = spawn_grep_search(
            "needle".to_string(),
            root.to_path_buf(),
            preferred.clone(),
            cancel,
        );

        let mut displays = Vec::new();
        while let Some(batch) = rx.recv().await {
            for r in batch {
                displays.push(r.display);
            }
        }

        assert!(!displays.is_empty());

        // All preferred-dir matches should appear before any non-preferred-dir match.
        let mut seen_non_preferred = false;
        for d in displays {
            let rel_path = d.split(':').next().unwrap_or("");
            let is_preferred = rel_path.starts_with("preferred/");
            if !is_preferred {
                seen_non_preferred = true;
            }
            if seen_non_preferred {
                assert!(
                    !is_preferred,
                    "saw preferred-dir match after non-preferred: {rel_path}"
                );
            }
        }
    }

    #[test]
    fn grep_search_sync_finds_matches() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "hello world\nfoo bar\n").unwrap();
        fs::write(root.join("b.txt"), "hello again\n").unwrap();

        let results = grep_search_sync("hello", root, 100);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.content.contains("hello")));
        // Lines are 1-indexed
        assert!(results.iter().all(|r| r.line >= 1));
    }

    #[test]
    fn grep_search_sync_respects_max_results() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content: String = (0..100).map(|i| format!("needle {i}\n")).collect();
        fs::write(root.join("big.txt"), &content).unwrap();

        let results = grep_search_sync("needle", root, 10);
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn grep_search_sync_empty_query_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let results = grep_search_sync("", dir.path(), 100);
        assert!(results.is_empty());
    }
}
