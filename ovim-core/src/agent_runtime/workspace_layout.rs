//! Durable storage layout and ownership records for Ovim-managed workspaces.
//!
//! Workspace storage deliberately does not share the run-log directory. Run
//! history and a write worktree have different retention semantics: removing
//! history must never silently remove the only copy of unresolved agent work.

use super::DelegatedAgentWorkspacePolicy;
use crate::run_log::{AgentId, BaseManifestId, RepositoryId, RunId, WorkspaceId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub const OVIM_WORKSPACES_DIR_ENV: &str = "OVIM_WORKSPACES_DIR";
pub const WORKSPACE_MARKER_FILE: &str = "workspace.json";
pub const WORKSPACE_CHECKOUT_DIRECTORY: &str = "checkout";
pub const WORKSPACE_MARKER_VERSION: u32 = 1;
const WORKSPACE_MARKER_INTEGRITY: &str = "sha256";
const MEBIBYTE: u64 = 1024 * 1024;
const MAX_PATH_COMPONENT_BYTES: usize = 255;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// Why a workspace root was selected.
///
/// Only environment and configuration roots count as explicit overrides. A
/// repo-adjacent root is accepted only with one of those origins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceRootOrigin {
    Environment,
    Configuration,
    PlatformDefault,
}

impl WorkspaceRootOrigin {
    pub fn is_explicit(self) -> bool {
        matches!(self, Self::Environment | Self::Configuration)
    }
}

/// Unvalidated workspace root plus allocation policy.
///
/// Call [`Self::preflight`] before using the root for durable state. Keeping
/// this type distinct from [`ValidatedWorkspaceStorageLayout`] makes it hard
/// for later worktree code to accidentally skip canonicalization and Git-aware
/// containment checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceStorageLayout {
    root: PathBuf,
    origin: WorkspaceRootOrigin,
    minimum_free_space_bytes: u64,
}

impl WorkspaceStorageLayout {
    pub fn new(
        root: impl Into<PathBuf>,
        origin: WorkspaceRootOrigin,
        minimum_free_space_mb: u64,
    ) -> Self {
        Self {
            root: root.into(),
            origin,
            minimum_free_space_bytes: minimum_free_space_mb.saturating_mul(MEBIBYTE),
        }
    }

    /// Resolve production storage using environment, config, platform order.
    pub fn discover(config: &DelegatedAgentWorkspacePolicy) -> Result<Self, WorkspaceLayoutError> {
        Self::from_locations(
            std::env::var_os(OVIM_WORKSPACES_DIR_ENV),
            config,
            dirs::data_local_dir(),
        )
    }

    /// Injectable discovery that never reads or mutates process-global state.
    pub fn from_locations(
        environment_root: Option<OsString>,
        config: &DelegatedAgentWorkspacePolicy,
        data_local_dir: Option<PathBuf>,
    ) -> Result<Self, WorkspaceLayoutError> {
        if let Some(root) = environment_root.filter(|value| !value.is_empty()) {
            return Ok(Self::new(
                PathBuf::from(root),
                WorkspaceRootOrigin::Environment,
                config.minimum_free_space_mb,
            ));
        }
        if let Some(root) = config
            .root
            .as_ref()
            .filter(|root| !root.as_os_str().is_empty())
        {
            return Ok(Self::new(
                root,
                WorkspaceRootOrigin::Configuration,
                config.minimum_free_space_mb,
            ));
        }
        data_local_dir
            .map(|directory| {
                Self::new(
                    directory.join("ovim").join("workspaces"),
                    WorkspaceRootOrigin::PlatformDefault,
                    config.minimum_free_space_mb,
                )
            })
            .ok_or(WorkspaceLayoutError::PlatformDataUnavailable)
    }

    pub fn root_candidate(&self) -> &Path {
        &self.root
    }

    pub fn origin(&self) -> WorkspaceRootOrigin {
        self.origin
    }

    pub fn minimum_free_space_bytes(&self) -> u64 {
        self.minimum_free_space_bytes
    }

    pub fn candidate_paths(&self, coordinates: &WorkspaceCoordinates) -> WorkspacePaths {
        WorkspacePaths::under(&self.root, coordinates)
    }

    /// Validate and prepare the root for one exact future allocation.
    pub fn preflight(
        &self,
        source_repository: &Path,
        coordinates: &WorkspaceCoordinates,
    ) -> Result<WorkspacePreflight, WorkspaceLayoutError> {
        self.preflight_with(source_repository, coordinates, &NativeWorkspaceProbe)
    }

    /// Injectable preflight for deterministic disk/path/platform tests.
    pub fn preflight_with(
        &self,
        source_repository: &Path,
        coordinates: &WorkspaceCoordinates,
        probe: &dyn WorkspaceProbe,
    ) -> Result<WorkspacePreflight, WorkspaceLayoutError> {
        preflight_layout(self, source_repository, coordinates, probe)
    }
}

/// Identities that alone determine an Ovim workspace path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCoordinates {
    pub repository_id: RepositoryId,
    pub run_id: RunId,
    pub agent_id: AgentId,
}

impl WorkspaceCoordinates {
    pub fn new(repository_id: RepositoryId, run_id: RunId, agent_id: AgentId) -> Self {
        Self {
            repository_id,
            run_id,
            agent_id,
        }
    }
}

/// Exact derived locations. Display labels, task text and model output never
/// participate in this derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePaths {
    pub repository_directory: PathBuf,
    pub run_directory: PathBuf,
    pub agent_directory: PathBuf,
    pub marker: PathBuf,
    pub checkout: PathBuf,
}

impl WorkspacePaths {
    fn under(root: &Path, coordinates: &WorkspaceCoordinates) -> Self {
        let repository_directory = root.join(encoded_identity_component(
            "repo",
            coordinates.repository_id.as_str(),
        ));
        let run_directory = repository_directory.join(encoded_identity_component(
            "run",
            coordinates.run_id.as_str(),
        ));
        let agent_directory = run_directory.join(encoded_identity_component(
            "agent",
            coordinates.agent_id.as_str(),
        ));
        Self {
            marker: agent_directory.join(WORKSPACE_MARKER_FILE),
            checkout: agent_directory.join(WORKSPACE_CHECKOUT_DIRECTORY),
            repository_directory,
            run_directory,
            agent_directory,
        }
    }
}

fn encoded_identity_component(prefix: &str, value: &str) -> String {
    let mut encoded = String::with_capacity(prefix.len() + 1 + value.len() * 2);
    encoded.push_str(prefix);
    encoded.push('-');
    for byte in value.as_bytes() {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

/// Canonical, Git-aware layout proven safe for an exact identity tuple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedWorkspaceStorageLayout {
    root: PathBuf,
    origin: WorkspaceRootOrigin,
    minimum_free_space_bytes: u64,
}

impl ValidatedWorkspaceStorageLayout {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn origin(&self) -> WorkspaceRootOrigin {
        self.origin
    }

    pub fn minimum_free_space_bytes(&self) -> u64 {
        self.minimum_free_space_bytes
    }

    pub fn paths(&self, coordinates: &WorkspaceCoordinates) -> WorkspacePaths {
        WorkspacePaths::under(&self.root, coordinates)
    }

    /// Reserve an exact identity path and atomically publish its initial
    /// ownership marker. No Git worktree is created by this foundation API.
    pub fn initialize_marker(
        &self,
        marker: &WorkspaceOwnershipMarker,
    ) -> Result<WorkspacePaths, WorkspaceLayoutError> {
        marker.verify_integrity()?;
        if marker.ownership != WorkspaceOwnership::OvimOwned {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "ownership",
                detail: "only Ovim-owned workspaces can be initialized".into(),
            });
        }
        let paths = self.validate_marker_layout(marker)?;
        if paths.agent_directory.exists() {
            return Err(WorkspaceLayoutError::WorkspaceCollision {
                path: paths.agent_directory,
            });
        }

        ensure_private_directory(&paths.repository_directory)?;
        ensure_private_directory(&paths.run_directory)?;
        fs::create_dir(&paths.agent_directory).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                WorkspaceLayoutError::WorkspaceCollision {
                    path: paths.agent_directory.clone(),
                }
            } else {
                io_error(
                    "reserve workspace identity directory",
                    &paths.agent_directory,
                    source,
                )
            }
        })?;
        set_private_directory_permissions(&paths.agent_directory)?;
        // On failure the exact empty reservation stays visible to orphan
        // enumeration; cleanup is outside this non-destructive layer.
        write_marker_atomically(&paths.marker, marker)?;
        Ok(paths)
    }

    /// Read an exact derived marker and prove identity, canonical containment,
    /// source repository and current Git registry agreement.
    pub fn validate_exact_workspace(
        &self,
        source_repository: &Path,
        coordinates: &WorkspaceCoordinates,
        workspace_id: &WorkspaceId,
    ) -> Result<ExactWorkspaceValidation, WorkspaceLayoutError> {
        let git = inspect_git_workspace(source_repository)?;
        validate_root(
            &self.root,
            self.origin,
            &git.canonical_source,
            &git.canonical_common_directory,
            &git.registered_worktrees,
        )?;
        let canonical_root = fs::canonicalize(&self.root)
            .map_err(|source| io_error("re-canonicalize workspace root", &self.root, source))?;
        if canonical_root != self.root {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_root",
                detail: format!(
                    "validated root moved from {} to {}",
                    self.root.display(),
                    canonical_root.display()
                ),
            });
        }

        let paths = self.paths(coordinates);
        let marker = read_marker(&paths.marker)?;
        marker
            .verify_integrity()
            .map_err(|error| marker_error_at(error, &paths.marker))?;
        if &marker.workspace_id != workspace_id {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "workspace_id",
                detail: format!("expected {workspace_id}, found {}", marker.workspace_id),
            });
        }
        if marker.ownership != WorkspaceOwnership::OvimOwned {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "ownership",
                detail: "exact mutation validation requires an Ovim-owned marker".into(),
            });
        }
        if marker.repository_id != coordinates.repository_id
            || marker.run_id != coordinates.run_id
            || marker.agent_id != coordinates.agent_id
        {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "coordinates",
                detail: "repository/run/agent identities do not agree".into(),
            });
        }
        if marker.canonical_source != git.canonical_source {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_source",
                detail: format!(
                    "expected {}, found {}",
                    git.canonical_source.display(),
                    marker.canonical_source.display()
                ),
            });
        }
        self.validate_marker_layout(&marker)?;

        let actual_checkout = canonicalize_allow_missing(&paths.checkout).map_err(|source| {
            io_error("re-canonicalize exact checkout", &paths.checkout, source)
        })?;
        if actual_checkout != paths.checkout || actual_checkout != marker.canonical_checkout {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_checkout",
                detail: "checkout moved, escaped, or no longer matches its marker".into(),
            });
        }

        let registered_worktree = git
            .registered_worktrees
            .into_iter()
            .find(|worktree| worktree.path == paths.checkout);
        if let Some(registered) = registered_worktree.as_ref() {
            if registered.branch.as_deref() != Some(marker.git_ref.as_str()) {
                return Err(WorkspaceLayoutError::MarkerMismatch {
                    field: "git_ref",
                    detail: format!(
                        "marker records {:?}, Git reports {:?}",
                        marker.git_ref, registered.branch
                    ),
                });
            }
        }
        Ok(ExactWorkspaceValidation {
            marker,
            paths,
            registered_worktree,
        })
    }

    /// Lookup is exact-path only; branch names and directory patterns are not
    /// accepted as worktree identity.
    pub fn registered_worktree_at(
        &self,
        source_repository: &Path,
        exact_checkout: &Path,
    ) -> Result<Option<RegisteredGitWorktree>, WorkspaceLayoutError> {
        let exact_checkout = canonicalize_allow_missing(exact_checkout).map_err(|source| {
            io_error("canonicalize exact worktree lookup", exact_checkout, source)
        })?;
        let git = inspect_git_workspace(source_repository)?;
        Ok(git
            .registered_worktrees
            .into_iter()
            .find(|worktree| worktree.path == exact_checkout))
    }

    /// Surface unowned/malformed storage observations. This method cannot
    /// authorize or perform deletion and never treats a directory name as
    /// proof of identity.
    pub fn enumerate_orphans(
        &self,
        known_workspaces: &HashSet<WorkspaceId>,
    ) -> Result<Vec<WorkspaceOrphanObservation>, WorkspaceLayoutError> {
        let mut observations = Vec::new();
        if !self.root.exists() {
            return Ok(observations);
        }
        for repository in read_directories(&self.root, &mut observations)? {
            for run in read_directories(&repository, &mut observations)? {
                for agent in read_directories(&run, &mut observations)? {
                    let marker_path = agent.join(WORKSPACE_MARKER_FILE);
                    if !marker_path.is_file() {
                        observations.push(WorkspaceOrphanObservation {
                            observed_path: agent,
                            finding: WorkspaceOrphanFinding::MissingMarker,
                            marker: None,
                        });
                        continue;
                    }
                    let marker = match read_marker(&marker_path).and_then(|marker| {
                        marker.verify_integrity()?;
                        Ok(marker)
                    }) {
                        Ok(marker) => marker,
                        Err(error) => {
                            observations.push(WorkspaceOrphanObservation {
                                observed_path: agent,
                                finding: WorkspaceOrphanFinding::InvalidMarker(error.to_string()),
                                marker: None,
                            });
                            continue;
                        }
                    };
                    let expected = self.paths(&marker.coordinates());
                    let finding = if expected.agent_directory != agent
                        || marker.canonical_root != self.root
                        || marker.canonical_checkout != expected.checkout
                    {
                        Some(WorkspaceOrphanFinding::MisplacedMarker)
                    } else if !known_workspaces.contains(&marker.workspace_id) {
                        Some(WorkspaceOrphanFinding::UnreferencedMarker)
                    } else {
                        None
                    };
                    if let Some(finding) = finding {
                        observations.push(WorkspaceOrphanObservation {
                            observed_path: agent,
                            finding,
                            marker: Some(marker),
                        });
                    }
                }
            }
        }
        Ok(observations)
    }

    fn validate_marker_layout(
        &self,
        marker: &WorkspaceOwnershipMarker,
    ) -> Result<WorkspacePaths, WorkspaceLayoutError> {
        if marker.canonical_root != self.root {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_root",
                detail: format!(
                    "expected {}, found {}",
                    self.root.display(),
                    marker.canonical_root.display()
                ),
            });
        }
        let paths = self.paths(&marker.coordinates());
        if marker.canonical_checkout != paths.checkout {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_checkout",
                detail: format!(
                    "expected {}, found {}",
                    paths.checkout.display(),
                    marker.canonical_checkout.display()
                ),
            });
        }
        if !paths.agent_directory.starts_with(&self.root)
            || paths.marker.parent() != Some(paths.agent_directory.as_path())
            || paths.checkout.parent() != Some(paths.agent_directory.as_path())
        {
            return Err(WorkspaceLayoutError::MarkerMismatch {
                field: "containment",
                detail: "derived paths escape their exact workspace directory".into(),
            });
        }
        Ok(paths)
    }
}

/// Read-only diagnostics captured by preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePreflight {
    pub layout: ValidatedWorkspaceStorageLayout,
    pub coordinates: WorkspaceCoordinates,
    pub paths: WorkspacePaths,
    pub canonical_source: PathBuf,
    pub canonical_git_common_directory: PathBuf,
    pub registered_worktrees: Vec<RegisteredGitWorktree>,
    pub available_space_bytes: u64,
    pub same_filesystem_as_source: Option<bool>,
    pub repo_adjacent: bool,
    pub path_limit_bytes: usize,
}

impl WorkspacePreflight {
    /// Construct the initial Ovim-owned marker for this exact preflight.
    pub fn ownership_marker(
        &self,
        metadata: WorkspaceMarkerMetadata,
    ) -> Result<WorkspaceOwnershipMarker, WorkspaceLayoutError> {
        WorkspaceOwnershipMarker::new(
            metadata,
            self.coordinates.clone(),
            self.canonical_source.clone(),
            self.layout.root.clone(),
            self.paths.checkout.clone(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredGitWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
}

/// Metadata fixed before Git worktree creation. Branch/ref strings are stored
/// for exact later reconciliation; they never determine the directory path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMarkerMetadata {
    pub workspace_id: WorkspaceId,
    pub branch: String,
    pub git_ref: String,
    pub base_commit: String,
    pub base_manifest_id: BaseManifestId,
    pub state: WorkspaceMarkerState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOwnership {
    OvimOwned,
    Adopted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMarkerState {
    Reserved,
    Materialized,
    Retained,
    CleanupFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMarkerIntegrity {
    pub algorithm: String,
    pub digest: String,
}

/// Non-secret ownership record stored beside (never inside) `checkout`.
///
/// The SHA-256 digest detects truncation and accidental/tampering edits. It is
/// not an authentication code: durable run-log agreement is still required
/// before any destructive lifecycle operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOwnershipMarker {
    pub marker_version: u32,
    pub integrity: WorkspaceMarkerIntegrity,
    pub workspace_id: WorkspaceId,
    pub repository_id: RepositoryId,
    pub run_id: RunId,
    pub agent_id: AgentId,
    pub canonical_source: PathBuf,
    pub canonical_root: PathBuf,
    pub canonical_checkout: PathBuf,
    pub branch: String,
    pub git_ref: String,
    pub base_commit: String,
    pub base_manifest_id: BaseManifestId,
    pub ownership: WorkspaceOwnership,
    pub state: WorkspaceMarkerState,
}

impl WorkspaceOwnershipMarker {
    fn new(
        metadata: WorkspaceMarkerMetadata,
        coordinates: WorkspaceCoordinates,
        canonical_source: PathBuf,
        canonical_root: PathBuf,
        canonical_checkout: PathBuf,
    ) -> Result<Self, WorkspaceLayoutError> {
        validate_recorded_git_name("branch", &metadata.branch)?;
        validate_recorded_git_name("git_ref", &metadata.git_ref)?;
        validate_base_commit(&metadata.base_commit)?;
        let mut marker = Self {
            marker_version: WORKSPACE_MARKER_VERSION,
            integrity: WorkspaceMarkerIntegrity {
                algorithm: WORKSPACE_MARKER_INTEGRITY.into(),
                digest: String::new(),
            },
            workspace_id: metadata.workspace_id,
            repository_id: coordinates.repository_id,
            run_id: coordinates.run_id,
            agent_id: coordinates.agent_id,
            canonical_source,
            canonical_root,
            canonical_checkout,
            branch: metadata.branch,
            git_ref: metadata.git_ref,
            base_commit: metadata.base_commit,
            base_manifest_id: metadata.base_manifest_id,
            ownership: WorkspaceOwnership::OvimOwned,
            state: metadata.state,
        };
        marker.reseal()?;
        Ok(marker)
    }

    /// Recompute integrity after an intentional state transition. The caller
    /// must still use exact validation and atomic persistence for an update.
    pub fn reseal(&mut self) -> Result<(), WorkspaceLayoutError> {
        self.integrity.algorithm = WORKSPACE_MARKER_INTEGRITY.into();
        self.integrity.digest.clear();
        self.integrity.digest = marker_digest(self)?;
        Ok(())
    }

    pub fn verify_integrity(&self) -> Result<(), WorkspaceLayoutError> {
        if self.marker_version != WORKSPACE_MARKER_VERSION {
            return Err(WorkspaceLayoutError::MarkerInvalid {
                path: self.canonical_checkout.clone(),
                detail: format!(
                    "unsupported marker version {}; expected {}",
                    self.marker_version, WORKSPACE_MARKER_VERSION
                ),
            });
        }
        if self.integrity.algorithm != WORKSPACE_MARKER_INTEGRITY {
            return Err(WorkspaceLayoutError::MarkerInvalid {
                path: self.canonical_checkout.clone(),
                detail: format!(
                    "unsupported integrity algorithm {:?}",
                    self.integrity.algorithm
                ),
            });
        }
        let expected = marker_digest(self)?;
        if self.integrity.digest != expected {
            return Err(WorkspaceLayoutError::MarkerInvalid {
                path: self.canonical_checkout.clone(),
                detail: "integrity digest does not match marker content".into(),
            });
        }
        Ok(())
    }

    pub fn coordinates(&self) -> WorkspaceCoordinates {
        WorkspaceCoordinates::new(
            self.repository_id.clone(),
            self.run_id.clone(),
            self.agent_id.clone(),
        )
    }
}

fn marker_digest(marker: &WorkspaceOwnershipMarker) -> Result<String, WorkspaceLayoutError> {
    let mut unsigned = marker.clone();
    unsigned.integrity.digest.clear();
    let bytes =
        serde_json::to_vec(&unsigned).map_err(|error| WorkspaceLayoutError::MarkerInvalid {
            path: marker.canonical_checkout.clone(),
            detail: format!("cannot encode marker integrity payload: {error}"),
        })?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn validate_recorded_git_name(
    field: &'static str,
    value: &str,
) -> Result<(), WorkspaceLayoutError> {
    if value.trim().is_empty()
        || value.contains('\0')
        || value.contains('\n')
        || value.contains('\r')
    {
        return Err(WorkspaceLayoutError::MarkerMismatch {
            field,
            detail: "must be non-empty and single-line".into(),
        });
    }
    Ok(())
}

fn validate_base_commit(value: &str) -> Result<(), WorkspaceLayoutError> {
    if value.len() < 7 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkspaceLayoutError::MarkerMismatch {
            field: "base_commit",
            detail: "must be a hexadecimal object ID".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactWorkspaceValidation {
    pub marker: WorkspaceOwnershipMarker,
    pub paths: WorkspacePaths,
    pub registered_worktree: Option<RegisteredGitWorktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceOrphanObservation {
    pub observed_path: PathBuf,
    pub finding: WorkspaceOrphanFinding,
    pub marker: Option<WorkspaceOwnershipMarker>,
}

impl WorkspaceOrphanObservation {
    /// Enumeration is diagnostic only. A run-log owner must separately prove
    /// exact identity/path/ref agreement before retirement can be authorized.
    pub fn authorizes_deletion(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceOrphanFinding {
    MissingMarker,
    InvalidMarker(String),
    MisplacedMarker,
    UnreferencedMarker,
    UnexpectedEntry,
}

/// OS facts are injectable so tests need neither global environment changes
/// nor an actually full filesystem.
pub trait WorkspaceProbe {
    fn available_space(&self, path: &Path) -> io::Result<u64>;
    fn path_limit(&self, path: &Path) -> usize;
    fn same_filesystem(&self, left: &Path, right: &Path) -> io::Result<Option<bool>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeWorkspaceProbe;

impl WorkspaceProbe for NativeWorkspaceProbe {
    fn available_space(&self, path: &Path) -> io::Result<u64> {
        fs2::available_space(path)
    }

    fn path_limit(&self, _path: &Path) -> usize {
        #[cfg(windows)]
        {
            // Leave room for Git administrative suffixes on systems without
            // long-path support enabled.
            240
        }
        #[cfg(not(windows))]
        {
            4_096
        }
    }

    fn same_filesystem(&self, left: &Path, right: &Path) -> io::Result<Option<bool>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Some(
                fs::metadata(left)?.dev() == fs::metadata(right)?.dev(),
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = (left, right);
            Ok(None)
        }
    }
}

#[derive(Debug)]
pub enum WorkspaceLayoutError {
    PlatformDataUnavailable,
    UnsafeRoot {
        path: PathBuf,
        reason: String,
    },
    NotGitRepository {
        path: PathBuf,
        detail: String,
    },
    GitUnsupported {
        operation: String,
        detail: String,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    LowDiskSpace {
        available: u64,
        required: u64,
    },
    PathTooLong {
        path: PathBuf,
        limit: usize,
    },
    PathComponentTooLong {
        component_bytes: usize,
        limit: usize,
    },
    WorkspaceCollision {
        path: PathBuf,
    },
    MarkerMissing {
        path: PathBuf,
    },
    MarkerInvalid {
        path: PathBuf,
        detail: String,
    },
    MarkerMismatch {
        field: &'static str,
        detail: String,
    },
}

impl fmt::Display for WorkspaceLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlatformDataUnavailable => {
                formatter.write_str("the platform does not provide a local data directory")
            }
            Self::UnsafeRoot { path, reason } => {
                write!(
                    formatter,
                    "unsafe workspace root {}: {reason}",
                    path.display()
                )
            }
            Self::NotGitRepository { path, detail } => write!(
                formatter,
                "{} is not a supported Git worktree: {detail}",
                path.display()
            ),
            Self::GitUnsupported { operation, detail } => {
                write!(formatter, "Git cannot {operation}: {detail}")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::LowDiskSpace {
                available,
                required,
            } => write!(
                formatter,
                "workspace root has {available} bytes available; {required} bytes required"
            ),
            Self::PathTooLong { path, limit } => write!(
                formatter,
                "workspace path {} exceeds the platform limit of {limit} bytes",
                path.display()
            ),
            Self::PathComponentTooLong {
                component_bytes,
                limit,
            } => write!(
                formatter,
                "workspace path component is {component_bytes} bytes; limit is {limit}"
            ),
            Self::WorkspaceCollision { path } => {
                write!(
                    formatter,
                    "workspace allocation already exists at {}",
                    path.display()
                )
            }
            Self::MarkerMissing { path } => {
                write!(
                    formatter,
                    "workspace marker is missing at {}",
                    path.display()
                )
            }
            Self::MarkerInvalid { path, detail } => {
                write!(
                    formatter,
                    "workspace marker {} is invalid: {detail}",
                    path.display()
                )
            }
            Self::MarkerMismatch { field, detail } => {
                write!(formatter, "workspace marker {field} mismatch: {detail}")
            }
        }
    }
}

impl std::error::Error for WorkspaceLayoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> WorkspaceLayoutError {
    WorkspaceLayoutError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct GitWorkspaceContext {
    canonical_source: PathBuf,
    canonical_common_directory: PathBuf,
    registered_worktrees: Vec<RegisteredGitWorktree>,
}

fn preflight_layout(
    requested: &WorkspaceStorageLayout,
    source_repository: &Path,
    coordinates: &WorkspaceCoordinates,
    probe: &dyn WorkspaceProbe,
) -> Result<WorkspacePreflight, WorkspaceLayoutError> {
    let git = inspect_git_workspace(source_repository)?;
    let candidate = canonicalize_allow_missing(&requested.root)
        .map_err(|source| io_error("resolve workspace root", &requested.root, source))?;

    validate_root(
        &candidate,
        requested.origin,
        &git.canonical_source,
        &git.canonical_common_directory,
        &git.registered_worktrees,
    )?;
    ensure_private_directory(&candidate)?;
    // Canonicalize again after creation to close the missing-tail/symlink gap.
    let canonical_root = fs::canonicalize(&candidate)
        .map_err(|source| io_error("canonicalize workspace root", &candidate, source))?;
    validate_root(
        &canonical_root,
        requested.origin,
        &git.canonical_source,
        &git.canonical_common_directory,
        &git.registered_worktrees,
    )?;
    verify_writable(&canonical_root)?;

    let available_space_bytes = probe
        .available_space(&canonical_root)
        .map_err(|source| io_error("measure workspace free space", &canonical_root, source))?;
    if available_space_bytes < requested.minimum_free_space_bytes {
        return Err(WorkspaceLayoutError::LowDiskSpace {
            available: available_space_bytes,
            required: requested.minimum_free_space_bytes,
        });
    }

    let layout = ValidatedWorkspaceStorageLayout {
        root: canonical_root.clone(),
        origin: requested.origin,
        minimum_free_space_bytes: requested.minimum_free_space_bytes,
    };
    let paths = layout.paths(coordinates);
    validate_path_limits(&paths.checkout, probe.path_limit(&canonical_root))?;
    if paths.agent_directory.exists() {
        return Err(WorkspaceLayoutError::WorkspaceCollision {
            path: paths.agent_directory,
        });
    }

    let same_filesystem_as_source = probe
        .same_filesystem(&canonical_root, &git.canonical_source)
        .map_err(|source| {
            io_error(
                "compare workspace and source filesystems",
                &canonical_root,
                source,
            )
        })?;
    let repo_adjacent = is_repo_adjacent(&canonical_root, &git.canonical_source);

    Ok(WorkspacePreflight {
        layout,
        coordinates: coordinates.clone(),
        paths,
        canonical_source: git.canonical_source,
        canonical_git_common_directory: git.canonical_common_directory,
        registered_worktrees: git.registered_worktrees,
        available_space_bytes,
        same_filesystem_as_source,
        repo_adjacent,
        path_limit_bytes: probe.path_limit(&canonical_root),
    })
}

fn inspect_git_workspace(source: &Path) -> Result<GitWorkspaceContext, WorkspaceLayoutError> {
    let top_level = run_git(
        source,
        &["rev-parse", "--show-toplevel"],
        "find repository root",
    )?;
    let canonical_source = parse_single_git_path(&top_level, source, "repository root")?;
    let common = run_git(
        &canonical_source,
        &["rev-parse", "--git-common-dir"],
        "find common Git directory",
    )?;
    let common_path = parse_single_git_path(&common, &canonical_source, "common Git directory")?;
    let canonical_common_directory = canonicalize_allow_missing(&common_path)
        .map_err(|source| io_error("canonicalize common Git directory", &common_path, source))?;

    let output = Command::new("git")
        .arg("-C")
        .arg(&canonical_source)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|error| WorkspaceLayoutError::GitUnsupported {
            operation: "list registered worktrees".into(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorkspaceLayoutError::GitUnsupported {
            operation: "list registered worktrees".into(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let mut registered_worktrees = Vec::new();
    let mut record = Vec::new();
    for field in output.stdout.split(|byte| *byte == 0) {
        if !field.is_empty() {
            record.push(field);
            continue;
        }
        if !record.is_empty() {
            registered_worktrees.push(parse_worktree_record(&record)?);
            record.clear();
        }
    }
    if !registered_worktrees
        .iter()
        .any(|worktree| worktree.path == canonical_source)
    {
        return Err(WorkspaceLayoutError::GitUnsupported {
            operation: "verify registered source worktree".into(),
            detail: format!(
                "Git did not report {} in `git worktree list --porcelain -z`",
                canonical_source.display()
            ),
        });
    }

    Ok(GitWorkspaceContext {
        canonical_source,
        canonical_common_directory,
        registered_worktrees,
    })
}

fn parse_worktree_record(fields: &[&[u8]]) -> Result<RegisteredGitWorktree, WorkspaceLayoutError> {
    let raw_path = fields
        .iter()
        .find_map(|field| field.strip_prefix(b"worktree "))
        .ok_or_else(|| WorkspaceLayoutError::GitUnsupported {
            operation: "parse registered worktree".into(),
            detail: "porcelain record has no worktree path".into(),
        })?;
    let path =
        path_from_git_bytes(raw_path).map_err(|detail| WorkspaceLayoutError::GitUnsupported {
            operation: "decode registered worktree path".into(),
            detail,
        })?;
    let path = canonicalize_allow_missing(&path)
        .map_err(|source| io_error("canonicalize registered worktree", &path, source))?;
    let text_field = |prefix: &[u8]| {
        fields
            .iter()
            .find_map(|field| field.strip_prefix(prefix))
            .map(|value| String::from_utf8_lossy(value).into_owned())
    };
    Ok(RegisteredGitWorktree {
        path,
        head: text_field(b"HEAD "),
        branch: text_field(b"branch "),
    })
}

fn run_git(source: &Path, args: &[&str], operation: &str) -> Result<Vec<u8>, WorkspaceLayoutError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()
        .map_err(|error| WorkspaceLayoutError::NotGitRepository {
            path: source.to_path_buf(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorkspaceLayoutError::NotGitRepository {
            path: source.to_path_buf(),
            detail: format!(
                "{operation}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(output.stdout)
}

fn parse_single_git_path(
    bytes: &[u8],
    relative_to: &Path,
    description: &str,
) -> Result<PathBuf, WorkspaceLayoutError> {
    let raw = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let mut path =
        path_from_git_bytes(raw).map_err(|detail| WorkspaceLayoutError::GitUnsupported {
            operation: format!("decode {description}"),
            detail,
        })?;
    if path.is_relative() {
        path = relative_to.join(path);
    }
    canonicalize_allow_missing(&path)
        .map_err(|source| io_error("canonicalize Git-reported path", &path, source))
}

#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    String::from_utf8(bytes.to_vec())
        .map(PathBuf::from)
        .map_err(|error| error.to_string())
}

fn canonicalize_allow_missing(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace paths must be absolute",
        ));
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let component = existing
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing path ancestor"))?;
        if component == OsStr::new(".") || component == OsStr::new("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dot path components are not accepted",
            ));
        }
        missing.push(component.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing path ancestor"))?;
    }
    let mut canonical = fs::canonicalize(existing)?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn validate_root(
    root: &Path,
    origin: WorkspaceRootOrigin,
    source: &Path,
    common_git_directory: &Path,
    registered_worktrees: &[RegisteredGitWorktree],
) -> Result<(), WorkspaceLayoutError> {
    let normal_components = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if root.parent().is_none() || normal_components < 2 {
        return Err(unsafe_root(root, "it is an unsafe broad filesystem root"));
    }
    if dirs::home_dir().as_deref() == Some(root) {
        return Err(unsafe_root(root, "it is the user's home directory"));
    }
    if is_equal_or_descendant(root, source) {
        return Err(unsafe_root(
            root,
            "it is equal to or inside the source worktree",
        ));
    }
    if is_equal_or_descendant(root, common_git_directory) {
        return Err(unsafe_root(
            root,
            "it is equal to or inside the common Git directory",
        ));
    }
    for registered in registered_worktrees {
        if is_equal_or_descendant(root, &registered.path) {
            return Err(unsafe_root(
                root,
                &format!(
                    "it is equal to or inside registered worktree {}",
                    registered.path.display()
                ),
            ));
        }
    }
    if is_repo_adjacent(root, source) && !origin.is_explicit() {
        return Err(unsafe_root(
            root,
            "repo-adjacent storage requires an explicit environment or configuration override",
        ));
    }
    Ok(())
}

fn unsafe_root(path: &Path, reason: &str) -> WorkspaceLayoutError {
    WorkspaceLayoutError::UnsafeRoot {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

fn is_equal_or_descendant(path: &Path, ancestor: &Path) -> bool {
    path == ancestor || path.starts_with(ancestor)
}

fn is_repo_adjacent(root: &Path, source: &Path) -> bool {
    let Some(source_name) = source.file_name() else {
        return false;
    };
    let mut expected_name = source_name.to_os_string();
    expected_name.push("-worktrees");
    root.parent() == source.parent() && root.file_name() == Some(expected_name.as_os_str())
}

fn ensure_private_directory(path: &Path) -> Result<(), WorkspaceLayoutError> {
    fs::create_dir_all(path)
        .map_err(|source| io_error("create workspace directory", path, source))?;
    let metadata = fs::metadata(path)
        .map_err(|source| io_error("inspect workspace directory", path, source))?;
    if !metadata.is_dir() {
        return Err(unsafe_root(path, "it is not a directory"));
    }
    set_private_directory_permissions(path)?;
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<(), WorkspaceLayoutError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("set private workspace permissions", path, source))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn verify_writable(root: &Path) -> Result<(), WorkspaceLayoutError> {
    for _ in 0..100 {
        let temporary = root.join(format!(
            ".ovim-write-probe-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => {
                drop(file);
                fs::remove_file(&temporary).map_err(|source| {
                    io_error("remove workspace write probe", &temporary, source)
                })?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error("verify workspace root is writable", root, source));
            }
        }
    }
    Err(io_error(
        "verify workspace root is writable",
        root,
        io::Error::new(io::ErrorKind::AlreadyExists, "temporary name exhaustion"),
    ))
}

fn validate_path_limits(path: &Path, path_limit: usize) -> Result<(), WorkspaceLayoutError> {
    for component in path.components() {
        if let Component::Normal(component) = component {
            let bytes = component.as_encoded_bytes().len();
            if bytes > MAX_PATH_COMPONENT_BYTES {
                return Err(WorkspaceLayoutError::PathComponentTooLong {
                    component_bytes: bytes,
                    limit: MAX_PATH_COMPONENT_BYTES,
                });
            }
        }
    }
    let path_bytes = path.as_os_str().as_encoded_bytes().len();
    if path_bytes > path_limit {
        return Err(WorkspaceLayoutError::PathTooLong {
            path: path.to_path_buf(),
            limit: path_limit,
        });
    }
    Ok(())
}

fn write_marker_atomically(
    destination: &Path,
    marker: &WorkspaceOwnershipMarker,
) -> Result<(), WorkspaceLayoutError> {
    let parent = destination
        .parent()
        .ok_or_else(|| WorkspaceLayoutError::MarkerInvalid {
            path: destination.to_path_buf(),
            detail: "marker has no parent directory".into(),
        })?;
    let mut encoded =
        serde_json::to_vec_pretty(marker).map_err(|error| WorkspaceLayoutError::MarkerInvalid {
            path: destination.to_path_buf(),
            detail: format!("cannot encode marker: {error}"),
        })?;
    encoded.push(b'\n');

    for _ in 0..100 {
        let temporary = parent.join(format!(
            ".workspace.json.tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error(
                    "create temporary workspace marker",
                    &temporary,
                    source,
                ))
            }
        };
        let write_result = (|| -> io::Result<()> {
            file.write_all(&encoded)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::hard_link(&temporary, destination)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        return match write_result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(WorkspaceLayoutError::WorkspaceCollision {
                    path: destination.to_path_buf(),
                })
            }
            Err(source) => Err(io_error("publish workspace marker", destination, source)),
        };
    }
    Err(io_error(
        "create temporary workspace marker",
        destination,
        io::Error::new(io::ErrorKind::AlreadyExists, "temporary name exhaustion"),
    ))
}

fn read_marker(path: &Path) -> Result<WorkspaceOwnershipMarker, WorkspaceLayoutError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(WorkspaceLayoutError::MarkerMissing {
                path: path.to_path_buf(),
            });
        }
        Err(source) => return Err(io_error("open workspace marker", path, source)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error("read workspace marker", path, source))?;
    serde_json::from_slice(&bytes).map_err(|error| WorkspaceLayoutError::MarkerInvalid {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

fn marker_error_at(error: WorkspaceLayoutError, marker_path: &Path) -> WorkspaceLayoutError {
    match error {
        WorkspaceLayoutError::MarkerInvalid { detail, .. } => WorkspaceLayoutError::MarkerInvalid {
            path: marker_path.to_path_buf(),
            detail,
        },
        other => other,
    }
}

fn read_directories(
    parent: &Path,
    observations: &mut Vec<WorkspaceOrphanObservation>,
) -> Result<Vec<PathBuf>, WorkspaceLayoutError> {
    let entries = fs::read_dir(parent)
        .map_err(|source| io_error("enumerate workspace storage", parent, source))?;
    let mut directories = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("read workspace storage entry", parent, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| io_error("inspect workspace storage entry", &path, source))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            directories.push(path);
        } else {
            observations.push(WorkspaceOrphanObservation {
                observed_path: path,
                finding: WorkspaceOrphanFinding::UnexpectedEntry,
                marker: None,
            });
        }
    }
    directories.sort();
    Ok(directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        available: io::Result<u64>,
        path_limit: usize,
        same_filesystem: io::Result<Option<bool>>,
    }

    impl FakeProbe {
        fn permissive() -> Self {
            Self {
                available: Ok(u64::MAX),
                path_limit: 4_096,
                same_filesystem: Ok(Some(true)),
            }
        }
    }

    impl WorkspaceProbe for FakeProbe {
        fn available_space(&self, _path: &Path) -> io::Result<u64> {
            clone_io_result(&self.available)
        }

        fn path_limit(&self, _path: &Path) -> usize {
            self.path_limit
        }

        fn same_filesystem(&self, _left: &Path, _right: &Path) -> io::Result<Option<bool>> {
            clone_io_result(&self.same_filesystem)
        }
    }

    fn clone_io_result<T: Copy>(result: &io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => Ok(*value),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }

    fn coordinates(suffix: &str) -> WorkspaceCoordinates {
        WorkspaceCoordinates::new(
            RepositoryId::parse(format!("repo_{suffix}")).unwrap(),
            RunId::parse(format!("run_{suffix}")).unwrap(),
            AgentId::parse(format!("agt_{suffix}")).unwrap(),
        )
    }

    fn git_repository() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        git(repository.path(), &["init", "-q"]);
        fs::write(repository.path().join("README"), "workspace layout test\n").unwrap();
        git(repository.path(), &["add", "README"]);
        git(
            repository.path(),
            &[
                "-c",
                "user.name=Ovim Test",
                "-c",
                "user.email=ovim@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        );
        repository
    }

    fn git(repository: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn environment_then_config_then_platform_default_precedence_is_injectable() {
        let mut config = DelegatedAgentWorkspacePolicy {
            root: Some(PathBuf::from("/configured/workspaces")),
            ..Default::default()
        };

        let environment = WorkspaceStorageLayout::from_locations(
            Some(OsString::from("/environment/workspaces")),
            &config,
            Some(PathBuf::from("/platform/data")),
        )
        .unwrap();
        assert_eq!(
            environment.root_candidate(),
            Path::new("/environment/workspaces")
        );
        assert_eq!(environment.origin(), WorkspaceRootOrigin::Environment);

        let configured = WorkspaceStorageLayout::from_locations(
            None,
            &config,
            Some(PathBuf::from("/platform/data")),
        )
        .unwrap();
        assert_eq!(
            configured.root_candidate(),
            Path::new("/configured/workspaces")
        );
        assert_eq!(configured.origin(), WorkspaceRootOrigin::Configuration);

        config.root = None;
        let platform = WorkspaceStorageLayout::from_locations(
            None,
            &config,
            Some(PathBuf::from("/platform/data")),
        )
        .unwrap();
        assert_eq!(
            platform.root_candidate(),
            Path::new("/platform/data/ovim/workspaces")
        );
        assert_eq!(platform.origin(), WorkspaceRootOrigin::PlatformDefault);
    }

    #[test]
    fn opaque_hostile_identities_cannot_escape_or_collide() {
        let root = Path::new("/safe/ovim/workspaces");
        let layout = WorkspaceStorageLayout::new(root, WorkspaceRootOrigin::Configuration, 0);
        let hostile = WorkspaceCoordinates::new(
            RepositoryId::parse("repo_../../same").unwrap(),
            RunId::parse("run_..\\../same").unwrap(),
            AgentId::parse("agt_task/model prose /../same").unwrap(),
        );
        let similar = WorkspaceCoordinates::new(
            RepositoryId::parse("repo_..-..-same").unwrap(),
            RunId::parse("run_..-..-same").unwrap(),
            AgentId::parse("agt_task-model-prose-same").unwrap(),
        );

        let hostile_paths = layout.candidate_paths(&hostile);
        let similar_paths = layout.candidate_paths(&similar);
        assert!(hostile_paths.checkout.starts_with(root));
        assert_eq!(hostile_paths.checkout.ancestors().nth(4), Some(root));
        assert_ne!(hostile_paths.agent_directory, similar_paths.agent_directory);
        for component in hostile_paths
            .checkout
            .strip_prefix(root)
            .unwrap()
            .components()
        {
            let Component::Normal(component) = component else {
                panic!("derived path contained traversal");
            };
            assert!(!component.to_string_lossy().contains('/'));
        }
    }

    #[test]
    fn preflight_canonicalizes_root_and_reports_locality() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let requested = storage.path().join("nested").join("workspaces");
        let layout = WorkspaceStorageLayout::new(&requested, WorkspaceRootOrigin::Configuration, 1);

        let result = layout
            .preflight_with(
                repository.path(),
                &coordinates("canonical"),
                &FakeProbe::permissive(),
            )
            .unwrap();

        assert_eq!(result.layout.root(), fs::canonicalize(requested).unwrap());
        assert_eq!(result.same_filesystem_as_source, Some(true));
        assert!(!result.repo_adjacent);
        assert!(result
            .registered_worktrees
            .iter()
            .any(|worktree| worktree.path == fs::canonicalize(repository.path()).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_override_is_validated_at_its_target() {
        use std::os::unix::fs::symlink;
        let repository = git_repository();
        let inside = repository.path().join("agent-storage");
        fs::create_dir(&inside).unwrap();
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("workspaces");
        symlink(&inside, &link).unwrap();
        let layout = WorkspaceStorageLayout::new(link, WorkspaceRootOrigin::Configuration, 0);

        let error = layout
            .preflight_with(
                repository.path(),
                &coordinates("symlink"),
                &FakeProbe::permissive(),
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceLayoutError::UnsafeRoot { .. }));
    }

    #[test]
    fn source_and_git_administration_roots_are_rejected() {
        let repository = git_repository();
        for root in [
            repository.path().to_path_buf(),
            repository.path().join("nested"),
            repository.path().join(".git").join("ovim-workspaces"),
        ] {
            let layout = WorkspaceStorageLayout::new(root, WorkspaceRootOrigin::Configuration, 0);
            assert!(matches!(
                layout.preflight_with(
                    repository.path(),
                    &coordinates("source-rejection"),
                    &FakeProbe::permissive()
                ),
                Err(WorkspaceLayoutError::UnsafeRoot { .. })
            ));
        }
    }

    #[test]
    fn any_registered_worktree_root_is_rejected() {
        let repository = git_repository();
        let linked_parent = tempfile::tempdir().unwrap();
        let linked = linked_parent.path().join("linked");
        git(
            repository.path(),
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        let layout = WorkspaceStorageLayout::new(
            linked.join("storage"),
            WorkspaceRootOrigin::Configuration,
            0,
        );

        let error = layout
            .preflight_with(
                repository.path(),
                &coordinates("registered"),
                &FakeProbe::permissive(),
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceLayoutError::UnsafeRoot { .. }));
    }

    #[test]
    fn repo_adjacent_layout_requires_explicit_override() {
        let repository = git_repository();
        let source = fs::canonicalize(repository.path()).unwrap();
        let adjacent = source.parent().unwrap().join(format!(
            "{}-worktrees",
            source.file_name().unwrap().to_string_lossy()
        ));

        let implicit =
            WorkspaceStorageLayout::new(&adjacent, WorkspaceRootOrigin::PlatformDefault, 0);
        assert!(matches!(
            implicit.preflight_with(
                repository.path(),
                &coordinates("implicit-adjacent"),
                &FakeProbe::permissive()
            ),
            Err(WorkspaceLayoutError::UnsafeRoot { .. })
        ));

        let explicit =
            WorkspaceStorageLayout::new(&adjacent, WorkspaceRootOrigin::Configuration, 0);
        let result = explicit
            .preflight_with(
                repository.path(),
                &coordinates("explicit-adjacent"),
                &FakeProbe::permissive(),
            )
            .unwrap();
        assert!(result.repo_adjacent);
        fs::remove_dir(result.layout.root()).unwrap();
    }

    #[test]
    fn low_disk_and_path_limits_fail_before_allocation() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let layout = WorkspaceStorageLayout::new(
            storage.path().join("workspaces"),
            WorkspaceRootOrigin::Configuration,
            2,
        );
        let low_disk = FakeProbe {
            available: Ok(MEBIBYTE),
            ..FakeProbe::permissive()
        };
        assert!(matches!(
            layout.preflight_with(repository.path(), &coordinates("disk"), &low_disk),
            Err(WorkspaceLayoutError::LowDiskSpace { .. })
        ));

        let short_path = FakeProbe {
            path_limit: 10,
            ..FakeProbe::permissive()
        };
        assert!(matches!(
            layout.preflight_with(repository.path(), &coordinates("path"), &short_path),
            Err(WorkspaceLayoutError::PathTooLong { .. })
        ));

        let long_component = WorkspaceCoordinates::new(
            RepositoryId::parse(format!("repo_{}", "x".repeat(200))).unwrap(),
            RunId::parse("run_component-limit").unwrap(),
            AgentId::parse("agt_component-limit").unwrap(),
        );
        assert!(matches!(
            layout.preflight_with(repository.path(), &long_component, &FakeProbe::permissive()),
            Err(WorkspaceLayoutError::PathComponentTooLong { .. })
        ));
    }

    #[test]
    fn broad_filesystem_roots_are_rejected_without_creation() {
        let repository = git_repository();
        let filesystem_root = repository
            .path()
            .ancestors()
            .last()
            .expect("absolute temporary path has a filesystem root");
        let layout =
            WorkspaceStorageLayout::new(filesystem_root, WorkspaceRootOrigin::Configuration, 0);

        assert!(matches!(
            layout.preflight_with(
                repository.path(),
                &coordinates("broad-root"),
                &FakeProbe::permissive()
            ),
            Err(WorkspaceLayoutError::UnsafeRoot { .. })
        ));
    }

    #[test]
    fn path_collision_is_never_reused() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let coordinates = coordinates("collision");
        let layout = WorkspaceStorageLayout::new(
            storage.path().join("workspaces"),
            WorkspaceRootOrigin::Configuration,
            0,
        );
        let paths = layout.candidate_paths(&coordinates);
        fs::create_dir_all(&paths.agent_directory).unwrap();

        assert!(matches!(
            layout.preflight_with(repository.path(), &coordinates, &FakeProbe::permissive()),
            Err(WorkspaceLayoutError::WorkspaceCollision { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn prepared_root_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let layout = WorkspaceStorageLayout::new(
            storage.path().join("workspaces"),
            WorkspaceRootOrigin::Configuration,
            0,
        );
        let result = layout
            .preflight_with(
                repository.path(),
                &coordinates("permissions"),
                &FakeProbe::permissive(),
            )
            .unwrap();

        assert_eq!(
            fs::metadata(result.layout.root())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn non_directory_ancestor_fails_writability_preflight() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let blocker = storage.path().join("not-a-directory");
        fs::write(&blocker, "blocked").unwrap();
        let layout = WorkspaceStorageLayout::new(
            blocker.join("workspaces"),
            WorkspaceRootOrigin::Configuration,
            0,
        );

        assert!(matches!(
            layout.preflight_with(
                repository.path(),
                &coordinates("writability"),
                &FakeProbe::permissive()
            ),
            Err(WorkspaceLayoutError::Io { .. })
        ));
    }

    fn git_stdout(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn marker_metadata(repository: &Path, suffix: &str) -> WorkspaceMarkerMetadata {
        WorkspaceMarkerMetadata {
            workspace_id: WorkspaceId::parse(format!("wsp_{suffix}")).unwrap(),
            branch: format!("ovim/{suffix}"),
            git_ref: format!("refs/heads/ovim/{suffix}"),
            base_commit: git_stdout(repository, &["rev-parse", "HEAD"]),
            base_manifest_id: BaseManifestId::parse(format!("bsm_{suffix}")).unwrap(),
            state: WorkspaceMarkerState::Reserved,
        }
    }

    fn prepared_marker(
        repository: &Path,
        storage: &Path,
        suffix: &str,
    ) -> (WorkspacePreflight, WorkspaceOwnershipMarker) {
        let layout = WorkspaceStorageLayout::new(
            storage.join("workspaces"),
            WorkspaceRootOrigin::Configuration,
            0,
        );
        let preflight = layout
            .preflight_with(repository, &coordinates(suffix), &FakeProbe::permissive())
            .unwrap();
        let marker = preflight
            .ownership_marker(marker_metadata(repository, suffix))
            .unwrap();
        (preflight, marker)
    }

    fn overwrite_marker(path: &Path, marker: &WorkspaceOwnershipMarker) {
        let mut bytes = serde_json::to_vec_pretty(marker).unwrap();
        bytes.push(b'\n');
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn marker_records_exact_identity_and_is_published_beside_checkout() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "marker");
        assert!(marker.verify_integrity().is_ok());

        let paths = preflight.layout.initialize_marker(&marker).unwrap();
        assert_eq!(paths.marker.parent(), paths.checkout.parent());
        assert!(!paths.checkout.exists());
        let decoded = read_marker(&paths.marker).unwrap();
        assert_eq!(decoded, marker);
        assert_eq!(decoded.workspace_id.as_str(), "wsp_marker");
        assert_eq!(decoded.repository_id, preflight.coordinates.repository_id);
        assert_eq!(decoded.run_id, preflight.coordinates.run_id);
        assert_eq!(decoded.agent_id, preflight.coordinates.agent_id);
        assert_eq!(decoded.canonical_source, preflight.canonical_source);
        assert_eq!(decoded.canonical_root, preflight.layout.root);
        assert_eq!(decoded.canonical_checkout, paths.checkout);
        assert_eq!(decoded.base_manifest_id.as_str(), "bsm_marker");
        assert_eq!(decoded.ownership, WorkspaceOwnership::OvimOwned);
        assert_eq!(decoded.state, WorkspaceMarkerState::Reserved);

        let exact = preflight
            .layout
            .validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            )
            .unwrap();
        assert_eq!(exact.marker, marker);
        assert!(exact.registered_worktree.is_none());
    }

    #[test]
    fn marker_collision_never_replaces_the_original() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "atomic");
        let paths = preflight.layout.initialize_marker(&marker).unwrap();
        let original = fs::read(&paths.marker).unwrap();

        assert!(matches!(
            preflight.layout.initialize_marker(&marker),
            Err(WorkspaceLayoutError::WorkspaceCollision { .. })
        ));
        assert_eq!(fs::read(paths.marker).unwrap(), original);
    }

    #[test]
    fn marker_tampering_and_unknown_version_fail_closed() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "tamper");
        let paths = preflight.layout.initialize_marker(&marker).unwrap();

        let mut tampered = marker.clone();
        tampered.branch = "someone/else".into();
        overwrite_marker(&paths.marker, &tampered);
        let error = preflight
            .layout
            .validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceLayoutError::MarkerInvalid { .. }));

        let mut future = marker.clone();
        future.marker_version = WORKSPACE_MARKER_VERSION + 1;
        future.reseal().unwrap();
        overwrite_marker(&paths.marker, &future);
        let error = preflight
            .layout
            .validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceLayoutError::MarkerInvalid { .. }));
        assert!(error.to_string().contains("unsupported marker version"));
    }

    #[test]
    fn resealed_identity_and_containment_mismatches_are_rejected() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "mismatch");
        let paths = preflight.layout.initialize_marker(&marker).unwrap();

        let mut wrong_id = marker.clone();
        wrong_id.workspace_id = WorkspaceId::parse("wsp_different").unwrap();
        wrong_id.reseal().unwrap();
        overwrite_marker(&paths.marker, &wrong_id);
        assert!(matches!(
            preflight.layout.validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            ),
            Err(WorkspaceLayoutError::MarkerMismatch {
                field: "workspace_id",
                ..
            })
        ));

        let mut escaped = marker.clone();
        escaped.canonical_checkout = storage.path().join("elsewhere");
        escaped.reseal().unwrap();
        overwrite_marker(&paths.marker, &escaped);
        assert!(matches!(
            preflight.layout.validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            ),
            Err(WorkspaceLayoutError::MarkerMismatch {
                field: "canonical_checkout",
                ..
            })
        ));
    }

    #[test]
    fn exact_registered_worktree_lookup_checks_recorded_ref() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) =
            prepared_marker(repository.path(), storage.path(), "registered-ref");
        let paths = preflight.layout.initialize_marker(&marker).unwrap();
        git(
            repository.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &marker.branch,
                paths.checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let exact = preflight
            .layout
            .validate_exact_workspace(
                repository.path(),
                &preflight.coordinates,
                &marker.workspace_id,
            )
            .unwrap();
        let registered = exact.registered_worktree.unwrap();
        assert_eq!(registered.path, paths.checkout);
        assert_eq!(registered.branch.as_deref(), Some(marker.git_ref.as_str()));
        assert_eq!(
            preflight
                .layout
                .registered_worktree_at(repository.path(), &paths.checkout)
                .unwrap(),
            Some(registered)
        );
        assert!(preflight
            .layout
            .registered_worktree_at(repository.path(), &paths.agent_directory.join("similar"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn orphan_enumeration_surfaces_but_never_authorizes_deletion() {
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "orphan");
        preflight.layout.initialize_marker(&marker).unwrap();
        let name_derived_decoy = preflight
            .layout
            .root()
            .join("ovim-task-prose")
            .join("branch-prefix")
            .join("agent-looking-name");
        fs::create_dir_all(&name_derived_decoy).unwrap();

        let observations = preflight.layout.enumerate_orphans(&HashSet::new()).unwrap();
        assert!(observations.iter().any(|observation| {
            matches!(
                observation.finding,
                WorkspaceOrphanFinding::UnreferencedMarker
            ) && observation
                .marker
                .as_ref()
                .map(|marker| &marker.workspace_id)
                == Some(&marker.workspace_id)
        }));
        assert!(observations.iter().any(|observation| {
            observation.observed_path == name_derived_decoy
                && observation.finding == WorkspaceOrphanFinding::MissingMarker
        }));
        assert!(observations
            .iter()
            .all(|observation| !observation.authorizes_deletion()));

        let known = HashSet::from([marker.workspace_id.clone()]);
        let observations = preflight.layout.enumerate_orphans(&known).unwrap();
        assert!(!observations.iter().any(|observation| {
            observation
                .marker
                .as_ref()
                .map(|marker| &marker.workspace_id)
                == Some(&marker.workspace_id)
        }));
        assert!(observations
            .iter()
            .all(|observation| !observation.authorizes_deletion()));
    }

    #[cfg(unix)]
    #[test]
    fn marker_and_identity_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let repository = git_repository();
        let storage = tempfile::tempdir().unwrap();
        let (preflight, marker) = prepared_marker(repository.path(), storage.path(), "marker-mode");
        let paths = preflight.layout.initialize_marker(&marker).unwrap();

        for directory in [
            &paths.repository_directory,
            &paths.run_directory,
            &paths.agent_directory,
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(paths.marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
