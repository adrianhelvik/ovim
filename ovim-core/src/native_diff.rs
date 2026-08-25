//! Native Git comparison support for Ovim's diff workspace.

use anyhow::{bail, Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository, Tree};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
        .include_typechange(true);
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
