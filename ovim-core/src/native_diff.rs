//! Native Git comparison support for Ovim's diff workspace.

use anyhow::{bail, Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository, Tree};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReview {
    pub root: PathBuf,
    pub spec: String,
    pub display_spec: String,
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

pub fn worktree_root(path: &Path) -> Result<PathBuf> {
    let repo = Repository::discover(path)
        .with_context(|| format!("{} is not inside a Git worktree", path.display()))?;
    repo.workdir()
        .map(Path::to_path_buf)
        .or_else(|| repo.path().parent().map(Path::to_path_buf))
        .context("Could not resolve the Git worktree")
}

pub fn review(path: &Path, spec: Option<&str>) -> Result<DiffReview> {
    let repo = Repository::discover(path)
        .with_context(|| format!("{} is not inside a Git worktree", path.display()))?;
    let root = worktree_root(path)?;
    let spec = normalize_spec(spec);
    let diff = build_diff(&repo, &spec, None)?;
    let files = summarize(&diff)?;
    Ok(DiffReview {
        root,
        display_spec: spec.replace("WORKTREE", "working tree"),
        spec,
        files,
    })
}

pub fn file_patch(path: &Path, spec: Option<&str>, requested_path: &str) -> Result<String> {
    if requested_path.is_empty() || Path::new(requested_path).is_absolute() {
        bail!("Choose a changed file from the comparison");
    }
    let repo = Repository::discover(path)
        .with_context(|| format!("{} is not inside a Git worktree", path.display()))?;
    let spec = normalize_spec(spec);
    let diff = build_diff(&repo, &spec, Some(requested_path))?;
    let files = summarize(&diff)?;
    if !files.iter().any(|file| file.path == requested_path) {
        bail!("{requested_path} is not changed in {spec}");
    }

    let mut output = Vec::new();
    output.extend_from_slice(format!("comparison: {spec}\nfile: {requested_path}\n\n").as_bytes());
    for index in 0..diff.deltas().len() {
        let Some(mut patch) = Patch::from_diff(&diff, index)? else {
            continue;
        };
        output.extend_from_slice(patch.to_buf()?.as_ref());
        if output.len() > MAX_PATCH_BYTES {
            bail!("The diff for {requested_path} exceeds the 4 MiB display limit");
        }
    }
    if output.is_empty() {
        bail!("No textual patch is available for {requested_path}");
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

// ---------------------------------------------------------------------------
// Branch review: base resolution and line-annotated patches
// ---------------------------------------------------------------------------

/// Branch names probed, in order, when the remote does not advertise a HEAD.
const DEFAULT_BRANCH_CANDIDATES: &[&str] = &["main", "master", "develop", "trunk"];

/// How the comparison base of a branch review was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BaseKind {
    /// The repository's default branch (merge-base with HEAD).
    DefaultBranch,
    /// HEAD is the default branch itself, so only uncommitted work is shown.
    OnDefaultBranch,
    /// No default branch could be found; falling back to uncommitted work.
    NoDefaultBranch,
    /// The user supplied the comparison explicitly.
    Explicit,
}

/// The base a branch review compares the working tree against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBase {
    /// Human-readable name, e.g. `origin/main`, `main`, or `HEAD`.
    pub name: String,
    /// Comparison spec accepted by [`review`] and [`review_patch`].
    pub spec: String,
    pub kind: BaseKind,
    /// `(remote, branch)` when the base is a remote-tracking ref.
    pub remote: Option<(String, String)>,
    /// Time since the last `git fetch` (from `FETCH_HEAD`), when known.
    pub fetched_ago: Option<Duration>,
    /// Whether the repository has ever fetched (`FETCH_HEAD` exists).
    pub ever_fetched: bool,
}

impl ReviewBase {
    fn head() -> Self {
        Self {
            name: "HEAD".to_string(),
            spec: "HEAD...WORKTREE".to_string(),
            kind: BaseKind::NoDefaultBranch,
            remote: None,
            fetched_ago: None,
            ever_fetched: false,
        }
    }

    /// A user-supplied comparison. A bare ref such as `develop` or `HEAD~3`
    /// compares its merge-base with HEAD against the working tree; specs that
    /// already contain `..` are used verbatim.
    pub fn explicit(spec: &str) -> Self {
        let spec = spec.trim();
        let full = if spec.contains("..") {
            spec.to_string()
        } else {
            format!("{spec}...WORKTREE")
        };
        Self {
            name: spec.to_string(),
            spec: full,
            kind: BaseKind::Explicit,
            remote: None,
            fetched_ago: None,
            ever_fetched: false,
        }
    }

    /// The ref the review compares against, if it is a plain `<base>...WORKTREE`.
    pub fn base_ref(&self) -> Option<&str> {
        self.spec
            .strip_suffix("...WORKTREE")
            .or_else(|| self.spec.strip_suffix("..WORKTREE"))
            .map(str::trim)
    }
}

/// Resolves the best base to review the current branch against.
///
/// The default branch is discovered from the remote's advertised HEAD, then
/// from common branch names. When both a local branch and its remote-tracking
/// ref exist, the one whose merge-base with HEAD is most recent wins, so a
/// stale local `main` (or a stale, un-fetched `origin/main`) never makes the
/// review show commits that are not yours.
pub fn resolve_base(path: &Path) -> Result<ReviewBase> {
    let repo = Repository::discover(path)
        .with_context(|| format!("{} is not inside a Git worktree", path.display()))?;
    let head_oid = resolve_commit_oid(&repo, "HEAD").context("The repository has no commits")?;
    let head_branch = repo
        .head()
        .ok()
        .filter(|head| head.is_branch())
        .and_then(|head| head.shorthand().map(str::to_string));

    let Some((remote, name)) = default_branch(&repo) else {
        return Ok(ReviewBase::head());
    };

    if head_branch.as_deref() == Some(name.as_str()) {
        let mut base = ReviewBase::head();
        base.kind = BaseKind::OnDefaultBranch;
        base.name = name;
        return Ok(base);
    }

    // Candidate refs for the default branch: remote-tracking first, then local.
    let mut candidates: Vec<BaseCandidate> = Vec::new();
    if let Some(remote) = &remote {
        let label = format!("{remote}/{name}");
        if let Ok(oid) = resolve_commit_oid(&repo, &format!("refs/remotes/{label}")) {
            candidates.push(BaseCandidate {
                label,
                oid,
                remote: Some((remote.clone(), name.clone())),
            });
        }
    }
    if let Ok(oid) = resolve_commit_oid(&repo, &format!("refs/heads/{name}")) {
        candidates.push(BaseCandidate {
            label: name.clone(),
            oid,
            remote: None,
        });
    }

    // Keep the candidate whose merge-base with HEAD is the most recent; on a
    // tie the remote-tracking ref (listed first) wins.
    let mut best: Option<(BaseCandidate, Oid)> = None;
    for candidate in candidates {
        let Ok(merge_base) = repo.merge_base(candidate.oid, head_oid) else {
            continue;
        };
        let replaces = match &best {
            None => true,
            Some((_, best_merge_base)) => {
                *best_merge_base != merge_base
                    && repo
                        .graph_descendant_of(merge_base, *best_merge_base)
                        .unwrap_or(false)
            }
        };
        if replaces {
            best = Some((candidate, merge_base));
        }
    }

    let Some((BaseCandidate { label, remote, .. }, _)) = best else {
        return Ok(ReviewBase::head());
    };
    let (fetched_ago, ever_fetched) = fetch_age(&repo);
    Ok(ReviewBase {
        spec: format!("{label}...WORKTREE"),
        name: label,
        kind: BaseKind::DefaultBranch,
        fetched_ago: if remote.is_some() { fetched_ago } else { None },
        ever_fetched,
        remote,
    })
}

/// A ref that could serve as the default-branch base.
struct BaseCandidate {
    label: String,
    oid: Oid,
    remote: Option<(String, String)>,
}

/// Finds the default branch as `(remote, branch)`. `remote` is `None` when
/// only a local branch with a conventional name exists.
fn default_branch(repo: &Repository) -> Option<(Option<String>, String)> {
    let mut remotes: Vec<String> = repo
        .remotes()
        .ok()
        .map(|names| names.iter().flatten().map(str::to_string).collect())
        .unwrap_or_default();
    remotes.sort_by_key(|remote| (remote != "origin", remote.clone()));

    // 1. The remote's advertised HEAD (set by clone or `git remote set-head`).
    for remote in &remotes {
        let Ok(reference) = repo.find_reference(&format!("refs/remotes/{remote}/HEAD")) else {
            continue;
        };
        let prefix = format!("refs/remotes/{remote}/");
        if let Some(name) = reference
            .symbolic_target()
            .and_then(|target| target.strip_prefix(prefix.as_str()))
        {
            return Some((Some(remote.clone()), name.to_string()));
        }
    }

    // 2. Conventional names, preferring ones that exist on a remote.
    for name in DEFAULT_BRANCH_CANDIDATES {
        for remote in &remotes {
            if repo
                .find_reference(&format!("refs/remotes/{remote}/{name}"))
                .is_ok()
            {
                return Some((Some(remote.clone()), name.to_string()));
            }
        }
    }
    for name in DEFAULT_BRANCH_CANDIDATES {
        if repo.find_reference(&format!("refs/heads/{name}")).is_ok() {
            return Some((None, name.to_string()));
        }
    }
    None
}

/// `(time since last fetch, FETCH_HEAD exists)`.
fn fetch_age(repo: &Repository) -> (Option<Duration>, bool) {
    let fetch_head = repo.path().join("FETCH_HEAD");
    let Ok(metadata) = std::fs::metadata(&fetch_head) else {
        return (None, false);
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok());
    (age, true)
}

/// Kind of a line in a rendered patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchLineKind {
    /// `diff --git`, `index`, `---`, `+++`, rename/mode lines
    FileHeader,
    /// `@@ -a,b +c,d @@`
    HunkHeader,
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`, binary notices, truncation notes
    Meta,
}

/// Source mapping for one line of a rendered patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchLine {
    pub kind: PatchLineKind,
    /// Index into [`ReviewPatch::files`].
    pub file: Option<usize>,
    /// 1-based line in the new (working tree) file this line corresponds to.
    /// For removed lines this is where the removal happened, so jumping there
    /// lands on the surrounding code.
    pub new_line: Option<usize>,
    /// 1-based line in the old file (context and removed lines only).
    pub old_line: Option<usize>,
}

/// A full branch review: summary plus a unified patch with per-line source
/// mapping, ready to be shown in a buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPatch {
    pub root: PathBuf,
    /// Current branch name, or a short OID when detached.
    pub head: String,
    pub base: ReviewBase,
    /// Short OID of the merge-base used, when the spec has one.
    pub merge_base: Option<String>,
    /// Commits on HEAD that are not on the base.
    pub ahead: usize,
    /// Commits on the base that are not on HEAD.
    pub behind: usize,
    pub files: Vec<DiffFile>,
    /// Unified patch text (no header); one entry in `lines` per text line.
    pub text: String,
    pub lines: Vec<PatchLine>,
    pub truncated: bool,
}

impl ReviewPatch {
    pub fn additions(&self) -> usize {
        self.files.iter().map(|file| file.additions).sum()
    }

    pub fn deletions(&self) -> usize {
        self.files.iter().map(|file| file.deletions).sum()
    }
}

/// Builds the full review patch for `base`.
pub fn review_patch(path: &Path, base: &ReviewBase) -> Result<ReviewPatch> {
    let repo = Repository::discover(path)
        .with_context(|| format!("{} is not inside a Git worktree", path.display()))?;
    let root = worktree_root(path)?;
    let head = repo
        .head()
        .ok()
        .and_then(|head| {
            if head.is_branch() {
                head.shorthand().map(str::to_string)
            } else {
                head.target().map(|oid| short_oid(&oid))
            }
        })
        .unwrap_or_else(|| "HEAD".to_string());

    let (merge_base, ahead, behind) = match (base.base_ref(), resolve_commit_oid(&repo, "HEAD")) {
        (Some(base_ref), Ok(head_oid)) if base.spec.ends_with("...WORKTREE") => {
            let base_oid = resolve_commit_oid(&repo, base_ref)?;
            let merge_base = repo.merge_base(base_oid, head_oid).ok();
            let (ahead, behind) = repo
                .graph_ahead_behind(head_oid, base_oid)
                .unwrap_or((0, 0));
            (merge_base.map(|oid| short_oid(&oid)), ahead, behind)
        }
        _ => (None, 0, 0),
    };

    let diff = build_diff(&repo, &base.spec, None)?;
    let files = summarize(&diff)?;
    let file_index: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.as_str(), index))
        .collect();

    let mut text = String::new();
    let mut lines: Vec<PatchLine> = Vec::new();
    let mut truncated = false;
    let mut current_file: Option<usize> = None;
    let mut next_new_line: usize = 1;

    diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
        if truncated {
            return true;
        }
        if text.len() > MAX_PATCH_BYTES {
            truncated = true;
            return true;
        }
        let content = String::from_utf8_lossy(line.content());
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .and_then(Path::to_str);
        current_file = path.and_then(|path| file_index.get(path).copied());

        match line.origin() {
            'F' => {
                for header in content.lines() {
                    push_line(
                        &mut text,
                        &mut lines,
                        header,
                        PatchLine {
                            kind: PatchLineKind::FileHeader,
                            file: current_file,
                            new_line: None,
                            old_line: None,
                        },
                    );
                }
            }
            'H' => {
                let hunk = hunk.as_ref();
                next_new_line = hunk.map(|hunk| hunk.new_start() as usize).unwrap_or(1);
                let old_line = hunk.map(|hunk| hunk.old_start() as usize);
                for header in content.lines() {
                    push_line(
                        &mut text,
                        &mut lines,
                        header,
                        PatchLine {
                            kind: PatchLineKind::HunkHeader,
                            file: current_file,
                            new_line: Some(next_new_line),
                            old_line,
                        },
                    );
                }
            }
            origin @ ('+' | '-' | ' ') => {
                let kind = match origin {
                    '+' => PatchLineKind::Added,
                    '-' => PatchLineKind::Removed,
                    _ => PatchLineKind::Context,
                };
                let new_line = line.new_lineno().map(|n| n as usize);
                if let Some(new_line) = new_line {
                    next_new_line = new_line + 1;
                }
                let body = content.strip_suffix('\n').unwrap_or(&content);
                let body = body.strip_suffix('\r').unwrap_or(body);
                let rendered = format!("{origin}{body}");
                push_line(
                    &mut text,
                    &mut lines,
                    &rendered,
                    PatchLine {
                        kind,
                        file: current_file,
                        new_line: Some(new_line.unwrap_or(next_new_line.max(1))),
                        old_line: line.old_lineno().map(|n| n as usize),
                    },
                );
            }
            _ => {
                for meta in content.lines().filter(|meta| !meta.trim().is_empty()) {
                    push_line(
                        &mut text,
                        &mut lines,
                        meta,
                        PatchLine {
                            kind: PatchLineKind::Meta,
                            file: current_file,
                            new_line: None,
                            old_line: None,
                        },
                    );
                }
            }
        }
        true
    })?;

    if truncated {
        push_line(
            &mut text,
            &mut lines,
            "\\ Diff truncated at 4 MiB; open the remaining files directly",
            PatchLine {
                kind: PatchLineKind::Meta,
                file: None,
                new_line: None,
                old_line: None,
            },
        );
    }

    Ok(ReviewPatch {
        root,
        head,
        base: base.clone(),
        merge_base,
        ahead,
        behind,
        files,
        text,
        lines,
        truncated,
    })
}

fn push_line(text: &mut String, lines: &mut Vec<PatchLine>, rendered: &str, info: PatchLine) {
    text.push_str(rendered);
    text.push('\n');
    lines.push(info);
}

fn short_oid(oid: &Oid) -> String {
    let hex = oid.to_string();
    hex[..7.min(hex.len())].to_string()
}

fn normalize_spec(spec: Option<&str>) -> String {
    let spec = spec.map(str::trim).filter(|spec| !spec.is_empty());
    spec.unwrap_or("HEAD...WORKTREE").to_string()
}

fn build_diff<'repo>(
    repo: &'repo Repository,
    spec: &str,
    pathspec: Option<&str>,
) -> Result<Diff<'repo>> {
    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true)
        // libgit2 defaults to c/ i/ w/ prefixes; use git's familiar a/ b/.
        .old_prefix("a")
        .new_prefix("b");
    if let Some(path) = pathspec {
        options.pathspec(path).disable_pathspec_match(true);
    }

    let mut diff = if let Some(base) = spec.strip_suffix("...WORKTREE") {
        let base_tree = merge_base_tree(repo, base.trim())?;
        repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut options))?
    } else if let Some(base) = spec.strip_suffix("..WORKTREE") {
        let base_tree = resolve_tree(repo, base.trim())?;
        repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut options))?
    } else if let Some((left, right)) = spec.split_once("...") {
        let left = resolve_commit_oid(repo, left.trim())?;
        let right = resolve_commit_oid(repo, right.trim())?;
        let base = repo.merge_base(left, right)?;
        let base_tree = repo.find_commit(base)?.tree()?;
        let right_tree = repo.find_commit(right)?.tree()?;
        repo.diff_tree_to_tree(Some(&base_tree), Some(&right_tree), Some(&mut options))?
    } else if let Some((left, right)) = spec.split_once("..") {
        let left_tree = resolve_tree(repo, left.trim())?;
        let right_tree = resolve_tree(repo, right.trim())?;
        repo.diff_tree_to_tree(Some(&left_tree), Some(&right_tree), Some(&mut options))?
    } else {
        bail!("Use a comparison such as HEAD...WORKTREE, main...WORKTREE, or main..feature")
    };
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    Ok(diff)
}

fn resolve_commit_oid(repo: &Repository, reference: &str) -> Result<Oid> {
    repo.revparse_single(reference)
        .with_context(|| format!("Unknown Git reference: {reference}"))?
        .peel_to_commit()
        .map(|commit| commit.id())
        .with_context(|| format!("{reference} does not resolve to a commit"))
}

fn resolve_tree<'repo>(repo: &'repo Repository, reference: &str) -> Result<Tree<'repo>> {
    repo.find_commit(resolve_commit_oid(repo, reference)?)?
        .tree()
        .with_context(|| format!("Could not read the tree for {reference}"))
}

fn merge_base_tree<'repo>(repo: &'repo Repository, base: &str) -> Result<Tree<'repo>> {
    let base = resolve_commit_oid(repo, base)?;
    let head = resolve_commit_oid(repo, "HEAD")?;
    let oid = repo.merge_base(base, head).unwrap_or(base);
    Ok(repo.find_commit(oid)?.tree()?)
}

fn summarize(diff: &Diff<'_>) -> Result<Vec<DiffFile>> {
    let mut files = Vec::with_capacity(diff.deltas().len());
    for (index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .and_then(Path::to_str)
            .context("A changed path is not valid UTF-8")?
            .to_string();
        let old_path = delta
            .old_file()
            .path()
            .and_then(Path::to_str)
            .filter(|old| *old != path)
            .map(str::to_string);
        let binary = delta.flags().contains(git2::DiffFlags::BINARY);
        let (additions, deletions) = Patch::from_diff(diff, index)?
            .map(|patch| {
                patch
                    .line_stats()
                    .map(|(_, additions, deletions)| (additions, deletions))
            })
            .transpose()?
            .unwrap_or_default();
        files.push(DiffFile {
            path,
            old_path,
            status: status_name(delta.status()).to_string(),
            additions,
            deletions,
            binary,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn status_name(status: Delta) -> &'static str {
    match status {
        Delta::Added | Delta::Untracked => "added",
        Delta::Deleted => "deleted",
        Delta::Renamed => "renamed",
        Delta::Copied => "copied",
        Delta::Typechange => "typechanged",
        Delta::Conflicted => "conflicted",
        _ => "modified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn commit(repo: &Repository, path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Ovim", "ovim@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
    }

    #[test]
    fn summarizes_and_renders_worktree_changes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let file = temp.path().join("file.txt");
        commit(&repo, &file, "one\ntwo\n");
        fs::write(&file, "one\nchanged\nthree\n").unwrap();
        fs::write(temp.path().join("new.txt"), "new\n").unwrap();

        let review = review(temp.path(), None).unwrap();
        assert_eq!(review.spec, "HEAD...WORKTREE");
        assert_eq!(review.files.len(), 2);
        assert!(review.files.iter().any(|file| file.path == "new.txt"));

        let patch = file_patch(temp.path(), None, "file.txt").unwrap();
        assert!(patch.contains("comparison: HEAD...WORKTREE"));
        assert!(patch.contains("-two"));
        assert!(patch.contains("+changed"), "{patch}");
    }

    /// Stages everything and commits on the current HEAD branch.
    fn commit_all(repo: &Repository, message: &str) -> Oid {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Ovim", "ovim@example.com").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
    }

    /// Repository whose first commit lives on `main`, regardless of the
    /// machine's `init.defaultBranch`.
    fn repo_on_main(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        commit_all(&repo, "c1");
        repo
    }

    fn checkout_new_branch(repo: &Repository, name: &str) {
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch(name, &head, false).unwrap();
        repo.set_head(&format!("refs/heads/{name}")).unwrap();
    }

    #[test]
    fn resolve_base_prefers_local_main_when_remote_is_stale() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repo_on_main(temp.path());
        let c1 = repo.head().unwrap().target().unwrap();
        fs::write(temp.path().join("m.txt"), "main moved\n").unwrap();
        let c2 = commit_all(&repo, "c2 on main");
        // origin/main was fetched before main moved to c2.
        repo.reference("refs/remotes/origin/main", c1, true, "stale fetch")
            .unwrap();
        checkout_new_branch(&repo, "feature");
        fs::write(temp.path().join("f.txt"), "feature\n").unwrap();
        commit_all(&repo, "feature work");

        let base = resolve_base(temp.path()).unwrap();
        assert_eq!(base.kind, BaseKind::DefaultBranch);
        assert_eq!(base.name, "main", "local main has the newer merge-base");
        assert_eq!(base.spec, "main...WORKTREE");
        assert!(base.remote.is_none());
        let _ = c2;
    }

    #[test]
    fn resolve_base_prefers_remote_main_when_local_is_stale() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repo_on_main(temp.path());
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        checkout_new_branch(&repo, "feature");
        fs::write(temp.path().join("m.txt"), "landed on main\n").unwrap();
        let c2 = commit_all(&repo, "c2 (also on origin/main)");
        fs::write(temp.path().join("f.txt"), "feature\n").unwrap();
        commit_all(&repo, "feature work");
        // Remote main already contains c2; the local main branch is behind.
        repo.reference("refs/remotes/origin/main", c2, true, "fresh fetch")
            .unwrap();

        let base = resolve_base(temp.path()).unwrap();
        assert_eq!(base.name, "origin/main");
        assert_eq!(base.spec, "origin/main...WORKTREE");
        assert_eq!(
            base.remote,
            Some(("origin".to_string(), "main".to_string()))
        );
        assert!(!base.ever_fetched, "no FETCH_HEAD was ever written");
    }

    #[test]
    fn resolve_base_on_default_branch_shows_uncommitted_work() {
        let temp = tempfile::tempdir().unwrap();
        let _repo = repo_on_main(temp.path());
        let base = resolve_base(temp.path()).unwrap();
        assert_eq!(base.kind, BaseKind::OnDefaultBranch);
        assert_eq!(base.name, "main");
        assert_eq!(base.spec, "HEAD...WORKTREE");
    }

    #[test]
    fn resolve_base_honours_remote_head_symref() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.set_head("refs/heads/trunk-ish").unwrap();
        fs::write(temp.path().join("a.txt"), "x\n").unwrap();
        let c1 = commit_all(&repo, "c1");
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        repo.reference("refs/remotes/origin/trunk-ish", c1, true, "fetch")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk-ish",
            true,
            "set-head",
        )
        .unwrap();
        checkout_new_branch(&repo, "feature");

        let base = resolve_base(temp.path()).unwrap();
        assert_eq!(base.name, "origin/trunk-ish");
    }

    #[test]
    fn explicit_base_specs() {
        assert_eq!(ReviewBase::explicit("develop").spec, "develop...WORKTREE");
        assert_eq!(ReviewBase::explicit("HEAD~3").base_ref(), Some("HEAD~3"));
        assert_eq!(ReviewBase::explicit("main..feature").spec, "main..feature");
        assert_eq!(ReviewBase::explicit("main..feature").base_ref(), None);
    }

    #[test]
    fn review_patch_maps_every_line_to_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repo_on_main(temp.path());
        checkout_new_branch(&repo, "feature");
        fs::write(temp.path().join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        commit_all(&repo, "edit a");
        fs::write(temp.path().join("b.txt"), "new file\n").unwrap();

        let base = resolve_base(temp.path()).unwrap();
        let patch = review_patch(temp.path(), &base).unwrap();
        assert_eq!(patch.head, "feature");
        assert_eq!(patch.ahead, 1);
        assert_eq!(patch.behind, 0);
        assert!(patch.merge_base.is_some());
        assert_eq!(
            patch
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt", "b.txt"]
        );
        assert_eq!(patch.text.lines().count(), patch.lines.len());

        let rows: Vec<(&str, PatchLine)> = patch
            .text
            .lines()
            .zip(patch.lines.iter().copied())
            .collect();
        let find = |needle: &str| {
            rows.iter()
                .find(|(text, _)| *text == needle)
                .map(|(_, info)| *info)
                .unwrap_or_else(|| panic!("missing line {needle:?} in\n{}", patch.text))
        };

        let hunk = find("@@ -1,3 +1,4 @@");
        assert_eq!(hunk.kind, PatchLineKind::HunkHeader);
        assert_eq!((hunk.file, hunk.new_line), (Some(0), Some(1)));

        let removed = find("-two");
        assert_eq!(removed.kind, PatchLineKind::Removed);
        assert_eq!(removed.new_line, Some(2), "removal lands where `2` now is");
        assert_eq!(removed.old_line, Some(2));

        let added = find("+2");
        assert_eq!(
            (added.kind, added.new_line),
            (PatchLineKind::Added, Some(2))
        );
        let four = find("+four");
        assert_eq!(four.new_line, Some(4));
        let context = find(" three");
        assert_eq!(
            (context.kind, context.new_line, context.old_line),
            (PatchLineKind::Context, Some(3), Some(3))
        );

        let header = find("diff --git a/b.txt b/b.txt");
        assert_eq!(
            (header.kind, header.file),
            (PatchLineKind::FileHeader, Some(1))
        );
        let new_file_line = find("+new file");
        assert_eq!(
            (new_file_line.file, new_file_line.new_line),
            (Some(1), Some(1))
        );
    }

    #[test]
    fn rejects_files_outside_the_comparison() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let file = temp.path().join("file.txt");
        commit(&repo, &file, "one\n");
        let error = file_patch(temp.path(), None, "other.txt").unwrap_err();
        assert!(error.to_string().contains("not changed"));
    }
}
