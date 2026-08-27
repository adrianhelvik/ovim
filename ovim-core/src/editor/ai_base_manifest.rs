use super::Editor;
use crate::ai::path_policy::sensitive_path_reason;
use crate::run_log::{
    capture_base_manifest, discover_git_manifest, ArtifactStore, BaseManifest, BaseManifestId,
    CaptureDecision, CaptureKind, CaptureLimits, CapturePolicy, CaptureSubject, ContentRequest,
    EditorOverlayInput, GitManifestAdapterError, GitManifestMetadata, GitSnapshotContentReader,
    ManifestConfidence, ManifestIssue, RepoPath, RepositoryId, SnapshotContentReader,
    UnsavedBufferInput,
};
use ropey::Rope;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorBaseManifestIssue {
    pub path: Option<String>,
    pub ephemeral_buffer_id: Option<u64>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorBaseManifestCapture {
    pub manifest: BaseManifest,
    pub adapter_issues: Vec<EditorBaseManifestIssue>,
}

struct EditorManifestReader {
    git: GitSnapshotContentReader,
    ropes: HashMap<String, Rope>,
}

impl SnapshotContentReader for EditorManifestReader {
    fn read(&self, request: &ContentRequest, max_bytes: u64) -> Result<Vec<u8>, String> {
        if let Some(rope) = self.ropes.get(&request.locator) {
            if rope.len_bytes() as u64 > max_bytes {
                return Err(format!("editor buffer exceeds {max_bytes} byte read limit"));
            }
            // Rope cloning above is structural. UTF-8 serialization remains
            // deferred until after capture policy approves this request.
            return Ok(rope.to_string().into_bytes());
        }
        self.git.read(request, max_bytes)
    }
}

pub(super) struct DefaultEditorCapturePolicy;

impl CapturePolicy for DefaultEditorCapturePolicy {
    fn decide(&self, subject: &CaptureSubject<'_>) -> CaptureDecision {
        // A pathless unsaved buffer is an unnamed scratch buffer: it carries no
        // path for the sensitive-path net to screen, and its contents may be a
        // pasted secret. It cannot be classified, so fail closed and never
        // expose anonymous scratch buffers to delegated child agents. Excluding
        // here — at capture — means the bytes are never read into the artifact
        // store. Named/file-backed buffers keep the path-based screening below;
        // a future explicit affordance can opt an individual scratch buffer in.
        if subject.path.is_none() && matches!(subject.kind, CaptureKind::UnsavedBuffer) {
            return CaptureDecision::Exclude {
                reason: "unnamed scratch buffer is not shared with delegated agents".into(),
            };
        }
        subject
            .path
            .and_then(|path| sensitive_path_reason(Path::new(path.as_str())))
            .map_or(CaptureDecision::Include, |reason| {
                CaptureDecision::Exclude {
                    reason: reason.into(),
                }
            })
    }
}

#[derive(Clone)]
struct CapturedBufferVersion {
    buffer_id: u64,
    version: usize,
    path: Option<RepoPath>,
    kind: CaptureKind,
}

impl Editor {
    /// Captures the current Git workspace plus authoritative editor buffers.
    ///
    /// Git and Rope bytes remain deferred until the manifest capture policy
    /// approves each artifact. The method is synchronous so the editor cannot
    /// mutate through safe UI code during capture; versions are nevertheless
    /// rechecked to make future background capture fault-local. Repository
    /// selection is deliberately explicit so multi-repository sessions cannot
    /// silently anchor a run to whichever buffer happens to be first.
    pub fn capture_ai_base_manifest(
        &self,
        repository_start: impl AsRef<Path>,
        repository_id: RepositoryId,
        base_manifest_id: BaseManifestId,
        captured_at: impl Into<String>,
        artifact_store: &ArtifactStore,
    ) -> Result<EditorBaseManifestCapture, GitManifestAdapterError> {
        let mut git = discover_git_manifest(
            repository_start,
            GitManifestMetadata {
                repository_id,
                base_manifest_id,
                captured_at: captured_at.into(),
            },
        )?;
        let root = git.reader.workdir().to_owned();
        let mut ropes = HashMap::new();
        let mut versions = Vec::new();
        let mut adapter_issues: Vec<_> = git
            .issues
            .iter()
            .map(|issue| EditorBaseManifestIssue {
                path: issue.path.clone(),
                ephemeral_buffer_id: None,
                detail: issue.detail.clone(),
            })
            .collect();

        for buffer in &self.buffers {
            if let Some(file_path) = buffer.file_path() {
                let Some(path) = repo_relative_path(&root, Path::new(file_path)) else {
                    // Bracket-named scratch/generated buffers and files from
                    // another repository are not part of this base.
                    continue;
                };
                let locator = register_rope(&mut ropes, buffer.rope().clone());
                let overlay = EditorOverlayInput {
                    content: ContentRequest {
                        locator,
                        declared_bytes: Some(buffer.rope().len_bytes() as u64),
                    },
                    version: Some(buffer.version() as u64),
                    modified: buffer.is_modified(),
                    encoding: Some(buffer.encoding().display_name().into()),
                    line_endings: Some(buffer.line_ending().display_name().into()),
                };
                if let Err(detail) = git.attach_editor_overlay(path.as_str(), overlay) {
                    adapter_issues.push(EditorBaseManifestIssue {
                        path: Some(path.to_string()),
                        ephemeral_buffer_id: Some(buffer.id()),
                        detail,
                    });
                    continue;
                }
                versions.push(CapturedBufferVersion {
                    buffer_id: buffer.id(),
                    version: buffer.version(),
                    path: Some(path),
                    kind: CaptureKind::Editor,
                });
            } else if buffer.is_modified()
                && !buffer.is_read_only()
                && buffer.rope().len_bytes() > 0
            {
                let locator = register_rope(&mut ropes, buffer.rope().clone());
                git.add_unsaved_buffer(UnsavedBufferInput {
                    ephemeral_buffer_id: Some(buffer.id().to_string()),
                    display_name: None,
                    content: ContentRequest {
                        locator,
                        declared_bytes: Some(buffer.rope().len_bytes() as u64),
                    },
                    version: Some(buffer.version() as u64),
                    modified: true,
                    encoding: Some(buffer.encoding().display_name().into()),
                    line_endings: Some(buffer.line_ending().display_name().into()),
                });
                versions.push(CapturedBufferVersion {
                    buffer_id: buffer.id(),
                    version: buffer.version(),
                    path: None,
                    kind: CaptureKind::UnsavedBuffer,
                });
            }
        }

        let reader = EditorManifestReader {
            git: git.reader,
            ropes,
        };
        let mut manifest = capture_base_manifest(
            git.input,
            artifact_store,
            &DefaultEditorCapturePolicy,
            &reader,
            CaptureLimits::default(),
        );

        // Git adapter issues (for example conflicts or rename provenance) make
        // replay partial even when all retained artifacts were captured.
        for issue in &adapter_issues {
            manifest.issues.push(ManifestIssue {
                path: issue
                    .path
                    .as_deref()
                    .and_then(|path| RepoPath::parse(path).ok()),
                ephemeral_buffer_id: issue.ephemeral_buffer_id.map(|id| id.to_string()),
                kind: CaptureKind::Disk,
                detail: issue.detail.clone(),
            });
        }
        for captured in versions {
            let changed = self
                .get_buffer_by_id(captured.buffer_id)
                .is_none_or(|buffer| buffer.version() != captured.version);
            if changed {
                let detail = "buffer changed while its base manifest was captured".to_string();
                manifest.issues.push(ManifestIssue {
                    path: captured.path.clone(),
                    ephemeral_buffer_id: Some(captured.buffer_id.to_string()),
                    kind: captured.kind,
                    detail: detail.clone(),
                });
                adapter_issues.push(EditorBaseManifestIssue {
                    path: captured.path.map(|path| path.to_string()),
                    ephemeral_buffer_id: Some(captured.buffer_id),
                    detail,
                });
            }
        }
        if !manifest.issues.is_empty() {
            manifest.confidence = ManifestConfidence::Partial;
        }
        Ok(EditorBaseManifestCapture {
            manifest,
            adapter_issues,
        })
    }
}

fn register_rope(ropes: &mut HashMap<String, Rope>, rope: Rope) -> String {
    let locator = format!("editor-snapshot:{}", ropes.len());
    ropes.insert(locator.clone(), rope);
    locator
}

fn repo_relative_path(root: &Path, path: &Path) -> Option<RepoPath> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let normalized = lexical_normalize(&absolute);
    let relative = normalized.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        parts.push(component.to_str()?);
    }
    RepoPath::parse(parts.join("/")).ok()
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::run_log::{ArtifactState, ContentRepresentation, GitManifestMetadata};
    use crate::unicode::CharCol;
    use git2::{Repository, Signature};
    use std::fs;

    fn ids() -> (RepositoryId, BaseManifestId) {
        (
            RepositoryId::parse("repo_editor_manifest").unwrap(),
            BaseManifestId::parse("bsm_editor_manifest").unwrap(),
        )
    }

    fn commit_files(repository: &Repository) {
        let mut index = repository.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = Signature::now("ovim", "ovim@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
            .unwrap();
    }

    fn capture(
        editor: &Editor,
        repository_start: &Path,
        store: &ArtifactStore,
    ) -> EditorBaseManifestCapture {
        let (repository_id, manifest_id) = ids();
        editor
            .capture_ai_base_manifest(
                repository_start,
                repository_id,
                manifest_id,
                "2026-07-13T15:00:00Z",
                store,
            )
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn unmodified_open_buffer_overrides_externally_changed_crlf_disk() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(directory.path()).unwrap();
        let path = directory.path().join("file.txt");
        fs::write(&path, b"original\r\n").unwrap();
        commit_files(&repository);
        let mut editor = Editor::new();
        editor.open_file(&path).unwrap();
        fs::write(&path, b"external\r\n").unwrap();
        let blobs = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(blobs.path()).unwrap();

        let captured = capture(&editor, directory.path(), &store);
        let file = captured
            .manifest
            .files
            .iter()
            .find(|file| file.path.as_str() == "file.txt")
            .unwrap();
        assert!(!file.editor.as_ref().unwrap().modified);
        let mut raw = None;
        let mut editor_text = None;
        for artifact in &captured.manifest.artifacts {
            let ArtifactState::Available { blob_id, .. } = artifact.state else {
                continue;
            };
            match artifact.representation {
                ContentRepresentation::RawBytes => raw = Some(store.read(blob_id).unwrap()),
                ContentRepresentation::EditorText { .. } => {
                    editor_text = Some(store.read(blob_id).unwrap())
                }
            }
        }
        assert_eq!(raw.unwrap(), b"external\r\n");
        assert_eq!(editor_text.unwrap(), b"original\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn records_modified_pathless_buffer_but_excludes_its_content() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(directory.path()).unwrap();
        fs::write(directory.path().join("base"), b"base").unwrap();
        commit_files(&repository);
        let mut editor = Editor::new();
        editor.open_file(directory.path().join("base")).unwrap();
        let mut draft = Buffer::new_from_str("draft");
        draft.insert_text_at(0, CharCol::ZERO, "changed ");
        editor.push_buffer(draft);
        let mut generated = Buffer::new_from_str("generated");
        generated.insert_text_at(0, CharCol::ZERO, "changed ");
        generated.set_read_only(true);
        editor.push_buffer(generated);
        let blobs = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(blobs.path()).unwrap();

        let captured = capture(&editor, directory.path(), &store);
        // The read-only pathless buffer is dropped at capture; the modified one
        // is still recorded as a manifest entry...
        assert_eq!(captured.manifest.unsaved_buffers.len(), 1);
        assert!(captured.manifest.unsaved_buffers[0].modified);
        // ...but its content is excluded from the artifact store, so an unnamed
        // scratch buffer (which could hold a pasted secret) never reaches a
        // delegated child agent.
        assert!(captured.manifest.artifacts.iter().any(|artifact| matches!(
            &artifact.state,
            ArtifactState::Excluded { reason } if reason.contains("scratch buffer")
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn default_policy_excludes_sensitive_open_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(directory.path()).unwrap();
        let env = directory.path().join(".env");
        fs::write(&env, b"TOKEN=secret\n").unwrap();
        commit_files(&repository);
        let mut editor = Editor::new();
        editor.open_file(&env).unwrap();
        let blobs = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(blobs.path()).unwrap();

        let captured = capture(&editor, directory.path(), &store);
        assert!(captured.manifest.artifacts.iter().any(|artifact| matches!(
            artifact.state,
            ArtifactState::Excluded { ref reason } if reason.contains(".env")
        )));
        assert_eq!(captured.manifest.confidence, ManifestConfidence::Partial);
    }

    #[test]
    fn git_discovery_remains_compatible_with_editor_capture_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(directory.path()).unwrap();
        fs::write(directory.path().join("base"), b"base").unwrap();
        commit_files(&repository);
        let (repository_id, base_manifest_id) = ids();
        let git = discover_git_manifest(
            directory.path(),
            GitManifestMetadata {
                repository_id,
                base_manifest_id,
                captured_at: "2026-07-13T15:00:00Z".into(),
            },
        )
        .unwrap();
        assert!(git.input.files.is_empty());
    }
}
