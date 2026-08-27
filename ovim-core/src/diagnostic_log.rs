//! Bounded, private storage for disposable diagnostic logs.
//!
//! Ovim can have several TUI, GUI, and headless processes alive at once. They
//! intentionally share diagnostic files, so rotation by rename is unsafe: an
//! older process would keep writing to the renamed inode forever. This writer
//! instead locks and compacts the shared inode in place, retaining its newest
//! complete records.

use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const COMPACTION_MARKER: &[u8] = b"[ovim] older diagnostic records discarded at size limit\n";
const MINIMUM_MAX_BYTES: u64 = 4 * 1024;

pub(crate) struct DiagnosticLog {
    file: File,
    max_bytes: u64,
}

impl DiagnosticLog {
    pub(crate) fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "diagnostic log has no parent")
        })?;
        ensure_private_directory(parent)?;

        let file = private_open_options().open(path)?;
        make_private_file(path)?;
        Ok(Self {
            file,
            max_bytes: max_bytes.max(MINIMUM_MAX_BYTES),
        })
    }

    pub(crate) fn write_record(&mut self, record: &str) -> io::Result<()> {
        FileExt::lock_exclusive(&self.file)?;
        let result = self.write_record_locked(record);
        let unlock = FileExt::unlock(&self.file);
        result.and(unlock)
    }

    fn write_record_locked(&mut self, record: &str) -> io::Result<()> {
        let bounded = bounded_record(record, self.max_bytes / 2);
        let incoming = u64::try_from(bounded.len()).unwrap_or(u64::MAX);
        if self.file.metadata()?.len().saturating_add(incoming) > self.max_bytes {
            self.compact(incoming)?;
        }
        self.file.write_all(bounded.as_bytes())
    }

    fn compact(&mut self, incoming: u64) -> io::Result<()> {
        let marker_len = COMPACTION_MARKER.len() as u64;
        let available = self
            .max_bytes
            .saturating_sub(marker_len)
            .saturating_sub(incoming);
        let retain_bytes = available.min(self.max_bytes / 2);
        let retained = read_complete_tail(&mut self.file, retain_bytes)?;

        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(COMPACTION_MARKER)?;
        self.file.write_all(&retained)
    }
}

pub(crate) fn log_path(file_name: &str) -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::cache_dir)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("ovim").join(file_name)
}

fn read_complete_tail(file: &mut File, retain_bytes: u64) -> io::Result<Vec<u8>> {
    let len = file.metadata()?.len();
    if retain_bytes == 0 || len == 0 {
        return Ok(Vec::new());
    }
    let start = len.saturating_sub(retain_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.read_to_end(&mut tail)?;
    if start > 0 {
        if let Some(newline) = tail.iter().position(|byte| *byte == b'\n') {
            tail.drain(..=newline);
        } else {
            tail.clear();
        }
    }
    Ok(tail)
}

fn bounded_record(record: &str, max_bytes: u64) -> String {
    const NOTICE: &str = "\n[ovim] diagnostic record truncated\n";
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    if record.len() <= max_bytes {
        return record.to_owned();
    }
    let budget = max_bytes.saturating_sub(NOTICE.len());
    let mut end = budget.min(record.len());
    while end > 0 && !record.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = record[..end].to_owned();
    bounded.push_str(NOTICE);
    bounded
}

fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "diagnostic directory {} is not a real directory",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn make_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_keeps_the_file_bounded_and_retains_recent_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ovim.log");
        let mut log = DiagnosticLog::open(&path, MINIMUM_MAX_BYTES).unwrap();

        for index in 0..200 {
            log.write_record(&format!("record-{index:03} {}\n", "x".repeat(48)))
                .unwrap();
        }

        let bytes = fs::read(&path).unwrap();
        assert!(bytes.len() <= MINIMUM_MAX_BYTES as usize);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("older diagnostic records discarded"));
        assert!(text.contains("record-199"));
        assert!(!text.contains("record-000"));
    }

    #[test]
    fn oversized_unicode_record_is_truncated_on_a_character_boundary() {
        let bounded = bounded_record(&"å".repeat(4_000), MINIMUM_MAX_BYTES / 2);
        assert!(bounded.len() <= (MINIMUM_MAX_BYTES / 2) as usize);
        assert!(bounded.ends_with("diagnostic record truncated\n"));
    }

    #[cfg(unix)]
    #[test]
    fn storage_is_owner_only_even_when_existing_modes_are_permissive() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let log_dir = directory.path().join("cache").join("ovim");
        fs::create_dir_all(&log_dir).unwrap();
        let path = log_dir.join("ovim.log");
        fs::write(&path, b"old\n").unwrap();
        fs::set_permissions(&log_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let _log = DiagnosticLog::open(&path, MINIMUM_MAX_BYTES).unwrap();

        assert_eq!(
            fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
