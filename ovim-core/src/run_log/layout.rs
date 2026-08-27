use super::{RunId, RunLogError};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

pub const OVIM_RUNS_DIR_ENV: &str = "OVIM_RUNS_DIR";
const EVENT_DATABASE: &str = "events.sqlite3";
const ARTIFACT_DIRECTORY: &str = "artifacts";

/// Canonical on-disk locations for durable agent runs.
///
/// Run identifiers are encoded before becoming path components. Although IDs
/// are opaque strings, even an externally supplied ID containing separators
/// can therefore never escape the configured storage root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStorageLayout {
    root: PathBuf,
}

impl RunStorageLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the production location, honoring an explicit override first.
    pub fn discover() -> Result<Self, RunLogError> {
        Self::from_locations(std::env::var_os(OVIM_RUNS_DIR_ENV), dirs::data_local_dir())
    }

    /// Injectable form of [`Self::discover`], useful to callers that already
    /// have configuration and to tests that must not mutate process globals.
    pub fn from_locations(
        override_root: Option<OsString>,
        data_local_dir: Option<PathBuf>,
    ) -> Result<Self, RunLogError> {
        if let Some(root) = override_root.filter(|value| !value.is_empty()) {
            return Ok(Self::new(PathBuf::from(root)));
        }
        data_local_dir
            .map(|directory| Self::new(directory.join("ovim").join("runs")))
            .ok_or_else(|| RunLogError::Storage {
                operation: "resolve runs directory".into(),
                detail: "the platform does not provide a local data directory".into(),
            })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run_directory(&self, run_id: &RunId) -> PathBuf {
        self.root.join(encoded_run_component(run_id))
    }

    pub fn event_database(&self, run_id: &RunId) -> PathBuf {
        self.run_directory(run_id).join(EVENT_DATABASE)
    }

    pub fn artifact_directory(&self, run_id: &RunId) -> PathBuf {
        self.run_directory(run_id).join(ARTIFACT_DIRECTORY)
    }

    pub(crate) fn ensure_root(&self) -> Result<(), RunLogError> {
        ensure_private_directory(&self.root, "create runs directory")
    }

    pub(crate) fn ensure_run_directory(&self, run_id: &RunId) -> Result<PathBuf, RunLogError> {
        self.ensure_root()?;
        let directory = self.run_directory(run_id);
        ensure_private_directory(&directory, "create run directory")?;
        Ok(directory)
    }
}

pub(crate) fn encoded_run_component(run_id: &RunId) -> String {
    let mut encoded = String::with_capacity(4 + run_id.as_str().len() * 2);
    encoded.push_str("run-");
    for byte in run_id.as_str().as_bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) fn decoded_run_component(component: &std::ffi::OsStr) -> Option<RunId> {
    let encoded = component.to_str()?.strip_prefix("run-")?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    let value = String::from_utf8(bytes).ok()?;
    let run_id = RunId::parse(value).ok()?;
    (encoded_run_component(&run_id) == component.to_str()?).then_some(run_id)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn ensure_private_directory(path: &Path, operation: &str) -> Result<(), RunLogError> {
    fs::create_dir_all(path).map_err(|error| io_error(operation, path, error))?;
    let metadata = fs::metadata(path).map_err(|error| io_error(operation, path, error))?;
    if !metadata.is_dir() {
        return Err(RunLogError::Storage {
            operation: operation.into(),
            detail: format!("{} is not a directory", path.display()),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("set private directory permissions", path, error))?;
    }
    Ok(())
}

fn io_error(operation: &str, path: &Path, error: std::io::Error) -> RunLogError {
    RunLogError::Storage {
        operation: operation.into(),
        detail: format!("{}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_without_reading_or_mutating_the_environment() {
        let layout = RunStorageLayout::from_locations(
            Some(OsString::from("/explicit/runs")),
            Some(PathBuf::from("/platform/data")),
        )
        .unwrap();
        assert_eq!(layout.root(), Path::new("/explicit/runs"));
    }

    #[test]
    fn platform_default_is_namespaced_below_ovim() {
        let layout =
            RunStorageLayout::from_locations(None, Some(PathBuf::from("/platform/data"))).unwrap();
        assert_eq!(layout.root(), Path::new("/platform/data/ovim/runs"));
    }

    #[test]
    fn opaque_run_ids_cannot_become_path_traversal() {
        let layout = RunStorageLayout::new("/safe/root");
        let run_id = RunId::parse("run_../../outside").unwrap();
        let directory = layout.run_directory(&run_id);
        assert_eq!(directory.parent(), Some(Path::new("/safe/root")));
        assert!(!directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains('/'));
    }

    #[test]
    fn encoded_run_components_roundtrip_but_malformed_names_do_not() {
        let run_id = RunId::parse("run_value/with:opaque-data").unwrap();
        let encoded = encoded_run_component(&run_id);
        assert_eq!(decoded_run_component(encoded.as_ref()), Some(run_id));
        assert_eq!(decoded_run_component("run-xyz".as_ref()), None);
        assert_eq!(
            decoded_run_component("unresolved-workspaces".as_ref()),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn created_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let run_id = RunId::new();

        let run_directory = layout.ensure_run_directory(&run_id).unwrap();

        assert_eq!(
            fs::metadata(layout.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(run_directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
