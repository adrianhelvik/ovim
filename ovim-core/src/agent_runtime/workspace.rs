//! Immutable, read-only workspaces for delegated agents.
//!
//! A projection combines the clean tree at the base manifest's recorded HEAD
//! with content-addressed manifest overlays. It never consults the current
//! worktree or editor state after construction.

use super::{
    AgentApprovalBroker, AgentApprovalContext, AgentCapability, AgentCapabilityCeiling,
    AgentDispatchRecord, AgentFuture, AgentLoopDependencies, AgentLoopInputFactory,
    AgentProviderAdapter, AgentToolCall, AgentToolError, AgentToolExecutor, AgentToolResult,
    AgentWorkspaceDescriptor, DelegationEnvelope, DenyAllAgentApprovals, ScopedTool,
    ScopedToolView, WorkspaceStrategy,
};
use crate::ai::path_policy::sensitive_path_reason;
use crate::ai::tools::StrictJsonSchema;
use crate::run_log::{
    ArtifactId, ArtifactState, ArtifactStore, BaseManifest, BaseManifestId, BlobId, FileKind,
    GitBaseEntry, ManifestConfidence, ManifestId, ManifestLayer, RepoPath, RepositoryId,
    ToolSideEffect,
};
use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SNAPSHOT_READ_FILE_TOOL: &str = "read_file_at_path";
pub const SNAPSHOT_LIST_FILES_TOOL: &str = "list_files";
pub const SNAPSHOT_SEARCH_TOOL: &str = "search_project";
pub const SNAPSHOT_READ_UNSAVED_TOOL: &str = "read_unsaved_buffer";
pub const SNAPSHOT_SEARCH_SYMBOLS_TOOL: &str = "search_symbols";
pub const SNAPSHOT_READ_DIAGNOSTICS_TOOL: &str = "read_diagnostics";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkspaceWarningKind {
    PartialCapture,
    CaptureIssue,
    MissingArtifact,
    ExcludedArtifact,
    RedactedArtifact,
    MissingGitObject,
    SensitivePathExcluded,
    UnsupportedGitEntry,
    WarningsOmitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceWarning {
    pub kind: AgentWorkspaceWarningKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<ArtifactId>,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentWorkspaceLimits {
    pub max_read_bytes: u64,
    pub max_read_lines: usize,
    pub default_list_results: usize,
    pub max_list_results: usize,
    pub default_search_results: usize,
    pub max_search_results: usize,
    pub max_search_files: usize,
    pub max_search_bytes: u64,
    pub max_search_file_bytes: u64,
    pub max_query_bytes: usize,
    pub max_result_line_bytes: usize,
    pub max_workspace_warnings: usize,
}

impl Default for AgentWorkspaceLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 1024 * 1024,
            max_read_lines: 2_000,
            default_list_results: 200,
            max_list_results: 1_000,
            default_search_results: 50,
            max_search_results: 200,
            max_search_files: 10_000,
            max_search_bytes: 32 * 1024 * 1024,
            max_search_file_bytes: 1024 * 1024,
            max_query_bytes: 1_024,
            max_result_line_bytes: 1_024,
            max_workspace_warnings: 64,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceIdentity {
    pub manifest_id: ManifestId,
    pub base_manifest_id: BaseManifestId,
    pub repository_id: RepositoryId,
    pub captured_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceFileMetadata {
    pub path: RepoPath,
    pub file_kind: FileKind,
    pub executable: bool,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceUnsavedBuffer {
    pub entry_id: ArtifactId,
    pub display_name: Option<String>,
    pub version: Option<u64>,
    pub modified: bool,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceReadResult {
    pub path: RepoPath,
    pub file_kind: FileKind,
    pub executable: bool,
    pub byte_len: u64,
    pub binary: bool,
    pub total_lines: usize,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub text: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceUnsavedReadResult {
    pub entry_id: ArtifactId,
    pub display_name: Option<String>,
    pub version: Option<u64>,
    pub modified: bool,
    pub byte_len: u64,
    pub binary: bool,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceListResult {
    pub prefix: Option<RepoPath>,
    pub entries: Vec<AgentWorkspaceFileMetadata>,
    pub total: usize,
    pub truncated: bool,
    pub unsaved_buffers: Vec<AgentWorkspaceUnsavedBuffer>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceSearchMatch {
    pub path: RepoPath,
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceSearchResult {
    pub query: String,
    pub matches: Vec<AgentWorkspaceSearchMatch>,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub skipped_binary: usize,
    pub skipped_unavailable: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSymbol {
    pub name: String,
    pub kind: String,
    pub path: RepoPath,
    pub line: usize,
    pub column: usize,
}

/// Contract for a symbol index captured against one immutable manifest.
/// Live editor/LSP state must not implement this contract without first being
/// copied into a manifest-bound index.
pub trait SnapshotSymbolAdapter: Send + Sync {
    fn manifest_id(&self) -> &ManifestId;
    fn search(&self, query: &str, maximum: usize) -> Result<Vec<SnapshotSymbol>, String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDiagnostic {
    pub path: RepoPath,
    pub line: usize,
    pub column: usize,
    pub severity: String,
    pub message: String,
}

/// Contract for diagnostics captured against one immutable manifest. The
/// current live LSP projection is deliberately not exposed to child agents.
pub trait SnapshotDiagnosticAdapter: Send + Sync {
    fn manifest_id(&self) -> &ManifestId;
    fn diagnostics(
        &self,
        path_prefix: Option<&RepoPath>,
        maximum: usize,
    ) -> Result<Vec<SnapshotDiagnostic>, String>;
}

#[derive(Clone)]
pub struct CapturedSnapshotSymbolIndex {
    manifest_id: ManifestId,
    symbols: Vec<SnapshotSymbol>,
}

impl CapturedSnapshotSymbolIndex {
    pub fn new(manifest_id: ManifestId, symbols: Vec<SnapshotSymbol>) -> Self {
        Self {
            manifest_id,
            symbols,
        }
    }
}

impl SnapshotSymbolAdapter for CapturedSnapshotSymbolIndex {
    fn manifest_id(&self) -> &ManifestId {
        &self.manifest_id
    }

    fn search(&self, query: &str, maximum: usize) -> Result<Vec<SnapshotSymbol>, String> {
        let query = query.to_lowercase();
        Ok(self
            .symbols
            .iter()
            .filter(|symbol| symbol.name.to_lowercase().contains(&query))
            .take(maximum)
            .cloned()
            .collect())
    }
}

#[derive(Clone)]
pub struct CapturedSnapshotDiagnostics {
    manifest_id: ManifestId,
    diagnostics: Vec<SnapshotDiagnostic>,
}

impl CapturedSnapshotDiagnostics {
    pub fn new(manifest_id: ManifestId, diagnostics: Vec<SnapshotDiagnostic>) -> Self {
        Self {
            manifest_id,
            diagnostics,
        }
    }
}

impl SnapshotDiagnosticAdapter for CapturedSnapshotDiagnostics {
    fn manifest_id(&self) -> &ManifestId {
        &self.manifest_id
    }

    fn diagnostics(
        &self,
        path_prefix: Option<&RepoPath>,
        maximum: usize,
    ) -> Result<Vec<SnapshotDiagnostic>, String> {
        Ok(self
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                path_prefix.is_none_or(|prefix| {
                    diagnostic.path == *prefix
                        || is_descendant(diagnostic.path.as_str(), prefix.as_str())
                })
            })
            .take(maximum)
            .cloned()
            .collect())
    }
}

#[derive(Clone)]
pub struct AgentWorkspaceManager {
    inner: Arc<AgentWorkspaceManagerInner>,
}

struct AgentWorkspaceManagerInner {
    artifact_store: ArtifactStore,
    limits: AgentWorkspaceLimits,
    read_only: Mutex<HashMap<ManifestId, Arc<ReadOnlyAgentWorkspace>>>,
    symbols: Mutex<HashMap<ManifestId, Arc<dyn SnapshotSymbolAdapter>>>,
    diagnostics: Mutex<HashMap<ManifestId, Arc<dyn SnapshotDiagnosticAdapter>>>,
}

impl AgentWorkspaceManager {
    pub fn new(artifact_store: ArtifactStore) -> Self {
        Self::with_limits(artifact_store, AgentWorkspaceLimits::default())
    }

    pub fn with_limits(artifact_store: ArtifactStore, limits: AgentWorkspaceLimits) -> Self {
        Self {
            inner: Arc::new(AgentWorkspaceManagerInner {
                artifact_store,
                limits,
                read_only: Mutex::new(HashMap::new()),
                symbols: Mutex::new(HashMap::new()),
                diagnostics: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn register_read_only(
        &self,
        manifest_id: ManifestId,
        manifest: BaseManifest,
        repository_start: impl AsRef<Path>,
    ) -> Result<Arc<ReadOnlyAgentWorkspace>, AgentWorkspaceError> {
        let workspace = Arc::new(ReadOnlyAgentWorkspace::build(
            manifest_id.clone(),
            manifest,
            repository_start.as_ref(),
            self.inner.artifact_store.clone(),
            self.inner.limits,
        )?);
        let mut projections = self.inner.read_only.lock().map_err(|_| {
            AgentWorkspaceError::Internal("workspace projection registry is poisoned".into())
        })?;
        if projections.contains_key(&manifest_id) {
            return Err(AgentWorkspaceError::DuplicateManifest(manifest_id));
        }
        projections.insert(manifest_id, workspace.clone());
        Ok(workspace)
    }

    pub fn read_only(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<Option<Arc<ReadOnlyAgentWorkspace>>, AgentWorkspaceError> {
        Ok(self
            .inner
            .read_only
            .lock()
            .map_err(|_| {
                AgentWorkspaceError::Internal("workspace projection registry is poisoned".into())
            })?
            .get(manifest_id)
            .cloned())
    }

    pub fn register_snapshot_adapters(
        &self,
        manifest_id: &ManifestId,
        symbols: Arc<dyn SnapshotSymbolAdapter>,
        diagnostics: Arc<dyn SnapshotDiagnosticAdapter>,
    ) -> Result<(), AgentWorkspaceError> {
        if symbols.manifest_id() != manifest_id || diagnostics.manifest_id() != manifest_id {
            return Err(AgentWorkspaceError::Internal(
                "snapshot adapter manifest identity mismatch".into(),
            ));
        }
        let mut symbol_indexes = self.inner.symbols.lock().map_err(|_| {
            AgentWorkspaceError::Internal("symbol adapter registry is poisoned".into())
        })?;
        let mut diagnostic_indexes = self.inner.diagnostics.lock().map_err(|_| {
            AgentWorkspaceError::Internal("diagnostic adapter registry is poisoned".into())
        })?;
        if symbol_indexes.contains_key(manifest_id) || diagnostic_indexes.contains_key(manifest_id)
        {
            return Err(AgentWorkspaceError::DuplicateManifest(manifest_id.clone()));
        }
        symbol_indexes.insert(manifest_id.clone(), symbols);
        diagnostic_indexes.insert(manifest_id.clone(), diagnostics);
        Ok(())
    }

    fn snapshot_adapters(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<SnapshotAdapters, AgentWorkspaceError> {
        let symbols = self
            .inner
            .symbols
            .lock()
            .map_err(|_| {
                AgentWorkspaceError::Internal("symbol adapter registry is poisoned".into())
            })?
            .get(manifest_id)
            .cloned();
        let diagnostics = self
            .inner
            .diagnostics
            .lock()
            .map_err(|_| {
                AgentWorkspaceError::Internal("diagnostic adapter registry is poisoned".into())
            })?
            .get(manifest_id)
            .cloned();
        Ok(SnapshotAdapters {
            symbols,
            diagnostics,
        })
    }
}

struct SnapshotAdapters {
    symbols: Option<Arc<dyn SnapshotSymbolAdapter>>,
    diagnostics: Option<Arc<dyn SnapshotDiagnosticAdapter>>,
}

#[derive(Clone)]
pub struct ReadOnlyAgentWorkspace {
    identity: AgentWorkspaceIdentity,
    repository_git_dir: PathBuf,
    artifact_store: ArtifactStore,
    files: BTreeMap<RepoPath, SnapshotFile>,
    unsaved_buffers: BTreeMap<ArtifactId, SnapshotUnsavedBuffer>,
    warnings: Vec<AgentWorkspaceWarning>,
    limits: AgentWorkspaceLimits,
}

#[derive(Clone)]
struct SnapshotFile {
    file_kind: FileKind,
    executable: bool,
    content: SnapshotContent,
}

#[derive(Clone)]
struct SnapshotUnsavedBuffer {
    display_name: Option<String>,
    version: Option<u64>,
    modified: bool,
    content: SnapshotContent,
}

#[derive(Clone)]
enum SnapshotContent {
    Absent,
    GitBlob { object_id: String, byte_len: u64 },
    Artifact { blob_id: BlobId, byte_len: u64 },
    Unavailable { detail: String },
    Directory,
}

impl ReadOnlyAgentWorkspace {
    fn build(
        manifest_id: ManifestId,
        manifest: BaseManifest,
        repository_start: &Path,
        artifact_store: ArtifactStore,
        limits: AgentWorkspaceLimits,
    ) -> Result<Self, AgentWorkspaceError> {
        let repository = Repository::discover(repository_start)
            .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
        let repository_git_dir = repository
            .path()
            .canonicalize()
            .unwrap_or_else(|_| repository.path().to_owned());
        let identity = AgentWorkspaceIdentity {
            manifest_id,
            base_manifest_id: manifest.base_manifest_id.clone(),
            repository_id: manifest.repository.repository_id.clone(),
            captured_at: manifest.captured_at.clone(),
        };
        let mut warnings = manifest_warnings(&manifest);
        let mut files = BTreeMap::new();
        if let Some(commit_id) = manifest.repository.head_commit.as_deref() {
            enumerate_git_tree(&repository, commit_id, &mut files, &mut warnings)?;
        }
        let artifacts: HashMap<_, _> = manifest
            .artifacts
            .iter()
            .map(|record| (record.artifact_id.clone(), record))
            .collect();

        for file in &manifest.files {
            let base = match &file.git_base {
                GitBaseEntry::Absent => SnapshotContent::Absent,
                GitBaseEntry::Blob { object_id } => {
                    git_blob_content(&repository, object_id, Some(&file.path), &mut warnings)
                }
            };
            let index = resolve_manifest_layer(
                &file.index,
                base,
                &artifacts,
                &artifact_store,
                Some(&file.path),
                &mut warnings,
            );
            let disk = resolve_manifest_layer(
                &file.disk,
                index,
                &artifacts,
                &artifact_store,
                Some(&file.path),
                &mut warnings,
            );
            let visible = file.editor.as_ref().map_or(disk, |editor| {
                artifact_content(
                    &editor.artifact_id,
                    &artifacts,
                    &artifact_store,
                    Some(&file.path),
                    &mut warnings,
                )
            });
            if matches!(visible, SnapshotContent::Absent) {
                files.remove(&file.path);
            } else {
                files.insert(
                    file.path.clone(),
                    SnapshotFile {
                        file_kind: file.file_kind.clone(),
                        executable: file.executable,
                        content: visible,
                    },
                );
            }
        }

        exclude_sensitive_git_paths(&mut files, &mut warnings);
        add_derived_directories(&mut files);

        let mut unsaved_buffers = BTreeMap::new();
        for unsaved in &manifest.unsaved_buffers {
            let content = artifact_content(
                &unsaved.artifact_id,
                &artifacts,
                &artifact_store,
                None,
                &mut warnings,
            );
            unsaved_buffers.insert(
                unsaved.entry_id.clone(),
                SnapshotUnsavedBuffer {
                    display_name: unsaved.display_name.clone(),
                    version: unsaved.version,
                    modified: unsaved.modified,
                    content,
                },
            );
        }

        Ok(Self {
            identity,
            repository_git_dir,
            artifact_store,
            files,
            unsaved_buffers,
            warnings: bound_warnings(
                deduplicate_warnings(warnings),
                limits.max_workspace_warnings,
            ),
            limits,
        })
    }

    pub fn identity(&self) -> &AgentWorkspaceIdentity {
        &self.identity
    }

    pub fn warnings(&self) -> &[AgentWorkspaceWarning] {
        &self.warnings
    }

    pub fn unsaved_buffers(&self) -> Vec<AgentWorkspaceUnsavedBuffer> {
        self.unsaved_buffers
            .iter()
            .map(|(entry_id, buffer)| AgentWorkspaceUnsavedBuffer {
                entry_id: entry_id.clone(),
                display_name: buffer.display_name.clone(),
                version: buffer.version,
                modified: buffer.modified,
                available: buffer.content.is_available(),
            })
            .collect()
    }

    pub fn read(
        &self,
        path: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<AgentWorkspaceReadResult, AgentWorkspaceError> {
        let path = checked_repo_path(path)?;
        self.reject_symlink_ancestor(&path)?;
        let file = self
            .files
            .get(&path)
            .ok_or_else(|| AgentWorkspaceError::NotFound(path.to_string()))?;
        if file.file_kind == FileKind::Directory {
            return Err(AgentWorkspaceError::NotAFile(path.to_string()));
        }
        let bytes = self.load_content(&file.content, self.limits.max_read_bytes)?;
        let byte_len = bytes.len() as u64;
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(AgentWorkspaceReadResult {
                path,
                file_kind: file.file_kind.clone(),
                executable: file.executable,
                byte_len,
                binary: true,
                total_lines: 0,
                start_line: None,
                end_line: None,
                text: None,
                truncated: false,
            });
        };
        let lines: Vec<_> = text.lines().collect();
        let total_lines = lines.len();
        let start = start_line.unwrap_or(1);
        if start == 0 {
            return Err(AgentWorkspaceError::InvalidArguments(
                "start_line must be one or greater".into(),
            ));
        }
        let requested_end = end_line.unwrap_or_else(|| {
            start
                .saturating_add(self.limits.max_read_lines)
                .saturating_sub(1)
        });
        if requested_end < start {
            return Err(AgentWorkspaceError::InvalidArguments(
                "end_line must not precede start_line".into(),
            ));
        }
        let bounded_end = requested_end.min(
            start
                .saturating_add(self.limits.max_read_lines)
                .saturating_sub(1),
        );
        let first = start.saturating_sub(1).min(total_lines);
        let last = bounded_end.min(total_lines);
        let selected = if first < last {
            lines[first..last].join("\n")
        } else {
            String::new()
        };
        Ok(AgentWorkspaceReadResult {
            path,
            file_kind: file.file_kind.clone(),
            executable: file.executable,
            byte_len,
            binary: false,
            total_lines,
            start_line: (first < last).then_some(first + 1),
            end_line: (first < last).then_some(last),
            text: Some(selected),
            truncated: first > 0 || last < total_lines || requested_end != bounded_end,
        })
    }

    pub fn read_unsaved(
        &self,
        entry_id: &ArtifactId,
    ) -> Result<AgentWorkspaceUnsavedReadResult, AgentWorkspaceError> {
        let buffer = self
            .unsaved_buffers
            .get(entry_id)
            .ok_or_else(|| AgentWorkspaceError::UnsavedBufferNotFound(entry_id.clone()))?;
        let bytes = self.load_content(&buffer.content, self.limits.max_read_bytes)?;
        let byte_len = bytes.len() as u64;
        let text = String::from_utf8(bytes).ok();
        Ok(AgentWorkspaceUnsavedReadResult {
            entry_id: entry_id.clone(),
            display_name: buffer.display_name.clone(),
            version: buffer.version,
            modified: buffer.modified,
            byte_len,
            binary: text.is_none(),
            text,
        })
    }

    pub fn list(
        &self,
        prefix: Option<&str>,
        maximum: Option<usize>,
    ) -> Result<AgentWorkspaceListResult, AgentWorkspaceError> {
        let prefix = prefix
            .filter(|value| !value.trim().is_empty())
            .map(checked_repo_path)
            .transpose()?;
        if let Some(prefix) = &prefix {
            self.reject_symlink_ancestor(prefix)?;
            match self.files.get(prefix) {
                Some(file) if file.file_kind == FileKind::Directory => {}
                Some(_) => return Err(AgentWorkspaceError::NotADirectory(prefix.to_string())),
                None => return Err(AgentWorkspaceError::NotFound(prefix.to_string())),
            }
        }
        let maximum = maximum
            .unwrap_or(self.limits.default_list_results)
            .min(self.limits.max_list_results);
        let mut matching = Vec::new();
        for (path, file) in &self.files {
            if prefix.as_ref().is_some_and(|prefix| {
                path != prefix && !is_descendant(path.as_str(), prefix.as_str())
            }) {
                continue;
            }
            if prefix.as_ref() == Some(path) {
                continue;
            }
            matching.push(AgentWorkspaceFileMetadata {
                path: path.clone(),
                file_kind: file.file_kind.clone(),
                executable: file.executable,
                available: file.content.is_available(),
            });
        }
        let total = matching.len();
        matching.truncate(maximum);
        let include_unsaved_buffers = prefix.is_none();
        Ok(AgentWorkspaceListResult {
            prefix,
            entries: matching,
            total,
            truncated: total > maximum,
            unsaved_buffers: if include_unsaved_buffers {
                self.unsaved_buffers()
            } else {
                Vec::new()
            },
        })
    }

    pub fn search(
        &self,
        query: &str,
        path_prefix: Option<&str>,
        maximum: Option<usize>,
    ) -> Result<AgentWorkspaceSearchResult, AgentWorkspaceError> {
        if query.is_empty() {
            return Err(AgentWorkspaceError::InvalidArguments(
                "query must not be empty".into(),
            ));
        }
        if query.len() > self.limits.max_query_bytes {
            return Err(AgentWorkspaceError::InvalidArguments(format!(
                "query exceeds {} bytes",
                self.limits.max_query_bytes
            )));
        }
        let expression = RegexBuilder::new(query)
            .size_limit(2 * 1024 * 1024)
            .build()
            .map_err(|error| AgentWorkspaceError::InvalidArguments(error.to_string()))?;
        let prefix = path_prefix
            .filter(|value| !value.trim().is_empty())
            .map(checked_repo_path)
            .transpose()?;
        if let Some(prefix) = &prefix {
            self.reject_symlink_ancestor(prefix)?;
        }
        let maximum = maximum
            .unwrap_or(self.limits.default_search_results)
            .min(self.limits.max_search_results);
        let mut result = AgentWorkspaceSearchResult {
            query: query.into(),
            matches: Vec::new(),
            files_scanned: 0,
            bytes_scanned: 0,
            skipped_binary: 0,
            skipped_unavailable: 0,
            truncated: false,
        };
        for (path, file) in &self.files {
            if file.file_kind == FileKind::Directory
                || prefix.as_ref().is_some_and(|prefix| {
                    path != prefix && !is_descendant(path.as_str(), prefix.as_str())
                })
            {
                continue;
            }
            if result.files_scanned >= self.limits.max_search_files
                || result.bytes_scanned >= self.limits.max_search_bytes
            {
                result.truncated = true;
                break;
            }
            let remaining = self
                .limits
                .max_search_bytes
                .saturating_sub(result.bytes_scanned)
                .min(self.limits.max_search_file_bytes);
            let bytes = match self.load_content(&file.content, remaining) {
                Ok(bytes) => bytes,
                Err(AgentWorkspaceError::Unavailable(_))
                | Err(AgentWorkspaceError::ContentTooLarge { .. }) => {
                    result.skipped_unavailable += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            result.files_scanned += 1;
            result.bytes_scanned += bytes.len() as u64;
            let Ok(text) = std::str::from_utf8(&bytes) else {
                result.skipped_binary += 1;
                continue;
            };
            for (line_index, line) in text.lines().enumerate() {
                for matched in expression.find_iter(line) {
                    result.matches.push(AgentWorkspaceSearchMatch {
                        path: path.clone(),
                        line: line_index + 1,
                        column: line[..matched.start()].chars().count() + 1,
                        text: truncate_utf8(line, self.limits.max_result_line_bytes),
                    });
                    if result.matches.len() >= maximum {
                        result.truncated = true;
                        return Ok(result);
                    }
                }
            }
        }
        Ok(result)
    }

    fn reject_symlink_ancestor(&self, path: &RepoPath) -> Result<(), AgentWorkspaceError> {
        let mut prefix = String::new();
        let parts: Vec<_> = path.as_str().split('/').collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let prefix_path = RepoPath::parse(prefix.clone())
                .expect("prefix of a normalized repository path is normalized");
            if self
                .files
                .get(&prefix_path)
                .is_some_and(|entry| entry.file_kind == FileKind::Symlink)
            {
                return Err(AgentWorkspaceError::SymlinkTraversal(path.to_string()));
            }
        }
        Ok(())
    }

    fn load_content(
        &self,
        content: &SnapshotContent,
        maximum: u64,
    ) -> Result<Vec<u8>, AgentWorkspaceError> {
        match content {
            SnapshotContent::Absent => Err(AgentWorkspaceError::Unavailable(
                "snapshot path is absent".into(),
            )),
            SnapshotContent::Directory => Err(AgentWorkspaceError::Unavailable(
                "snapshot entry is a directory".into(),
            )),
            SnapshotContent::Unavailable { detail } => {
                Err(AgentWorkspaceError::Unavailable(detail.clone()))
            }
            SnapshotContent::Artifact {
                blob_id, byte_len, ..
            } => {
                if *byte_len > maximum {
                    return Err(AgentWorkspaceError::ContentTooLarge {
                        actual: *byte_len,
                        maximum,
                    });
                }
                self.artifact_store
                    .read(*blob_id)
                    .map_err(|error| AgentWorkspaceError::Artifact(error.to_string()))
            }
            SnapshotContent::GitBlob {
                object_id,
                byte_len,
            } => {
                if *byte_len > maximum {
                    return Err(AgentWorkspaceError::ContentTooLarge {
                        actual: *byte_len,
                        maximum,
                    });
                }
                let repository = Repository::open(&self.repository_git_dir)
                    .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
                let oid = Oid::from_str(object_id)
                    .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
                let blob = repository
                    .find_blob(oid)
                    .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
                Ok(blob.content().to_vec())
            }
        }
    }
}

impl SnapshotContent {
    fn is_available(&self) -> bool {
        matches!(
            self,
            Self::GitBlob { .. } | Self::Artifact { .. } | Self::Directory
        )
    }
}

#[derive(Clone)]
pub struct SnapshotToolExecutor {
    workspace: Arc<ReadOnlyAgentWorkspace>,
    symbols: Option<Arc<dyn SnapshotSymbolAdapter>>,
    diagnostics: Option<Arc<dyn SnapshotDiagnosticAdapter>>,
}

/// Optional coordination surface layered over immutable snapshot tools.
///
/// The snapshot executor remains the authority for repository reads. Editor
/// integration may add bounded, capability-gated orchestration tools without
/// teaching the workspace layer about supervisors, provider routes, or UI.
pub trait AgentToolExtension: Send + Sync {
    fn scoped_tools(&self, dispatch: &AgentDispatchRecord) -> Result<Vec<ScopedTool>, String>;

    fn execute(
        &self,
        call: AgentToolCall,
    ) -> AgentFuture<'_, Result<AgentToolResult, AgentToolError>>;
}

#[derive(Clone)]
struct ExtendedSnapshotToolExecutor {
    snapshot: SnapshotToolExecutor,
    extension: Arc<dyn AgentToolExtension>,
    extension_names: BTreeSet<String>,
}

impl AgentToolExecutor for ExtendedSnapshotToolExecutor {
    fn execute(
        &self,
        call: AgentToolCall,
    ) -> AgentFuture<'_, Result<AgentToolResult, AgentToolError>> {
        if self.extension_names.contains(&call.tool_name) {
            self.extension.execute(call)
        } else {
            self.snapshot.execute(call)
        }
    }
}

impl SnapshotToolExecutor {
    pub fn new(workspace: Arc<ReadOnlyAgentWorkspace>) -> Self {
        Self {
            workspace,
            symbols: None,
            diagnostics: None,
        }
    }

    fn with_adapters(workspace: Arc<ReadOnlyAgentWorkspace>, adapters: SnapshotAdapters) -> Self {
        Self {
            workspace,
            symbols: adapters.symbols,
            diagnostics: adapters.diagnostics,
        }
    }

    pub fn scoped_view() -> ScopedToolView {
        ScopedToolView::new([
            ScopedTool {
                name: SNAPSHOT_READ_FILE_TOOL.into(),
                description: "Read a bounded line range from a captured repository file.".into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "minLength": 1 },
                        "start_line": { "type": "integer", "minimum": 1 },
                        "end_line": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["path"]
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            },
            ScopedTool {
                name: SNAPSHOT_LIST_FILES_TOOL.into(),
                description: "List files and captured unsaved buffers in the immutable snapshot."
                    .into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "minLength": 1 },
                        "max_results": { "type": "integer", "minimum": 1 }
                    }
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            },
            ScopedTool {
                name: SNAPSHOT_SEARCH_TOOL.into(),
                description: "Search text across bounded files in the immutable snapshot.".into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "query": { "type": "string", "minLength": 1 },
                        "path": { "type": "string", "minLength": 1 },
                        "max_results": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["query"]
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            },
            ScopedTool {
                name: SNAPSHOT_READ_UNSAVED_TOOL.into(),
                description: "Read one captured unsaved buffer by its opaque entry ID.".into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "entry_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["entry_id"]
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            },
        ])
        .expect("snapshot tool names are static and unique")
    }

    fn scoped_view_with_adapters(&self) -> ScopedToolView {
        let mut tools = Self::scoped_view().tools();
        if self.symbols.is_some() {
            tools.push(ScopedTool {
                name: SNAPSHOT_SEARCH_SYMBOLS_TOOL.into(),
                description: "Search the manifest-bound symbol index captured at dispatch.".into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "query": { "type": "string", "minLength": 1 },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 200 }
                    },
                    "required": ["query"]
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            });
        }
        if self.diagnostics.is_some() {
            tools.push(ScopedTool {
                name: SNAPSHOT_READ_DIAGNOSTICS_TOOL.into(),
                description: "Read diagnostics captured against this immutable manifest.".into(),
                input_schema: strict_schema(json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "minLength": 1 },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 500 }
                    }
                })),
                side_effect: ToolSideEffect::Read,
                required_capability: AgentCapability::Read,
                requires_approval: false,
            });
        }
        ScopedToolView::new(tools).expect("snapshot adapter tool names are static and unique")
    }

    fn execute_sync(&self, call: AgentToolCall) -> Result<AgentToolResult, AgentToolError> {
        if call.workspace.assignment != call.handle.workspace || !call.workspace.read_only {
            return Err(AgentToolError::new(
                "snapshot tool call has an invalid workspace descriptor",
            ));
        }
        let result = match call.tool_name.as_str() {
            SNAPSHOT_READ_FILE_TOOL => {
                let path = required_string(&call.arguments, "path")?;
                let start = optional_usize(&call.arguments, "start_line")?;
                let end = optional_usize(&call.arguments, "end_line")?;
                serde_json::to_value(self.workspace.read(path, start, end).map_err(tool_error)?)
            }
            SNAPSHOT_LIST_FILES_TOOL => {
                let prefix = optional_string(&call.arguments, "path")?;
                let maximum = optional_usize(&call.arguments, "max_results")?;
                serde_json::to_value(self.workspace.list(prefix, maximum).map_err(tool_error)?)
            }
            SNAPSHOT_SEARCH_TOOL => {
                let query = required_string(&call.arguments, "query")?;
                let prefix = optional_string(&call.arguments, "path")?;
                let maximum = optional_usize(&call.arguments, "max_results")?;
                serde_json::to_value(
                    self.workspace
                        .search(query, prefix, maximum)
                        .map_err(tool_error)?,
                )
            }
            SNAPSHOT_READ_UNSAVED_TOOL => {
                let entry_id = ArtifactId::parse(required_string(&call.arguments, "entry_id")?)
                    .map_err(|error| AgentToolError::new(error.to_string()))?;
                serde_json::to_value(self.workspace.read_unsaved(&entry_id).map_err(tool_error)?)
            }
            SNAPSHOT_SEARCH_SYMBOLS_TOOL => {
                let query = required_string(&call.arguments, "query")?;
                let maximum = optional_usize(&call.arguments, "max_results")?
                    .unwrap_or(50)
                    .min(200);
                let adapter = self.symbols.as_ref().ok_or_else(|| {
                    AgentToolError::new("this manifest has no captured symbol index")
                })?;
                serde_json::to_value(
                    adapter
                        .search(query, maximum)
                        .map_err(AgentToolError::new)?,
                )
            }
            SNAPSHOT_READ_DIAGNOSTICS_TOOL => {
                let prefix = optional_string(&call.arguments, "path")?
                    .map(RepoPath::parse)
                    .transpose()
                    .map_err(|error| AgentToolError::new(error.to_string()))?;
                let maximum = optional_usize(&call.arguments, "max_results")?
                    .unwrap_or(100)
                    .min(500);
                let adapter = self.diagnostics.as_ref().ok_or_else(|| {
                    AgentToolError::new("this manifest has no captured diagnostic index")
                })?;
                serde_json::to_value(
                    adapter
                        .diagnostics(prefix.as_ref(), maximum)
                        .map_err(AgentToolError::new)?,
                )
            }
            _ => {
                return Err(AgentToolError::new(
                    "tool is not a snapshot-native read tool",
                ))
            }
        }
        .map_err(|error| AgentToolError::new(error.to_string()))?;
        Ok(AgentToolResult::completed(Some(result)))
    }
}

fn strict_schema(schema: Value) -> StrictJsonSchema {
    StrictJsonSchema::new(schema).expect("snapshot tool schemas are static and strict")
}

impl AgentToolExecutor for SnapshotToolExecutor {
    fn execute(
        &self,
        call: AgentToolCall,
    ) -> AgentFuture<'_, Result<AgentToolResult, AgentToolError>> {
        Box::pin(async move { self.execute_sync(call) })
    }
}

/// Input factory for read-only children whose manifest projection has already
/// been registered by the editor-aware dispatch wiring.
pub struct SnapshotAgentLoopInputFactory {
    manager: AgentWorkspaceManager,
    provider: Arc<dyn AgentProviderAdapter>,
    approval_broker: Option<Arc<AgentApprovalBroker>>,
    envelopes: Mutex<HashMap<ManifestId, PreparedDelegation>>,
    tool_extension: Mutex<Option<Arc<dyn AgentToolExtension>>>,
}

#[derive(Clone)]
struct PreparedDelegation {
    envelope: DelegationEnvelope,
    budget: Option<super::AgentLoopBudget>,
}

impl SnapshotAgentLoopInputFactory {
    pub fn new(manager: AgentWorkspaceManager, provider: Arc<dyn AgentProviderAdapter>) -> Self {
        Self {
            manager,
            provider,
            approval_broker: None,
            envelopes: Mutex::new(HashMap::new()),
            tool_extension: Mutex::new(None),
        }
    }

    pub fn with_approval_broker(
        manager: AgentWorkspaceManager,
        provider: Arc<dyn AgentProviderAdapter>,
        approval_broker: Arc<AgentApprovalBroker>,
    ) -> Self {
        Self {
            manager,
            provider,
            approval_broker: Some(approval_broker),
            envelopes: Mutex::new(HashMap::new()),
            tool_extension: Mutex::new(None),
        }
    }

    /// Install the run-owned coordination surface before any child starts.
    /// Replacing it after provider sessions exist would change authority under
    /// an active turn, so duplicate installation fails closed.
    pub fn set_tool_extension(&self, extension: Arc<dyn AgentToolExtension>) -> Result<(), String> {
        let mut slot = self
            .tool_extension
            .lock()
            .map_err(|_| "agent tool extension registry is poisoned".to_string())?;
        if slot.is_some() {
            return Err("agent tool extension is already installed".into());
        }
        *slot = Some(extension);
        Ok(())
    }

    /// Bind the complete typed delegation contract before queue execution can
    /// start. Manifest identities are one-per-dispatch, so replacing a value
    /// would make the immutable context ambiguous and is rejected.
    pub fn register_envelope(
        &self,
        manifest_id: ManifestId,
        envelope: DelegationEnvelope,
    ) -> Result<(), String> {
        self.register_prepared(manifest_id, envelope, None)
    }

    pub fn register_prepared(
        &self,
        manifest_id: ManifestId,
        envelope: DelegationEnvelope,
        budget: Option<super::AgentLoopBudget>,
    ) -> Result<(), String> {
        let mut envelopes = self
            .envelopes
            .lock()
            .map_err(|_| "delegation envelope registry is poisoned".to_string())?;
        if envelopes.contains_key(&manifest_id) {
            return Err(format!(
                "delegation envelope for manifest {manifest_id} is already registered"
            ));
        }
        envelopes.insert(manifest_id, PreparedDelegation { envelope, budget });
        Ok(())
    }
}

impl AgentLoopInputFactory for SnapshotAgentLoopInputFactory {
    fn build(
        &self,
        dispatch: &super::AgentDispatchRecord,
    ) -> Result<AgentLoopDependencies, String> {
        let manifest_id = match &dispatch.handle.workspace.strategy {
            WorkspaceStrategy::ReadOnlySnapshot {
                manifest_id: Some(manifest_id),
            } => manifest_id,
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None } => {
                return Err("read-only dispatch does not identify a captured manifest".into());
            }
            _ => return Err("snapshot input factory requires a read-only workspace".into()),
        };
        let workspace = self
            .manager
            .read_only(manifest_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("captured manifest {manifest_id} is not registered"))?;
        let warnings = workspace.warnings().to_vec();
        let adapters = self
            .manager
            .snapshot_adapters(manifest_id)
            .map_err(|error| error.to_string())?;
        let prepared = self
            .envelopes
            .lock()
            .map_err(|_| "delegation envelope registry is poisoned".to_string())?
            .get(manifest_id)
            .cloned()
            .ok_or_else(|| format!("captured manifest {manifest_id} has no delegation envelope"))?;
        let mut envelope = prepared.envelope;
        envelope.workspace_warnings = warnings.clone();
        let executor = SnapshotToolExecutor::with_adapters(workspace, adapters);
        let mut tools = executor.scoped_view_with_adapters().tools();
        let extension = self
            .tool_extension
            .lock()
            .map_err(|_| "agent tool extension registry is poisoned".to_string())?
            .clone();
        let extension_names = if let Some(extension) = &extension {
            let extra = extension.scoped_tools(dispatch)?;
            let names = extra.iter().map(|tool| tool.name.clone()).collect();
            tools.extend(extra);
            names
        } else {
            BTreeSet::new()
        };
        let tool_view = ScopedToolView::new(tools).map_err(|error| error.to_string())?;
        let workspace = AgentWorkspaceDescriptor {
            assignment: dispatch.handle.workspace.clone(),
            root: None,
            read_only: true,
            warnings: warnings.clone(),
        };
        let approval_client = if let Some(broker) = &self.approval_broker {
            let effective = dispatch.role.capabilities.clone();
            let mut ceiling = AgentCapabilityCeiling::uniform(effective);
            ceiling.profile_allowlist = tool_view
                .tools()
                .into_iter()
                .map(|tool| tool.required_capability)
                .collect();
            ceiling.workspace_policy =
                BTreeSet::from([AgentCapability::Read, AgentCapability::Navigate]);
            broker
                .scoped_client(
                    AgentApprovalContext {
                        handle: dispatch.handle.clone(),
                        task_name: dispatch.task_name.clone(),
                        ancestry: dispatch.parent_agent_id.clone().into_iter().collect(),
                        role: dispatch.role.name.to_string(),
                        model: dispatch.resolved_route.model.clone(),
                        reasoning_effort: dispatch.resolved_route.reasoning_effort.as_str().into(),
                        workspace: workspace.clone(),
                    },
                    ceiling,
                )
                .map_err(|error| error.to_string())?
        } else {
            Arc::new(DenyAllAgentApprovals)
        };
        Ok(AgentLoopDependencies {
            provider: self.provider.clone(),
            tool_view,
            tool_executor: match extension {
                Some(extension) => Arc::new(ExtendedSnapshotToolExecutor {
                    snapshot: executor,
                    extension,
                    extension_names,
                }),
                None => Arc::new(executor),
            },
            approval_client,
            workspace,
            envelope,
            budget: prepared.budget,
        })
    }
}

fn enumerate_git_tree(
    repository: &Repository,
    commit_id: &str,
    files: &mut BTreeMap<RepoPath, SnapshotFile>,
    warnings: &mut Vec<AgentWorkspaceWarning>,
) -> Result<(), AgentWorkspaceError> {
    let oid =
        Oid::from_str(commit_id).map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
    let commit = repository
        .find_commit(oid)
        .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
    let tree = commit
        .tree()
        .map_err(|error| AgentWorkspaceError::Git(error.to_string()))?;
    let mut walk_error = None;
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        let Some(name) = entry.name() else {
            warnings.push(AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::UnsupportedGitEntry,
                path: None,
                artifact_id: None,
                detail: "Git tree entry has a non-UTF-8 name".into(),
            });
            return TreeWalkResult::Skip;
        };
        let text = format!("{root}{name}");
        let path = match RepoPath::parse(text.clone()) {
            Ok(path) => path,
            Err(error) => {
                warnings.push(AgentWorkspaceWarning {
                    kind: AgentWorkspaceWarningKind::UnsupportedGitEntry,
                    path: Some(text),
                    artifact_id: None,
                    detail: error.to_string(),
                });
                return TreeWalkResult::Skip;
            }
        };
        let mode = entry.filemode() as u32;
        let file_kind = match entry.kind() {
            Some(ObjectType::Tree) | Some(ObjectType::Commit) => FileKind::Directory,
            Some(ObjectType::Blob) if mode & 0o170000 == 0o120000 => FileKind::Symlink,
            Some(ObjectType::Blob) => FileKind::Regular,
            _ => {
                warnings.push(AgentWorkspaceWarning {
                    kind: AgentWorkspaceWarningKind::UnsupportedGitEntry,
                    path: Some(path.to_string()),
                    artifact_id: None,
                    detail: "Git tree entry has an unsupported object kind".into(),
                });
                return TreeWalkResult::Ok;
            }
        };
        let content = if file_kind == FileKind::Directory {
            SnapshotContent::Directory
        } else {
            match repository.find_blob(entry.id()) {
                Ok(blob) => SnapshotContent::GitBlob {
                    object_id: entry.id().to_string(),
                    byte_len: blob.size() as u64,
                },
                Err(error) => {
                    warnings.push(AgentWorkspaceWarning {
                        kind: AgentWorkspaceWarningKind::MissingGitObject,
                        path: Some(path.to_string()),
                        artifact_id: None,
                        detail: error.to_string(),
                    });
                    SnapshotContent::Unavailable {
                        detail: "recorded Git object is unavailable".into(),
                    }
                }
            }
        };
        files.insert(
            path,
            SnapshotFile {
                file_kind,
                executable: mode & 0o111 != 0,
                content,
            },
        );
        TreeWalkResult::Ok
    })
    .unwrap_or_else(|error| walk_error = Some(error.to_string()));
    if let Some(error) = walk_error {
        return Err(AgentWorkspaceError::Git(error));
    }
    Ok(())
}

fn git_blob_content(
    repository: &Repository,
    object_id: &str,
    path: Option<&RepoPath>,
    warnings: &mut Vec<AgentWorkspaceWarning>,
) -> SnapshotContent {
    let resolved = Oid::from_str(object_id)
        .map_err(|error| error.to_string())
        .and_then(|oid| repository.find_blob(oid).map_err(|error| error.to_string()));
    match resolved {
        Ok(blob) => SnapshotContent::GitBlob {
            object_id: object_id.into(),
            byte_len: blob.size() as u64,
        },
        Err(detail) => {
            warnings.push(AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::MissingGitObject,
                path: path.map(ToString::to_string),
                artifact_id: None,
                detail: detail.clone(),
            });
            SnapshotContent::Unavailable { detail }
        }
    }
}

fn resolve_manifest_layer(
    layer: &ManifestLayer,
    inherited: SnapshotContent,
    artifacts: &HashMap<ArtifactId, &crate::run_log::ArtifactRecord>,
    store: &ArtifactStore,
    path: Option<&RepoPath>,
    warnings: &mut Vec<AgentWorkspaceWarning>,
) -> SnapshotContent {
    match layer {
        ManifestLayer::Inherit => inherited,
        ManifestLayer::Deleted => SnapshotContent::Absent,
        ManifestLayer::Artifact { artifact_id } => {
            artifact_content(artifact_id, artifacts, store, path, warnings)
        }
    }
}

fn artifact_content(
    artifact_id: &ArtifactId,
    artifacts: &HashMap<ArtifactId, &crate::run_log::ArtifactRecord>,
    store: &ArtifactStore,
    path: Option<&RepoPath>,
    warnings: &mut Vec<AgentWorkspaceWarning>,
) -> SnapshotContent {
    let Some(record) = artifacts.get(artifact_id) else {
        let detail = "manifest references artifact metadata that is absent".to_string();
        warnings.push(AgentWorkspaceWarning {
            kind: AgentWorkspaceWarningKind::MissingArtifact,
            path: path.map(ToString::to_string),
            artifact_id: Some(artifact_id.clone()),
            detail: detail.clone(),
        });
        return SnapshotContent::Unavailable { detail };
    };
    match &record.state {
        ArtifactState::Available { blob_id, byte_len } => match store.contains(*blob_id) {
            Ok(true) => SnapshotContent::Artifact {
                blob_id: *blob_id,
                byte_len: *byte_len,
            },
            Ok(false) => {
                let detail = "content-addressed artifact blob is missing".to_string();
                warnings.push(AgentWorkspaceWarning {
                    kind: AgentWorkspaceWarningKind::MissingArtifact,
                    path: path.map(ToString::to_string),
                    artifact_id: Some(artifact_id.clone()),
                    detail: detail.clone(),
                });
                SnapshotContent::Unavailable { detail }
            }
            Err(error) => {
                let detail = error.to_string();
                warnings.push(AgentWorkspaceWarning {
                    kind: AgentWorkspaceWarningKind::MissingArtifact,
                    path: path.map(ToString::to_string),
                    artifact_id: Some(artifact_id.clone()),
                    detail: detail.clone(),
                });
                SnapshotContent::Unavailable { detail }
            }
        },
        ArtifactState::Missing { reason } => {
            warnings.push(AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::MissingArtifact,
                path: path.map(ToString::to_string),
                artifact_id: Some(artifact_id.clone()),
                detail: reason.clone(),
            });
            SnapshotContent::Unavailable {
                detail: reason.clone(),
            }
        }
        ArtifactState::Excluded { reason } => {
            warnings.push(AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::ExcludedArtifact,
                path: path.map(ToString::to_string),
                artifact_id: Some(artifact_id.clone()),
                detail: reason.clone(),
            });
            SnapshotContent::Unavailable {
                detail: reason.clone(),
            }
        }
        ArtifactState::Redacted { reason, .. } => {
            warnings.push(AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::RedactedArtifact,
                path: path.map(ToString::to_string),
                artifact_id: Some(artifact_id.clone()),
                detail: reason.clone(),
            });
            SnapshotContent::Unavailable {
                detail: reason.clone(),
            }
        }
    }
}

fn manifest_warnings(manifest: &BaseManifest) -> Vec<AgentWorkspaceWarning> {
    let mut warnings = Vec::new();
    if manifest.confidence == ManifestConfidence::Partial {
        warnings.push(AgentWorkspaceWarning {
            kind: AgentWorkspaceWarningKind::PartialCapture,
            path: None,
            artifact_id: None,
            detail: "base manifest was captured partially".into(),
        });
    }
    warnings.extend(manifest.issues.iter().map(|issue| AgentWorkspaceWarning {
        kind: AgentWorkspaceWarningKind::CaptureIssue,
        path: issue.path.as_ref().map(ToString::to_string),
        artifact_id: None,
        detail: issue.detail.clone(),
    }));
    warnings
}

fn exclude_sensitive_git_paths(
    files: &mut BTreeMap<RepoPath, SnapshotFile>,
    warnings: &mut Vec<AgentWorkspaceWarning>,
) {
    let excluded: Vec<_> = files
        .keys()
        .filter_map(|path| {
            sensitive_path_reason(Path::new(path.as_str()))
                .map(|reason| (path.clone(), reason.to_string()))
        })
        .collect();
    for (path, detail) in excluded {
        files.remove(&path);
        warnings.push(AgentWorkspaceWarning {
            kind: AgentWorkspaceWarningKind::SensitivePathExcluded,
            path: Some(path.to_string()),
            artifact_id: None,
            detail,
        });
    }
}

fn add_derived_directories(files: &mut BTreeMap<RepoPath, SnapshotFile>) {
    let mut directories = BTreeSet::new();
    for path in files.keys() {
        let mut parts: Vec<_> = path.as_str().split('/').collect();
        parts.pop();
        while !parts.is_empty() {
            directories.insert(parts.join("/"));
            parts.pop();
        }
    }
    for directory in directories {
        let path = RepoPath::parse(directory).expect("derived repository path is normalized");
        files.entry(path).or_insert(SnapshotFile {
            file_kind: FileKind::Directory,
            executable: false,
            content: SnapshotContent::Directory,
        });
    }
}

fn deduplicate_warnings(warnings: Vec<AgentWorkspaceWarning>) -> Vec<AgentWorkspaceWarning> {
    let mut seen = BTreeSet::new();
    warnings
        .into_iter()
        .filter(|warning| {
            seen.insert((
                warning.kind.clone(),
                warning.path.clone(),
                warning.artifact_id.clone(),
                warning.detail.clone(),
            ))
        })
        .collect()
}

fn bound_warnings(
    mut warnings: Vec<AgentWorkspaceWarning>,
    maximum: usize,
) -> Vec<AgentWorkspaceWarning> {
    if warnings.len() <= maximum {
        return warnings;
    }
    let retained = maximum.saturating_sub(1);
    let omitted = warnings.len().saturating_sub(retained);
    warnings.truncate(retained);
    if maximum > 0 {
        warnings.push(AgentWorkspaceWarning {
            kind: AgentWorkspaceWarningKind::WarningsOmitted,
            path: None,
            artifact_id: None,
            detail: format!("{omitted} additional workspace capture warnings were omitted"),
        });
    }
    warnings
}

fn checked_repo_path(value: &str) -> Result<RepoPath, AgentWorkspaceError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AgentWorkspaceError::InvalidPath(value.into()));
    }
    RepoPath::parse(value).map_err(|_| AgentWorkspaceError::InvalidPath(value.into()))
}

fn is_descendant(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, AgentToolError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentToolError::new(format!("'{key}' must be a non-empty string")))
}

fn optional_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, AgentToolError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(AgentToolError::new(format!("'{key}' must be a string"))),
    }
}

fn optional_usize(arguments: &Value, key: &str) -> Result<Option<usize>, AgentToolError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| AgentToolError::new(format!("'{key}' must be a non-negative integer"))),
    }
}

fn tool_error(error: AgentWorkspaceError) -> AgentToolError {
    AgentToolError::new(error.to_string())
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.into();
    }
    let mut end = maximum.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentWorkspaceError {
    DuplicateManifest(ManifestId),
    InvalidPath(String),
    SymlinkTraversal(String),
    NotFound(String),
    NotAFile(String),
    NotADirectory(String),
    UnsavedBufferNotFound(ArtifactId),
    Unavailable(String),
    ContentTooLarge { actual: u64, maximum: u64 },
    InvalidArguments(String),
    Git(String),
    Artifact(String),
    Internal(String),
}

impl fmt::Display for AgentWorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateManifest(manifest_id) => {
                write!(formatter, "manifest {manifest_id} is already registered")
            }
            Self::InvalidPath(path) => {
                write!(formatter, "invalid repository-relative path: {path:?}")
            }
            Self::SymlinkTraversal(path) => {
                write!(formatter, "refusing to traverse a snapshot symlink: {path}")
            }
            Self::NotFound(path) => write!(formatter, "snapshot path was not found: {path}"),
            Self::NotAFile(path) => write!(formatter, "snapshot path is not a file: {path}"),
            Self::NotADirectory(path) => {
                write!(formatter, "snapshot path is not a directory: {path}")
            }
            Self::UnsavedBufferNotFound(entry_id) => {
                write!(formatter, "unsaved buffer handle was not found: {entry_id}")
            }
            Self::Unavailable(detail) => {
                write!(formatter, "snapshot content is unavailable: {detail}")
            }
            Self::ContentTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "snapshot content is {actual} bytes, exceeding the {maximum} byte tool limit"
                )
            }
            Self::InvalidArguments(detail) => {
                write!(formatter, "invalid snapshot tool arguments: {detail}")
            }
            Self::Git(detail) => {
                write!(formatter, "could not resolve recorded Git state: {detail}")
            }
            Self::Artifact(detail) => {
                write!(formatter, "could not resolve recorded artifact: {detail}")
            }
            Self::Internal(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for AgentWorkspaceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{
        fake_provider::FakeProviderAdapter, AgentCapability, AgentDispatchRecord, AgentKindName,
        AgentRoleTemplate, AgentSupervisor, AgentSupervisorConfig, DispatchHandle, DispatchRequest,
        DispatchState, ModelFallbackPolicy, ModelRouteResolution, ReasoningEffort,
        RequestedModelRoute, ResolvedModelRoute, SubagentModelCatalog, WorkspaceAssignment,
    };
    use crate::ai::AiConfig;
    use crate::run_log::{
        capture_base_manifest, discover_git_manifest, ArtifactSource, BaseManifestId,
        CaptureDecision, CaptureLimits, CapturePolicy, CaptureSubject, ContentRequest,
        EditorOverlayInput, EventKind, GitManifestMetadata, GitSnapshotContentReader,
        InMemoryRunEventSink, MailboxNotificationKind, RepositoryId, RunEventSink, RunId,
        SnapshotContentReader, UnsavedBufferInput, WorkspaceId,
    };
    use git2::{IndexAddOption, Repository, Signature};
    use std::fs;

    struct IncludeAll;

    impl CapturePolicy for IncludeAll {
        fn decide(&self, _subject: &CaptureSubject<'_>) -> CaptureDecision {
            CaptureDecision::Include
        }
    }

    struct OverlayReader {
        git: GitSnapshotContentReader,
        overlays: HashMap<String, Vec<u8>>,
    }

    impl SnapshotContentReader for OverlayReader {
        fn read(&self, request: &ContentRequest, max_bytes: u64) -> Result<Vec<u8>, String> {
            if let Some(bytes) = self.overlays.get(&request.locator) {
                if bytes.len() as u64 > max_bytes {
                    return Err("test overlay exceeds read bound".into());
                }
                return Ok(bytes.clone());
            }
            self.git.read(request, max_bytes)
        }
    }

    fn commit_all(repository: &Repository, message: &str) {
        let mut index = repository.index().unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = Signature::now("ovim", "ovim@example.invalid").unwrap();
        let parents = repository
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parent_refs,
            )
            .unwrap();
    }

    fn stage(repository: &Repository, path: &str) {
        let mut index = repository.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }

    fn manifest_with_overlays(
        repository: &Repository,
        store: &ArtifactStore,
        limits: CaptureLimits,
        editor_overlay: Option<(&str, &[u8])>,
        unsaved: Option<&[u8]>,
    ) -> BaseManifest {
        let mut snapshot = discover_git_manifest(
            repository.workdir().unwrap(),
            GitManifestMetadata {
                repository_id: RepositoryId::parse("repo_agent_workspace").unwrap(),
                base_manifest_id: BaseManifestId::parse("bsm_agent_workspace").unwrap(),
                captured_at: "2026-07-18T10:00:00Z".into(),
            },
        )
        .unwrap();
        let mut overlays = HashMap::new();
        if let Some((path, bytes)) = editor_overlay {
            overlays.insert("editor:test".into(), bytes.to_vec());
            snapshot
                .attach_editor_overlay(
                    path,
                    EditorOverlayInput {
                        content: ContentRequest {
                            locator: "editor:test".into(),
                            declared_bytes: Some(bytes.len() as u64),
                        },
                        version: Some(7),
                        modified: true,
                        encoding: Some("UTF-8".into()),
                        line_endings: Some("LF".into()),
                    },
                )
                .unwrap();
        }
        if let Some(bytes) = unsaved {
            overlays.insert("editor:unsaved".into(), bytes.to_vec());
            snapshot.add_unsaved_buffer(UnsavedBufferInput {
                ephemeral_buffer_id: Some("buffer-99".into()),
                display_name: Some("draft".into()),
                content: ContentRequest {
                    locator: "editor:unsaved".into(),
                    declared_bytes: Some(bytes.len() as u64),
                },
                version: Some(3),
                modified: true,
                encoding: Some("UTF-8".into()),
                line_endings: Some("LF".into()),
            });
        }
        let reader = OverlayReader {
            git: snapshot.reader,
            overlays,
        };
        capture_base_manifest(snapshot.input, store, &IncludeAll, &reader, limits)
    }

    struct Fixture {
        directory: tempfile::TempDir,
        repository: Repository,
        store: ArtifactStore,
        manifest_id: ManifestId,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let repository = Repository::init(directory.path()).unwrap();
            fs::create_dir_all(directory.path().join("src")).unwrap();
            fs::write(directory.path().join("clean.txt"), b"clean base\n").unwrap();
            fs::write(directory.path().join("staged.txt"), b"staged base\n").unwrap();
            fs::write(directory.path().join("layered.txt"), b"layered base\n").unwrap();
            fs::write(directory.path().join("editor.txt"), b"editor base\n").unwrap();
            fs::write(directory.path().join("deleted.txt"), b"delete me\n").unwrap();
            fs::write(directory.path().join("src/lib.rs"), b"pub fn base() {}\n").unwrap();
            fs::write(directory.path().join("exec.sh"), b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::{symlink, PermissionsExt};
                fs::set_permissions(
                    directory.path().join("exec.sh"),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                symlink("/tmp/outside", directory.path().join("escape")).unwrap();
            }
            fs::write(directory.path().join(".env"), b"TOKEN=secret\n").unwrap();
            commit_all(&repository, "base");
            let artifacts = tempfile::tempdir().unwrap().keep();
            let store = ArtifactStore::open(artifacts).unwrap();
            Self {
                directory,
                repository,
                store,
                manifest_id: ManifestId::parse("mft_agent_workspace").unwrap(),
            }
        }

        fn projection(&self, manifest: BaseManifest) -> Arc<ReadOnlyAgentWorkspace> {
            AgentWorkspaceManager::new(self.store.clone())
                .register_read_only(self.manifest_id.clone(), manifest, self.directory.path())
                .unwrap()
        }
    }

    #[test]
    fn projects_git_index_worktree_untracked_editor_deletion_and_metadata() {
        let fixture = Fixture::new();
        fs::write(
            fixture.directory.path().join("staged.txt"),
            b"staged view\n",
        )
        .unwrap();
        stage(&fixture.repository, "staged.txt");
        fs::write(
            fixture.directory.path().join("layered.txt"),
            b"index view\n",
        )
        .unwrap();
        stage(&fixture.repository, "layered.txt");
        fs::write(
            fixture.directory.path().join("layered.txt"),
            b"worktree view\n",
        )
        .unwrap();
        fs::write(
            fixture.directory.path().join("untracked.txt"),
            b"untracked view\n",
        )
        .unwrap();
        fs::remove_file(fixture.directory.path().join("deleted.txt")).unwrap();
        let manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits::default(),
            Some(("editor.txt", b"editor view\n")),
            Some(b"unsaved draft\n"),
        );
        let workspace = fixture.projection(manifest);

        assert_eq!(
            workspace.read("clean.txt", None, None).unwrap().text,
            Some("clean base".into())
        );
        assert_eq!(
            workspace.read("staged.txt", None, None).unwrap().text,
            Some("staged view".into())
        );
        assert_eq!(
            workspace.read("layered.txt", None, None).unwrap().text,
            Some("worktree view".into())
        );
        assert_eq!(
            workspace.read("editor.txt", None, None).unwrap().text,
            Some("editor view".into())
        );
        assert_eq!(
            workspace.read("untracked.txt", None, None).unwrap().text,
            Some("untracked view".into())
        );
        assert!(matches!(
            workspace.read("deleted.txt", None, None),
            Err(AgentWorkspaceError::NotFound(_))
        ));
        let unsaved = workspace.unsaved_buffers();
        assert_eq!(unsaved.len(), 1);
        assert_eq!(
            workspace.read_unsaved(&unsaved[0].entry_id).unwrap().text,
            Some("unsaved draft\n".into())
        );
        #[cfg(unix)]
        assert!(workspace.read("exec.sh", None, None).unwrap().executable);
        assert!(!workspace
            .list(None, None)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.path.as_str() == ".env"));
        assert!(workspace.warnings().iter().any(|warning| {
            warning.kind == AgentWorkspaceWarningKind::SensitivePathExcluded
                && warning.path.as_deref() == Some(".env")
        }));
    }

    #[test]
    fn projection_remains_stable_after_worktree_and_head_change() {
        let fixture = Fixture::new();
        fs::write(
            fixture.directory.path().join("layered.txt"),
            b"captured dirty\n",
        )
        .unwrap();
        let manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits::default(),
            None,
            None,
        );
        let workspace = fixture.projection(manifest);

        fs::write(fixture.directory.path().join("clean.txt"), b"later clean\n").unwrap();
        fs::write(
            fixture.directory.path().join("layered.txt"),
            b"later dirty\n",
        )
        .unwrap();
        commit_all(&fixture.repository, "later head");

        assert_eq!(
            workspace.read("clean.txt", None, None).unwrap().text,
            Some("clean base".into())
        );
        assert_eq!(
            workspace.read("layered.txt", None, None).unwrap().text,
            Some("captured dirty".into())
        );
    }

    #[test]
    fn rejects_absolute_traversal_and_symlink_escape_paths() {
        let fixture = Fixture::new();
        let workspace = fixture.projection(manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits::default(),
            None,
            None,
        ));

        for path in [
            "/etc/passwd",
            "../outside",
            "src/../../outside",
            "src\\lib.rs",
        ] {
            assert!(matches!(
                workspace.read(path, None, None),
                Err(AgentWorkspaceError::InvalidPath(_))
            ));
        }
        #[cfg(unix)]
        assert!(matches!(
            workspace.read("escape/secret", None, None),
            Err(AgentWorkspaceError::SymlinkTraversal(_))
        ));
    }

    #[test]
    fn binary_oversized_and_missing_artifacts_are_explicit() {
        let fixture = Fixture::new();
        fs::write(
            fixture.directory.path().join("binary.bin"),
            [0, 159, 146, 150],
        )
        .unwrap();
        fs::write(
            fixture.directory.path().join("large.txt"),
            b"too large for capture",
        )
        .unwrap();
        fs::write(
            fixture.directory.path().join("missing.txt"),
            b"missing blob",
        )
        .unwrap();
        let mut manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits {
                max_file_bytes: 12,
                max_total_bytes: 100,
            },
            None,
            None,
        );
        let missing_id = manifest
            .artifacts
            .iter_mut()
            .find_map(|record| match &record.source {
                ArtifactSource::Workspace { path, .. } if path == "missing.txt" => {
                    record.state = ArtifactState::Missing {
                        reason: "simulated missing artifact".into(),
                    };
                    Some(record.artifact_id.clone())
                }
                _ => None,
            })
            .unwrap();
        let workspace = fixture.projection(manifest);

        assert!(workspace.read("binary.bin", None, None).unwrap().binary);
        assert!(matches!(
            workspace.read("large.txt", None, None),
            Err(AgentWorkspaceError::Unavailable(_))
        ));
        assert!(matches!(
            workspace.read("missing.txt", None, None),
            Err(AgentWorkspaceError::Unavailable(_))
        ));
        assert!(workspace.warnings().iter().any(|warning| {
            warning.kind == AgentWorkspaceWarningKind::ExcludedArtifact
                && warning.path.as_deref() == Some("large.txt")
        }));
        assert!(workspace.warnings().iter().any(|warning| {
            warning.kind == AgentWorkspaceWarningKind::MissingArtifact
                && warning.artifact_id.as_ref() == Some(&missing_id)
        }));
        let search = workspace.search("anything", None, None).unwrap();
        assert_eq!(search.skipped_binary, 1);
        assert!(search.skipped_unavailable >= 2);
    }

    #[test]
    fn list_and_search_enforce_deterministic_bounds() {
        let fixture = Fixture::new();
        for index in 0..6 {
            fs::write(
                fixture.directory.path().join(format!("match-{index}.txt")),
                format!("needle {index}\nneedle again\n"),
            )
            .unwrap();
        }
        let manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits::default(),
            None,
            None,
        );
        let manager = AgentWorkspaceManager::with_limits(
            fixture.store.clone(),
            AgentWorkspaceLimits {
                max_list_results: 3,
                max_search_results: 2,
                ..AgentWorkspaceLimits::default()
            },
        );
        let workspace = manager
            .register_read_only(
                fixture.manifest_id.clone(),
                manifest,
                fixture.directory.path(),
            )
            .unwrap();

        let listed = workspace.list(None, Some(99)).unwrap();
        assert_eq!(listed.entries.len(), 3);
        assert!(listed.truncated);
        let searched = workspace.search("needle", None, Some(99)).unwrap();
        assert_eq!(searched.matches.len(), 2);
        assert!(searched.truncated);
        assert!(searched.matches.windows(2).all(|pair| {
            (pair[0].path.as_str(), pair[0].line) <= (pair[1].path.as_str(), pair[1].line)
        }));
    }

    #[tokio::test]
    async fn scoped_executor_and_factory_expose_only_snapshot_reads() {
        let fixture = Fixture::new();
        fs::write(
            fixture.directory.path().join("large.txt"),
            b"excluded content",
        )
        .unwrap();
        let manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits {
                max_file_bytes: 4,
                max_total_bytes: 100,
            },
            None,
            None,
        );
        let manager = AgentWorkspaceManager::new(fixture.store.clone());
        let workspace = manager
            .register_read_only(
                fixture.manifest_id.clone(),
                manifest,
                fixture.directory.path(),
            )
            .unwrap();
        let assignment = WorkspaceAssignment {
            workspace_id: WorkspaceId::parse("wsp_snapshot_tools").unwrap(),
            strategy: WorkspaceStrategy::ReadOnlySnapshot {
                manifest_id: Some(fixture.manifest_id.clone()),
            },
        };
        let handle = DispatchHandle {
            run_id: RunId::parse("run_snapshot_tools").unwrap(),
            agent_id: crate::run_log::AgentId::parse("agt_snapshot_tools").unwrap(),
            workspace: assignment.clone(),
        };
        let requested_route = RequestedModelRoute {
            catalog_model_id: "test/model".into(),
            reasoning_effort: ReasoningEffort::low(),
            fallback_policy: ModelFallbackPolicy::FailClosed,
        };
        let dispatch = AgentDispatchRecord {
            handle: handle.clone(),
            role: AgentRoleTemplate::built_in(AgentKindName::Explorer),
            requested_route,
            resolved_route: ResolvedModelRoute {
                catalog_generation: "test".into(),
                catalog_model_id: "test/model".into(),
                profile_name: "test".into(),
                provider: "fake".into(),
                model: "model".into(),
                reasoning_effort: ReasoningEffort::low(),
                resolution: ModelRouteResolution::Exact,
                fallback_reason: None,
            },
            task_name: "snapshot_tools".into(),
            objective: "inspect the snapshot".into(),
            parent_agent_id: None,
            causing_turn_id: None,
            turn_generation: 0,
            followup: None,
            state: DispatchState::Queued,
            queue_sequence: 1,
        };
        let factory = SnapshotAgentLoopInputFactory::new(
            manager,
            Arc::new(FakeProviderAdapter::new("delayed_completion")),
        );
        factory
            .register_envelope(
                fixture.manifest_id.clone(),
                DelegationEnvelope::objective("inspect the snapshot"),
            )
            .unwrap();
        let dependencies = factory.build(&dispatch).unwrap();
        let names = dependencies.tool_view.names();
        assert_eq!(
            names,
            vec![
                SNAPSHOT_LIST_FILES_TOOL,
                SNAPSHOT_READ_FILE_TOOL,
                SNAPSHOT_READ_UNSAVED_TOOL,
                SNAPSHOT_SEARCH_TOOL,
            ]
        );
        for forbidden in [
            "bash",
            "web_search",
            "spawn_agent",
            "write_file_at_path",
            "document_symbols",
            "read_diagnostics",
        ] {
            assert!(!names.iter().any(|name| name == forbidden));
        }
        assert_eq!(
            dependencies.workspace.warnings,
            dependencies.envelope.workspace_warnings
        );
        assert!(!dependencies.workspace.warnings.is_empty());
        assert!(dependencies.workspace.root.is_none());
        assert!(dependencies.workspace.read_only);

        let executor = SnapshotToolExecutor::new(workspace);
        let result = executor
            .execute(AgentToolCall {
                handle,
                causing_turn_id: Some(crate::run_log::TurnId::new()),
                caused_by_event: crate::run_log::EventId::new(),
                tool_call_id: "tool-1".into(),
                tool_name: SNAPSHOT_READ_FILE_TOOL.into(),
                arguments: serde_json::json!({"path": "clean.txt"}),
                workspace: dependencies.workspace,
            })
            .await
            .unwrap();
        assert_eq!(result.outcome, crate::run_log::ToolOutcome::Completed);
        assert_eq!(
            result.result.unwrap()["text"],
            Value::String("clean base".into())
        );
        assert!(!dispatch.role.capabilities.contains(&AgentCapability::Shell));
    }

    #[tokio::test]
    async fn supervisor_persists_workspace_warnings_into_handoff_and_mailbox() {
        let fixture = Fixture::new();
        fs::write(
            fixture.directory.path().join("excluded-large.txt"),
            b"excluded content",
        )
        .unwrap();
        let manifest = manifest_with_overlays(
            &fixture.repository,
            &fixture.store,
            CaptureLimits {
                max_file_bytes: 4,
                max_total_bytes: 100,
            },
            None,
            None,
        );
        let manager = AgentWorkspaceManager::new(fixture.store.clone());
        manager
            .register_read_only(
                fixture.manifest_id.clone(),
                manifest,
                fixture.directory.path(),
            )
            .unwrap();
        let sink = Arc::new(InMemoryRunEventSink::new());
        let run_id = RunId::parse("run_snapshot_warning_handoff").unwrap();
        let root_agent_id = crate::run_log::AgentId::parse("agt_snapshot_warning_root").unwrap();
        let catalog = Arc::new(SubagentModelCatalog::from_config(&AiConfig::default()).unwrap());
        let factory = Arc::new(SnapshotAgentLoopInputFactory::new(
            manager,
            Arc::new(FakeProviderAdapter::new("delayed_completion")),
        ));
        factory
            .register_envelope(
                fixture.manifest_id.clone(),
                DelegationEnvelope::objective("inspect the captured snapshot"),
            )
            .unwrap();
        let supervisor = AgentSupervisor::new(
            run_id.clone(),
            root_agent_id,
            sink.clone(),
            catalog,
            factory,
            AgentSupervisorConfig::default(),
        )
        .unwrap();
        supervisor
            .dispatch(DispatchRequest {
                task_name: "inspect_snapshot".into(),
                objective: "inspect the captured snapshot".into(),
                role: AgentRoleTemplate::built_in(AgentKindName::Explorer),
                requested_route: RequestedModelRoute::exact(
                    super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
                    ReasoningEffort::none(),
                ),
                parent_agent_id: None,
                causing_turn_id: None,
                caused_by_event: None,
                workspace: WorkspaceAssignment {
                    workspace_id: WorkspaceId::parse("wsp_snapshot_warning_handoff").unwrap(),
                    strategy: WorkspaceStrategy::ReadOnlySnapshot {
                        manifest_id: Some(fixture.manifest_id),
                    },
                },
            })
            .await
            .unwrap();
        assert!(supervisor
            .wait_for_idle(std::time::Duration::from_secs(2))
            .await
            .unwrap());

        let events = sink.events(&run_id).unwrap();
        let handoff_warnings = events.iter().find_map(|event| match &event.kind {
            EventKind::AgentHandoff(handoff) => Some(&handoff.workspace_warnings),
            _ => None,
        });
        assert!(handoff_warnings.is_some_and(|warnings| !warnings.is_empty()));
        let mailbox_warnings = events.iter().find_map(|event| match &event.kind {
            EventKind::MailboxNotification(notification) => match &notification.notification {
                MailboxNotificationKind::Handoff { handoff, .. } => {
                    Some(&handoff.workspace_warnings)
                }
                _ => None,
            },
            _ => None,
        });
        assert!(
            mailbox_warnings.is_some_and(|warnings| !warnings.is_empty()),
            "events: {events:#?}"
        );
    }

    #[test]
    fn captured_symbol_and_diagnostic_adapters_are_manifest_bound_copies() {
        let manifest_id = ManifestId::new();
        let mut source_symbols = vec![SnapshotSymbol {
            name: "CapturedThing".into(),
            kind: "struct".into(),
            path: RepoPath::parse("src/lib.rs").unwrap(),
            line: 7,
            column: 1,
        }];
        let mut source_diagnostics = vec![SnapshotDiagnostic {
            path: RepoPath::parse("src/lib.rs").unwrap(),
            line: 9,
            column: 3,
            severity: "warning".into(),
            message: "captured warning".into(),
        }];
        let symbols = CapturedSnapshotSymbolIndex::new(manifest_id.clone(), source_symbols.clone());
        let diagnostics =
            CapturedSnapshotDiagnostics::new(manifest_id.clone(), source_diagnostics.clone());
        source_symbols[0].name = "LiveMutation".into();
        source_diagnostics[0].message = "live mutation".into();

        assert_eq!(symbols.manifest_id(), &manifest_id);
        assert_eq!(
            symbols.search("captured", 10).unwrap()[0].name,
            "CapturedThing"
        );
        assert_eq!(
            diagnostics
                .diagnostics(Some(&RepoPath::parse("src").unwrap()), 10)
                .unwrap()[0]
                .message,
            "captured warning"
        );

        let directory = tempfile::tempdir().unwrap();
        let manager = AgentWorkspaceManager::new(ArtifactStore::open(directory.path()).unwrap());
        assert!(manager
            .register_snapshot_adapters(
                &ManifestId::new(),
                Arc::new(symbols),
                Arc::new(diagnostics),
            )
            .is_err());
    }
}
