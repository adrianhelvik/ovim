use super::{
    ArtifactExportPolicy, ArtifactId, ArtifactRecord, ArtifactRetention, ArtifactSource,
    ArtifactState, ArtifactStore, BlobId, CaptureDecision, CaptureKind, CapturePolicy,
    CaptureSubject, ContentRepresentation, FileKind, FileMutationEvent, FileMutationState,
    RepoPath, WorkspaceSurface,
};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct WorkspaceSnapshot {
    root: PathBuf,
    files: BTreeMap<String, CapturedFile>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone)]
struct CapturedFile {
    kind: FileKind,
    artifact: ArtifactRecord,
}

#[derive(Debug, Default)]
pub struct WorkspaceDelta {
    pub mutations: Vec<FileMutationEvent>,
    pub issues: Vec<String>,
}

/// Captures the disk surface without following symlinks. `.git` is excluded:
/// replay records the working tree, not Git's internal object database.
pub fn capture_workspace(
    root: &Path,
    store: &ArtifactStore,
    policy: &dyn CapturePolicy,
) -> Result<WorkspaceSnapshot, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("resolve workspace {}: {error}", root.display()))?;
    let mut snapshot = WorkspaceSnapshot {
        root: root.clone(),
        files: BTreeMap::new(),
        issues: Vec::new(),
    };
    let mut remaining = MAX_CAPTURE_BYTES;
    if let Some(paths) = git_workspace_paths(&root, &mut snapshot.issues) {
        for relative in paths {
            capture_path(
                &root,
                &relative,
                store,
                policy,
                &mut remaining,
                &mut snapshot,
            )?;
        }
    } else {
        capture_directory(&root, &root, store, policy, &mut remaining, &mut snapshot)?;
    }
    Ok(snapshot)
}

fn git_workspace_paths(root: &Path, issues: &mut Vec<String>) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut paths = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        match std::str::from_utf8(raw) {
            Ok(path) => paths.push(PathBuf::from(path)),
            Err(_) => {
                issues.push("Git reported a non-UTF-8 path; replay capture skipped it".into())
            }
        }
    }
    Some(paths)
}

fn capture_directory(
    root: &Path,
    directory: &Path,
    store: &ArtifactStore,
    policy: &dyn CapturePolicy,
    remaining: &mut u64,
    snapshot: &mut WorkspaceSnapshot,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("read directory {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                snapshot.issues.push(format!(
                    "could not enumerate an entry in {}: {error}",
                    directory.display()
                ));
                continue;
            }
        };
        if directory == root && entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().ok().is_some_and(|kind| kind.is_dir())
            && matches!(
                entry.file_name().to_str(),
                Some("target" | "node_modules" | ".cache" | "dist" | "build")
            )
        {
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                snapshot
                    .issues
                    .push(format!("could not inspect {}: {error}", path.display()));
                continue;
            }
        };
        if metadata.is_dir() {
            if let Err(error) = capture_directory(root, &path, store, policy, remaining, snapshot) {
                snapshot.issues.push(error);
            }
            continue;
        }
        let relative_path = match path.strip_prefix(root) {
            Ok(path) => path,
            Err(_) => continue,
        };
        capture_path(root, relative_path, store, policy, remaining, snapshot)?;
    }
    Ok(())
}

fn capture_path(
    root: &Path,
    relative_path: &Path,
    store: &ArtifactStore,
    policy: &dyn CapturePolicy,
    remaining: &mut u64,
    snapshot: &mut WorkspaceSnapshot,
) -> Result<(), String> {
    let path = root.join(relative_path);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        // Git lists tracked paths even when deleted. Their absence is the
        // intended after-state, not a capture failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            snapshot
                .issues
                .push(format!("could not inspect {}: {error}", path.display()));
            return Ok(());
        }
    };
    if metadata.is_dir() {
        return Ok(());
    }
    let relative = match relative_path.to_str() {
        Some(path) => path.replace(std::path::MAIN_SEPARATOR, "/"),
        None => {
            snapshot
                .issues
                .push(format!("path is not replayable UTF-8: {}", path.display()));
            return Ok(());
        }
    };
    let kind = if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_file() {
        FileKind::Regular
    } else {
        snapshot
            .issues
            .push(format!("unsupported special file: {relative}"));
        return Ok(());
    };
    let repo_path = match RepoPath::parse(relative.clone()) {
        Ok(path) => path,
        Err(error) => {
            snapshot.issues.push(error.to_string());
            return Ok(());
        }
    };
    let subject = CaptureSubject {
        path: Some(&repo_path),
        ephemeral_buffer_id: None,
        kind: CaptureKind::Disk,
        locator: &relative,
        declared_bytes: Some(metadata.len()),
    };
    if let CaptureDecision::Exclude { reason } = policy.decide(&subject) {
        snapshot.issues.push(format!("{relative}: {reason}"));
        snapshot.files.insert(
            relative.clone(),
            CapturedFile {
                kind,
                artifact: unavailable_record(&relative, reason),
            },
        );
        return Ok(());
    }

    let bytes = if metadata.file_type().is_symlink() {
        match fs::read_link(&path) {
            Ok(target) => target.as_os_str().as_encoded_bytes().to_vec(),
            Err(error) => {
                snapshot.issues.push(format!(
                    "could not read symlink {}: {error}",
                    path.display()
                ));
                return Ok(());
            }
        }
    } else {
        if metadata.len() > MAX_FILE_BYTES || metadata.len() > *remaining {
            let reason = if metadata.len() > MAX_FILE_BYTES {
                format!("file exceeds capture limit of {MAX_FILE_BYTES} bytes")
            } else {
                "workspace capture byte budget was exhausted".into()
            };
            snapshot.issues.push(format!("{relative}: {reason}"));
            snapshot.files.insert(
                relative.clone(),
                CapturedFile {
                    kind: FileKind::Regular,
                    artifact: unavailable_record(&relative, reason),
                },
            );
            return Ok(());
        }
        match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                snapshot
                    .issues
                    .push(format!("could not read {}: {error}", path.display()));
                snapshot.files.insert(
                    relative.clone(),
                    CapturedFile {
                        kind: FileKind::Regular,
                        artifact: unavailable_record(&relative, error.to_string()),
                    },
                );
                return Ok(());
            }
        }
    };
    *remaining = remaining.saturating_sub(bytes.len() as u64);
    let stored = store
        .put_bytes(&bytes)
        .map_err(|error| format!("store artifact for {relative}: {error}"))?;
    snapshot.files.insert(
        relative.clone(),
        CapturedFile {
            kind,
            artifact: ArtifactRecord {
                artifact_id: ArtifactId::new(),
                state: ArtifactState::Available {
                    blob_id: stored.blob_id,
                    byte_len: stored.byte_len,
                },
                source: ArtifactSource::Workspace {
                    path: relative,
                    surface: WorkspaceSurface::Disk,
                },
                representation: ContentRepresentation::RawBytes,
                media_type: None,
                retention: ArtifactRetention::Run,
                export_policy: ArtifactExportPolicy::Include,
            },
        },
    );
    Ok(())
}

fn unavailable_record(path: &str, reason: String) -> ArtifactRecord {
    ArtifactRecord {
        artifact_id: ArtifactId::new(),
        state: ArtifactState::Excluded { reason },
        source: ArtifactSource::Workspace {
            path: path.into(),
            surface: WorkspaceSurface::Disk,
        },
        representation: ContentRepresentation::RawBytes,
        media_type: None,
        retention: ArtifactRetention::Run,
        export_policy: ArtifactExportPolicy::Omit,
    }
}

fn blob(record: &ArtifactRecord) -> Option<BlobId> {
    match record.state {
        ArtifactState::Available { blob_id, .. } => Some(blob_id),
        _ => None,
    }
}

impl WorkspaceSnapshot {
    pub fn diff(self, after: WorkspaceSnapshot) -> WorkspaceDelta {
        let mut delta = WorkspaceDelta::default();
        if self.root != after.root {
            delta
                .issues
                .push("before/after workspace roots differ".into());
            return delta;
        }
        delta.issues.extend(self.issues);
        delta.issues.extend(after.issues);

        let mut deleted: Vec<_> = self
            .files
            .iter()
            .filter(|(path, _)| !after.files.contains_key(*path))
            .map(|(path, file)| (path.clone(), file.clone()))
            .collect();
        let mut created: Vec<_> = after
            .files
            .iter()
            .filter(|(path, _)| !self.files.contains_key(*path))
            .map(|(path, file)| (path.clone(), file.clone()))
            .collect();

        // Infer only unique, byte-identical moves. Ambiguity remains a delete
        // plus create, which is semantically safe and deterministic.
        let mut deleted_by_blob: HashMap<BlobId, Vec<usize>> = HashMap::new();
        let mut created_by_blob: HashMap<BlobId, Vec<usize>> = HashMap::new();
        for (index, (_, file)) in deleted.iter().enumerate() {
            if let Some(blob) = blob(&file.artifact) {
                deleted_by_blob.entry(blob).or_default().push(index);
            }
        }
        for (index, (_, file)) in created.iter().enumerate() {
            if let Some(blob) = blob(&file.artifact) {
                created_by_blob.entry(blob).or_default().push(index);
            }
        }
        let mut renamed_deleted = vec![false; deleted.len()];
        let mut renamed_created = vec![false; created.len()];
        for (id, old_indices) in deleted_by_blob {
            let Some(new_indices) = created_by_blob.get(&id) else {
                continue;
            };
            if old_indices.len() == 1 && new_indices.len() == 1 {
                let old = old_indices[0];
                let new = new_indices[0];
                if deleted[old].1.kind == created[new].1.kind {
                    renamed_deleted[old] = true;
                    renamed_created[new] = true;
                    delta.mutations.push(mutation(
                        created[new].0.clone(),
                        Some(deleted[old].0.clone()),
                        deleted[old].1.kind.clone(),
                        Some(deleted[old].1.artifact.clone()),
                        Some(created[new].1.artifact.clone()),
                    ));
                }
            }
        }
        for (index, (path, file)) in deleted.drain(..).enumerate() {
            if !renamed_deleted[index] {
                delta
                    .mutations
                    .push(mutation(path, None, file.kind, Some(file.artifact), None));
            }
        }
        for (index, (path, file)) in created.drain(..).enumerate() {
            if !renamed_created[index] {
                delta
                    .mutations
                    .push(mutation(path, None, file.kind, None, Some(file.artifact)));
            }
        }
        for (path, before) in self.files {
            let Some(after) = after.files.get(&path) else {
                continue;
            };
            if before.kind != after.kind || blob(&before.artifact) != blob(&after.artifact) {
                delta.mutations.push(mutation(
                    path,
                    None,
                    after.kind.clone(),
                    Some(before.artifact),
                    Some(after.artifact.clone()),
                ));
            }
        }
        delta
            .mutations
            .sort_by(|left, right| left.path.cmp(&right.path));
        delta.issues.sort();
        delta.issues.dedup();
        delta
    }
}

fn mutation(
    path: String,
    previous_path: Option<String>,
    file_kind: FileKind,
    before: Option<ArtifactRecord>,
    after: Option<ArtifactRecord>,
) -> FileMutationEvent {
    let before_artifact = before.as_ref().map(ArtifactRecord::as_ref);
    let after_artifact = after.as_ref().map(ArtifactRecord::as_ref);
    let artifacts = before.into_iter().chain(after).collect();
    FileMutationEvent {
        path,
        previous_path,
        surface: WorkspaceSurface::Disk,
        file_kind,
        before_artifact,
        after_artifact,
        artifacts,
        state: FileMutationState::Completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Allow;

    impl CapturePolicy for Allow {
        fn decide(&self, _subject: &CaptureSubject<'_>) -> CaptureDecision {
            CaptureDecision::Include
        }
    }

    #[test]
    fn captures_create_modify_delete_and_unique_rename() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(artifacts.path()).unwrap();
        fs::write(workspace.path().join("modified"), b"before").unwrap();
        fs::write(workspace.path().join("deleted"), b"gone").unwrap();
        fs::write(workspace.path().join("old"), b"move-me").unwrap();
        let before = capture_workspace(workspace.path(), &store, &Allow).unwrap();

        fs::write(workspace.path().join("modified"), b"after").unwrap();
        fs::remove_file(workspace.path().join("deleted")).unwrap();
        fs::rename(workspace.path().join("old"), workspace.path().join("new")).unwrap();
        fs::write(workspace.path().join("created"), b"new").unwrap();
        let delta = before.diff(capture_workspace(workspace.path(), &store, &Allow).unwrap());

        assert_eq!(delta.mutations.len(), 4);
        assert!(delta
            .mutations
            .iter()
            .any(|m| m.path == "created" && m.before_artifact.is_none()));
        assert!(delta
            .mutations
            .iter()
            .any(|m| m.path == "deleted" && m.after_artifact.is_none()));
        assert!(delta.mutations.iter().any(|m| m.path == "modified"
            && m.before_artifact.is_some()
            && m.after_artifact.is_some()));
        assert!(delta
            .mutations
            .iter()
            .any(|m| m.path == "new" && m.previous_path.as_deref() == Some("old")));
        assert!(delta.mutations.iter().all(|m| !m.artifacts.is_empty()));
    }

    #[test]
    fn git_ignored_build_output_is_not_captured_or_emitted() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(artifacts.path()).unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(workspace.path())
            .status()
            .unwrap()
            .success());
        fs::write(workspace.path().join(".gitignore"), "target/\n").unwrap();
        fs::write(workspace.path().join("tracked.txt"), "before").unwrap();
        assert!(Command::new("git")
            .args(["add", ".gitignore", "tracked.txt"])
            .current_dir(workspace.path())
            .status()
            .unwrap()
            .success());
        let before = capture_workspace(workspace.path(), &store, &Allow).unwrap();

        fs::create_dir(workspace.path().join("target")).unwrap();
        fs::write(workspace.path().join("target/output"), "generated").unwrap();
        fs::write(workspace.path().join("tracked.txt"), "after").unwrap();
        let delta = before.diff(capture_workspace(workspace.path(), &store, &Allow).unwrap());

        assert_eq!(delta.mutations.len(), 1);
        assert_eq!(delta.mutations[0].path, "tracked.txt");
    }

    #[test]
    fn policy_excludes_bytes_before_the_artifact_store_reads_them() {
        struct ExcludeEnv;

        impl CapturePolicy for ExcludeEnv {
            fn decide(&self, subject: &CaptureSubject<'_>) -> CaptureDecision {
                if subject.path.is_some_and(|path| path.as_str() == ".env") {
                    CaptureDecision::Exclude {
                        reason: "environment files may contain secrets".into(),
                    }
                } else {
                    CaptureDecision::Include
                }
            }
        }

        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(artifacts.path()).unwrap();
        let secret = b"TOKEN=do-not-store";
        fs::write(workspace.path().join(".env"), secret).unwrap();
        fs::write(workspace.path().join("public.txt"), b"captured").unwrap();

        let before = capture_workspace(workspace.path(), &store, &ExcludeEnv).unwrap();
        let excluded = &before.files[".env"].artifact;
        assert!(matches!(&excluded.state, ArtifactState::Excluded { .. }));
        assert!(!store.contains(BlobId::digest(secret)).unwrap());
        assert!(store.contains(BlobId::digest(b"captured")).unwrap());

        fs::write(workspace.path().join(".env"), b"TOKEN=still-private").unwrap();
        let delta = before.diff(capture_workspace(workspace.path(), &store, &ExcludeEnv).unwrap());
        assert!(delta.mutations.is_empty());
        assert_eq!(
            delta.issues.len(),
            1,
            "before/after issues are deduplicated"
        );
    }
}
