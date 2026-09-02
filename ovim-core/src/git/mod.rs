use anyhow::Result;
use chrono::{TimeZone, Utc};
use git2::{DiffOptions, Oid, Repository};
use std::collections::HashMap;
use std::path::Path;

/// Represents the status of a line in the git diff
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// Line was added
    Added,
    /// Line was modified
    Modified,
    /// Line was deleted (shown on the line before)
    Removed,
}

/// Git status information for a file
#[derive(Debug, Clone)]
pub struct GitStatus {
    /// Map of line number (0-indexed) to status
    line_status: HashMap<usize, LineStatus>,
}

impl GitStatus {
    /// Creates a new empty git status
    pub fn new() -> Self {
        Self {
            line_status: HashMap::new(),
        }
    }

    /// Gets the status for a given line
    pub fn get_line_status(&self, line: usize) -> Option<LineStatus> {
        self.line_status.get(&line).copied()
    }

    /// First line (0-indexed) of every run of consecutive changed lines,
    /// sorted ascending. Used for `]c` / `[c` hunk navigation.
    pub fn hunk_starts(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = self.line_status.keys().copied().collect();
        lines.sort_unstable();
        let mut starts = Vec::new();
        let mut previous: Option<usize> = None;
        for line in lines {
            if previous.is_none_or(|prev| line != prev + 1) {
                starts.push(line);
            }
            previous = Some(line);
        }
        starts
    }

    /// Returns (added, modified, removed) line counts.
    pub fn change_counts(&self) -> (usize, usize, usize) {
        let mut added = 0;
        let mut modified = 0;
        let mut removed = 0;
        for status in self.line_status.values() {
            match status {
                LineStatus::Added => added += 1,
                LineStatus::Modified => modified += 1,
                LineStatus::Removed => removed += 1,
            }
        }
        (added, modified, removed)
    }

    /// Computes git status for a file
    pub fn from_file<P: AsRef<Path>>(file_path: P) -> Result<Self> {
        let file_path = file_path.as_ref();

        // Find the git repository
        let repo = match Repository::discover(file_path) {
            Ok(repo) => repo,
            Err(_) => return Ok(Self::new()), // Not in a git repo
        };

        // Get the workdir
        let workdir = match repo.workdir() {
            Some(dir) => dir,
            None => return Ok(Self::new()), // Bare repo
        };

        // Get relative path from repo root
        let relative_path = match file_path.strip_prefix(workdir) {
            Ok(p) => p,
            Err(_) => return Ok(Self::new()),
        };

        // Get HEAD tree
        let head = match repo.head() {
            Ok(head) => head,
            Err(_) => return Ok(Self::new()), // No HEAD (empty repo)
        };

        let head_commit = match head.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => return Ok(Self::new()),
        };

        let head_tree = match head_commit.tree() {
            Ok(tree) => tree,
            Err(_) => return Ok(Self::new()),
        };

        // Create diff between HEAD and working directory
        let mut diff_opts = DiffOptions::new();
        diff_opts.pathspec(relative_path);
        diff_opts.context_lines(0); // We only need the changed lines

        let diff =
            match repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut diff_opts)) {
                Ok(diff) => diff,
                Err(_) => return Ok(Self::new()),
            };

        // Parse the diff
        let mut line_status = HashMap::new();

        diff.foreach(
            &mut |_, _| true,
            None,
            None,
            Some(&mut |_delta, _hunk, line| {
                // Get the new line number (in the working copy)
                let line_num = line.new_lineno();

                match line.origin() {
                    '+' => {
                        // Added line
                        if let Some(num) = line_num {
                            line_status.insert(num as usize - 1, LineStatus::Added);
                        }
                    }
                    '-' => {
                        // Deleted line - mark the line before it
                        if let Some(num) = line.old_lineno() {
                            // Show deletion marker on the previous line in the new file
                            let old_line = num as usize - 1;
                            line_status.insert(old_line, LineStatus::Removed);
                        }
                    }
                    ' ' => {
                        // Context line - check if surrounded by changes
                        // This is a heuristic for "modified" lines
                    }
                    _ => {}
                }
                true
            }),
        )
        .ok();

        // Detect modified lines (lines that have both additions and deletions nearby)
        // This is a simple heuristic - in a real implementation you'd want more sophisticated detection
        let keys: Vec<usize> = line_status.keys().copied().collect();
        for &line in &keys {
            if let Some(status) = line_status.get(&line) {
                if *status == LineStatus::Added {
                    // Check if there's a removal nearby
                    for offset in 1..=3 {
                        if line >= offset
                            && line_status.get(&(line - offset)) == Some(&LineStatus::Removed)
                        {
                            // Likely a modification
                            line_status.insert(line, LineStatus::Modified);
                            break;
                        }
                        if line_status.get(&(line + offset)) == Some(&LineStatus::Removed) {
                            line_status.insert(line, LineStatus::Modified);
                            break;
                        }
                    }
                }
            }
        }

        Ok(Self { line_status })
    }
}

impl Default for GitStatus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Git Blame
// ---------------------------------------------------------------------------

/// Blame information for a single line
#[derive(Debug, Clone)]
pub struct LineBlameInfo {
    /// Full 40-char hex OID
    pub commit_oid: String,
    /// Short commit hash (5 chars)
    pub commit_hash: String,
    /// Author name
    pub author: String,
    /// Commit timestamp (Unix epoch seconds)
    pub timestamp: i64,
}

/// Metadata for a single commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub oid_hex: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
}

/// Git blame data for an entire file
#[derive(Debug, Clone)]
pub struct GitBlame {
    /// Blame info indexed by 0-based line number
    lines: Vec<Option<LineBlameInfo>>,
}

impl GitBlame {
    /// Computes blame for a file using git2
    pub fn from_file<P: AsRef<Path>>(file_path: P) -> Result<Self> {
        let file_path = file_path.as_ref();

        let repo = match Repository::discover(file_path) {
            Ok(repo) => repo,
            Err(_) => return Ok(Self { lines: Vec::new() }),
        };

        let workdir = match repo.workdir() {
            Some(dir) => dir,
            None => return Ok(Self { lines: Vec::new() }),
        };

        let relative_path = match file_path.strip_prefix(workdir) {
            Ok(p) => p,
            Err(_) => return Ok(Self { lines: Vec::new() }),
        };

        let blame = match repo.blame_file(relative_path, None) {
            Ok(b) => b,
            Err(_) => return Ok(Self { lines: Vec::new() }),
        };

        let mut lines = Vec::new();
        for hunk_idx in 0..blame.len() {
            if let Some(hunk) = blame.get_index(hunk_idx) {
                let commit_id = hunk.final_commit_id();
                let oid_hex = format!("{}", commit_id);
                let hash = oid_hex[..5.min(oid_hex.len())].to_string();
                let sig = hunk.final_signature();
                let author = sig.name().unwrap_or("Unknown").to_string();
                let timestamp = sig.when().seconds();
                let start = hunk.final_start_line(); // 1-indexed
                let count = hunk.lines_in_hunk();

                // Ensure vec is large enough
                let end = start + count;
                if end > lines.len() {
                    lines.resize(end, None);
                }

                for i in 0..count {
                    let line_idx = start - 1 + i; // convert to 0-indexed
                    lines[line_idx] = Some(LineBlameInfo {
                        commit_oid: oid_hex.clone(),
                        commit_hash: hash.clone(),
                        author: author.clone(),
                        timestamp,
                    });
                }
            }
        }

        Ok(Self { lines })
    }

    /// Gets blame info for a 0-indexed line
    pub fn get(&self, line: usize) -> Option<&LineBlameInfo> {
        self.lines.get(line).and_then(|o| o.as_ref())
    }

    /// Number of lines with blame data
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Returns true if there is no blame data
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Returns the maximum author name length across all lines
    pub fn max_author_len(&self) -> usize {
        self.lines
            .iter()
            .filter_map(|o| o.as_ref())
            .map(|info| info.author.len())
            .max()
            .unwrap_or(0)
    }
}

/// Returns the current git branch name for a file path.
/// Returns `None` for non-git files. Uses short OID for detached HEAD.
pub fn branch_name<P: AsRef<Path>>(file_path: P) -> Option<String> {
    let repo = Repository::discover(file_path.as_ref()).ok()?;
    let head = repo.head().ok()?;
    if head.is_branch() {
        head.shorthand().map(|s| s.to_string())
    } else {
        // Detached HEAD — show short OID
        head.target()
            .map(|oid| format!("{}", oid))
            .map(|s| s[..7.min(s.len())].to_string())
    }
}

/// Returns true if the OID is all zeros (uncommitted line).
pub fn is_zero_oid(oid_hex: &str) -> bool {
    oid_hex.chars().all(|c| c == '0')
}

/// Looks up commit metadata for a given OID hex string.
pub fn commit_info<P: AsRef<Path>>(file_path: P, oid_hex: &str) -> Result<CommitInfo> {
    if is_zero_oid(oid_hex) {
        return Ok(CommitInfo {
            oid_hex: oid_hex.to_string(),
            author: String::new(),
            date: String::new(),
            subject: "Not yet committed".to_string(),
            body: String::new(),
        });
    }

    let file_path = file_path.as_ref();
    let repo = Repository::discover(file_path)?;
    let oid = Oid::from_str(oid_hex)?;
    let commit = repo.find_commit(oid)?;

    let author = commit.author().name().unwrap_or("Unknown").to_string();
    let time = commit.author().when();
    let dt = Utc.timestamp_opt(time.seconds(), 0).single();
    let date = dt
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let message = commit.message().unwrap_or("").to_string();
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or("").to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    Ok(CommitInfo {
        oid_hex: oid_hex.to_string(),
        author,
        date,
        subject,
        body,
    })
}

/// Returns a unified diff for a commit (compared to its first parent).
pub fn commit_diff<P: AsRef<Path>>(file_path: P, oid_hex: &str) -> Result<String> {
    if is_zero_oid(oid_hex) {
        return Ok("Not yet committed".to_string());
    }

    let file_path = file_path.as_ref();
    let repo = Repository::discover(file_path)?;
    let oid = Oid::from_str(oid_hex)?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;

    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut patch = String::new();

    // Header
    let author_sig = commit.author();
    let author = author_sig.name().unwrap_or("Unknown");
    let time = author_sig.when();
    let dt = Utc.timestamp_opt(time.seconds(), 0).single();
    let date = dt
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let message = commit.message().unwrap_or("");
    let subject = message.lines().next().unwrap_or("");

    patch.push_str(&format!("commit {}\n", oid_hex));
    patch.push_str(&format!("Author: {}\n", author));
    patch.push_str(&format!("Date:   {}\n", date));
    patch.push_str(&format!("\n    {}\n\n", subject));

    // Diff content
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        match origin {
            '+' | '-' | ' ' => patch.push(origin),
            _ => {}
        }
        if let Ok(content) = std::str::from_utf8(line.content()) {
            patch.push_str(content);
        }
        true
    })?;

    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_status() {
        let status = GitStatus::new();
        assert_eq!(status.get_line_status(0), None);
        assert_eq!(status.get_line_status(10), None);
    }

    #[test]
    fn hunk_starts_groups_consecutive_lines() {
        let mut status = GitStatus::new();
        for line in [3, 4, 5, 9, 12, 13] {
            status.line_status.insert(line, LineStatus::Added);
        }
        assert_eq!(status.hunk_starts(), vec![3, 9, 12]);
        assert!(GitStatus::new().hunk_starts().is_empty());
    }

    #[test]
    fn test_empty_blame() {
        let blame = GitBlame { lines: Vec::new() };
        assert!(blame.is_empty());
        assert_eq!(blame.line_count(), 0);
        assert!(blame.get(0).is_none());
    }

    #[test]
    fn test_is_zero_oid() {
        assert!(is_zero_oid("0000000000000000000000000000000000000000"));
        assert!(is_zero_oid("00000"));
        assert!(!is_zero_oid("abc12"));
        assert!(!is_zero_oid("a000000000000000000000000000000000000000"));
    }
}
