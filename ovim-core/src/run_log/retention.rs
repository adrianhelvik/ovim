//! Conservative retention for local AI run history.
//!
//! A run is eligible only when it is old, no conversation binding points to
//! it, and every recorded owner is proven dead. Unknown/corrupt layouts are
//! preserved for manual recovery rather than guessed safe to delete.

use super::{
    decoded_run_component, process_is_alive, CatalogError, RunCatalog, RunId, RunStorageLayout,
};
use rusqlite::{Connection, OpenFlags};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const DEFAULT_RUN_HISTORY_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct RunHistoryCleanupOptions {
    pub max_age: Duration,
    pub dry_run: bool,
}

impl Default for RunHistoryCleanupOptions {
    fn default() -> Self {
        Self {
            max_age: DEFAULT_RUN_HISTORY_MAX_AGE,
            dry_run: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHistoryCandidate {
    pub run_id: RunId,
    pub bytes: u64,
    pub age: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunHistoryCleanupReport {
    pub scanned_runs: usize,
    pub scanned_bytes: u64,
    pub candidates: Vec<RunHistoryCandidate>,
    pub removed_runs: usize,
    pub removed_bytes: u64,
    pub bound_runs: usize,
    pub live_runs: usize,
    pub recent_runs: usize,
    pub preserved_runs: usize,
    pub issues: Vec<String>,
}

#[derive(Debug)]
pub enum RunHistoryCleanupError {
    Catalog(CatalogError),
    Storage {
        operation: &'static str,
        path: PathBuf,
        detail: String,
    },
}

impl fmt::Display for RunHistoryCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => write!(formatter, "run catalog: {error}"),
            Self::Storage {
                operation,
                path,
                detail,
            } => write!(formatter, "{operation} {}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for RunHistoryCleanupError {}

impl From<CatalogError> for RunHistoryCleanupError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

pub fn cleanup_run_history(
    layout: &RunStorageLayout,
    options: &RunHistoryCleanupOptions,
) -> Result<RunHistoryCleanupReport, RunHistoryCleanupError> {
    cleanup_run_history_at(layout, options, SystemTime::now())
}

fn cleanup_run_history_at(
    layout: &RunStorageLayout,
    options: &RunHistoryCleanupOptions,
    now: SystemTime,
) -> Result<RunHistoryCleanupReport, RunHistoryCleanupError> {
    let catalog = RunCatalog::open(layout)?;
    let Some(_cleanup_lock) = acquire_cleanup_lock(layout)? else {
        return Ok(RunHistoryCleanupReport {
            issues: vec!["another AI history cleanup is already running".into()],
            ..RunHistoryCleanupReport::default()
        });
    };
    let bound = catalog.bound_run_ids()?;
    let entries = fs::read_dir(layout.root())
        .map_err(|error| storage("read run history", layout.root(), error))?;
    let mut report = RunHistoryCleanupReport::default();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report
                    .issues
                    .push(format!("could not enumerate a run history entry: {error}"));
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with("run-") {
            continue;
        }
        let Some(run_id) = decoded_run_component(entry.file_name().as_ref()) else {
            report.preserved_runs += 1;
            report
                .issues
                .push(format!("preserved malformed run directory name {name:?}"));
            continue;
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                preserve_issue(&mut report, &run_id, format!("inspect entry: {error}"));
                continue;
            }
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            preserve_issue(&mut report, &run_id, "entry is not a real directory".into());
            continue;
        }
        if layout.run_directory(&run_id) != path {
            preserve_issue(
                &mut report,
                &run_id,
                "encoded path does not match its run ID".into(),
            );
            continue;
        }

        report.scanned_runs += 1;
        let stats = match directory_stats(&path) {
            Ok(stats) => stats,
            Err(error) => {
                preserve_issue(&mut report, &run_id, error);
                continue;
            }
        };
        report.scanned_bytes = report.scanned_bytes.saturating_add(stats.bytes);

        if let Err(error) = validate_event_database(layout, &run_id) {
            preserve_issue(&mut report, &run_id, error);
            continue;
        }
        if bound.contains(&run_id) {
            report.bound_runs += 1;
            continue;
        }
        if has_live_owner(&catalog, &run_id)? {
            report.live_runs += 1;
            continue;
        }
        let age = now
            .duration_since(stats.latest_modified)
            .unwrap_or_default();
        if age < options.max_age {
            report.recent_runs += 1;
            continue;
        }

        let candidate = RunHistoryCandidate {
            run_id: run_id.clone(),
            bytes: stats.bytes,
            age,
        };
        report.candidates.push(candidate);
        if options.dry_run {
            continue;
        }

        // Recheck catalog state immediately before deletion. A freshly bound
        // or newly owned run wins the race and is preserved.
        if catalog.bound_run_ids()?.contains(&run_id) {
            report.bound_runs += 1;
            continue;
        }
        if has_live_owner(&catalog, &run_id)? {
            report.live_runs += 1;
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => {
                report.removed_runs += 1;
                report.removed_bytes = report.removed_bytes.saturating_add(stats.bytes);
                if let Err(error) = catalog.delete_run_owner_records(&run_id) {
                    report.issues.push(format!(
                        "removed {run_id}, but could not prune stale ownership metadata: {error}"
                    ));
                }
            }
            Err(error) => preserve_issue(&mut report, &run_id, format!("remove: {error}")),
        }
    }

    report
        .candidates
        .sort_by_key(|candidate| std::cmp::Reverse(candidate.age));
    report.issues.sort();
    Ok(report)
}

fn acquire_cleanup_lock(layout: &RunStorageLayout) -> Result<Option<File>, RunHistoryCleanupError> {
    let path = layout.root().join(".retention.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|error| storage("open cleanup lock", &path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| storage("set cleanup lock permissions", &path, error))?;
    }
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(storage("lock run history cleanup", &path, error)),
    }
}

fn has_live_owner(catalog: &RunCatalog, run_id: &RunId) -> Result<bool, CatalogError> {
    Ok(catalog
        .run_owners(run_id)?
        .into_iter()
        .any(|owner| process_is_alive(owner.pid, owner.process_start_time)))
}

fn validate_event_database(layout: &RunStorageLayout, run_id: &RunId) -> Result<(), String> {
    let database = layout.event_database(run_id);
    let metadata = match fs::symlink_metadata(&database) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect event database: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("event database is not a regular file".into());
    }
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("open event database read-only: {error}"))?;
    let integrity: String = connection
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .map_err(|error| format!("check event database: {error}"))?;
    if integrity != "ok" {
        return Err(format!(
            "event database integrity check returned {integrity:?}"
        ));
    }
    let mut statement = connection
        .prepare("SELECT run_id FROM runs")
        .map_err(|error| format!("read event database schema: {error}"))?;
    let stored = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("read stored run IDs: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("decode stored run IDs: {error}"))?;
    if stored.iter().any(|stored| stored != run_id.as_str()) {
        return Err("event database contains a different run ID".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct DirectoryStats {
    bytes: u64,
    latest_modified: SystemTime,
}

fn directory_stats(root: &Path) -> Result<DirectoryStats, String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("inspect directory {}: {error}", root.display()))?;
    let mut stats = DirectoryStats {
        bytes: metadata.len(),
        latest_modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    };
    for entry in
        fs::read_dir(root).map_err(|error| format!("read directory {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        stats.latest_modified = stats
            .latest_modified
            .max(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let child = directory_stats(&entry.path())?;
            // The recursive result includes the child directory metadata,
            // which was already counted above.
            stats.bytes = stats
                .bytes
                .saturating_add(child.bytes.saturating_sub(metadata.len()));
            stats.latest_modified = stats.latest_modified.max(child.latest_modified);
        }
    }
    Ok(stats)
}

fn preserve_issue(report: &mut RunHistoryCleanupReport, run_id: &RunId, detail: String) {
    report.preserved_runs += 1;
    report.issues.push(format!("preserved {run_id}: {detail}"));
}

fn storage(operation: &'static str, path: &Path, error: std::io::Error) -> RunHistoryCleanupError {
    RunHistoryCleanupError::Storage {
        operation,
        path: path.to_owned(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::{BranchId, ConversationKey, ConversationScope};

    #[test]
    fn cleanup_removes_only_old_unbound_runs_without_live_owners() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository_root = temporary.path().join("workspace");
        fs::create_dir(&repository_root).unwrap();
        let repository = catalog
            .register_approved_folder(&repository_root.canonicalize().unwrap())
            .unwrap();
        let binding = catalog
            .start_fresh_conversation(
                ConversationKey {
                    repository_id: repository.repository_id,
                    scope: ConversationScope::NoFile,
                    logical_name: "chat".into(),
                },
                BranchId::new(),
            )
            .unwrap()
            .into_binding();
        layout.ensure_run_directory(&binding.run_id).unwrap();

        let orphan = RunId::new();
        layout.ensure_run_directory(&orphan).unwrap();
        fs::write(layout.run_directory(&orphan).join("artifact"), b"old data").unwrap();

        let live = RunId::new();
        layout.ensure_run_directory(&live).unwrap();
        let (pid, start) = super::super::current_process_liveness();
        catalog.register_run_owner(&live, pid, start).unwrap();

        let corrupt = RunId::new();
        layout.ensure_run_directory(&corrupt).unwrap();
        fs::write(layout.event_database(&corrupt), b"not sqlite").unwrap();
        fs::create_dir(layout.root().join("unresolved-workspaces")).unwrap();
        drop(catalog);

        let options = RunHistoryCleanupOptions {
            max_age: DEFAULT_RUN_HISTORY_MAX_AGE,
            dry_run: true,
        };
        let future = SystemTime::now() + DEFAULT_RUN_HISTORY_MAX_AGE + Duration::from_secs(1);
        let preview = cleanup_run_history_at(&layout, &options, future).unwrap();
        assert_eq!(preview.candidates.len(), 1);
        assert_eq!(preview.candidates[0].run_id, orphan);
        assert_eq!(preview.bound_runs, 1);
        assert_eq!(preview.live_runs, 1);
        assert_eq!(preview.preserved_runs, 1);
        assert!(layout.run_directory(&orphan).exists());

        let removed = cleanup_run_history_at(
            &layout,
            &RunHistoryCleanupOptions {
                dry_run: false,
                ..options
            },
            future,
        )
        .unwrap();
        assert_eq!(removed.removed_runs, 1);
        assert!(!layout.run_directory(&orphan).exists());
        assert!(layout.run_directory(&binding.run_id).exists());
        assert!(layout.run_directory(&live).exists());
        assert!(layout.run_directory(&corrupt).exists());
        assert!(layout.root().join("unresolved-workspaces").exists());
    }
}
