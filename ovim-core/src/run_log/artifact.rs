use super::{
    ArtifactAvailability, ArtifactId, ArtifactRef, ContentRepresentation, EventId, OperationId,
    WorkspaceSurface,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

const SHA256_PREFIX: &str = "sha256:";
const COPY_BUFFER_SIZE: usize = 64 * 1024;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// The SHA-256 identity of artifact bytes.
///
/// Its stable wire representation is `sha256:` followed by 64 lowercase hex
/// digits. Blob identity deliberately does not include artifact metadata.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId([u8; 32]);

impl BlobId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn digest(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = String::with_capacity(64);
        for byte in self.0 {
            result.push(HEX[(byte >> 4) as usize] as char);
            result.push(HEX[(byte & 0x0f) as usize] as char);
        }
        result
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{SHA256_PREFIX}{}", self.hex())
    }
}

impl FromStr for BlobId {
    type Err = InvalidBlobId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix(SHA256_PREFIX)
            .ok_or(InvalidBlobId::MissingAlgorithm)?;
        if hex.len() != 64 {
            return Err(InvalidBlobId::InvalidLength(hex.len()));
        }

        let mut bytes = [0; 32];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex_digit(pair[0]).ok_or(InvalidBlobId::InvalidHex)? << 4)
                | hex_digit(pair[1]).ok_or(InvalidBlobId::InvalidHex)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for BlobId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BlobId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvalidBlobId {
    MissingAlgorithm,
    InvalidLength(usize),
    InvalidHex,
}

impl fmt::Display for InvalidBlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAlgorithm => formatter.write_str("blob ID must start with `sha256:`"),
            Self::InvalidLength(length) => {
                write!(
                    formatter,
                    "SHA-256 blob ID has {length} hex digits, expected 64"
                )
            }
            Self::InvalidHex => formatter.write_str("SHA-256 blob ID contains non-lowercase hex"),
        }
    }
}

impl std::error::Error for InvalidBlobId {}

/// Durable metadata for a logical artifact. Multiple records may intentionally
/// refer to the same blob while retaining different provenance or policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub artifact_id: ArtifactId,
    pub state: ArtifactState,
    pub source: ArtifactSource,
    pub representation: ContentRepresentation,
    /// An optional IANA media type when it is known independently of the
    /// content representation (for example, `image/png` for raw bytes).
    pub media_type: Option<String>,
    pub retention: ArtifactRetention,
    pub export_policy: ArtifactExportPolicy,
}

impl ArtifactRecord {
    pub fn as_ref(&self) -> ArtifactRef {
        ArtifactRef {
            artifact_id: self.artifact_id.clone(),
            // ArtifactRef predates policy-aware artifact states. Keep this a
            // deliberately lossy UI/event projection: excluded content was
            // never retained and therefore behaves as unavailable there.
            availability: match &self.state {
                ArtifactState::Available { .. } => ArtifactAvailability::Available,
                ArtifactState::Missing { .. } | ArtifactState::Excluded { .. } => {
                    ArtifactAvailability::Missing
                }
                ArtifactState::Redacted { .. } => ArtifactAvailability::Redacted,
            },
            representation: self.representation.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactState {
    Available {
        blob_id: BlobId,
        byte_len: u64,
    },
    Missing {
        reason: String,
    },
    /// Content intentionally not captured by storage/export policy.
    Excluded {
        reason: String,
    },
    Redacted {
        reason: String,
        /// Preserves local identity when a known blob is hidden during export.
        original: Option<BlobId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactSource {
    Workspace {
        path: String,
        surface: WorkspaceSurface,
    },
    ToolResult {
        operation_id: OperationId,
    },
    Message {
        event_id: EventId,
    },
    AgentHandoff {
        name: String,
    },
    Imported {
        label: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetention {
    Ephemeral,
    Run,
    Retained,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactExportPolicy {
    Include,
    Omit,
    Redact,
}

/// A filesystem-backed, content-addressed blob store.
///
/// Readers can only address final hash paths. Temporary files therefore remain
/// invisible after crashes, and concurrent writers of identical content
/// converge on the same path.
#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ArtifactStoreError> {
        let root = root.into();
        create_private_dir(&root)?;
        create_private_dir(&root.join("staging"))?;
        create_private_dir(&root.join("blobs").join("sha256"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<StoredBlob, ArtifactStoreError> {
        self.put_reader(bytes)
    }

    /// Streams bytes to private storage while hashing them. Publication occurs
    /// only after the complete file has been flushed and synced.
    pub fn put_reader(&self, mut reader: impl Read) -> Result<StoredBlob, ArtifactStoreError> {
        let (staging_path, mut staging) = self.create_staging_file()?;
        let write_result = stream_to_file(&mut reader, &mut staging);
        let (blob_id, byte_len) = match write_result {
            Ok(result) => result,
            Err(error) => {
                drop(staging);
                let _ = fs::remove_file(&staging_path);
                return Err(error.into());
            }
        };
        staging.flush()?;
        staging.sync_all()?;
        drop(staging);

        let destination = self.blob_path(blob_id);
        let shard = destination.parent().expect("blob path always has a parent");
        create_private_dir(shard)?;
        let final_temp = unique_temp_path(shard);
        if let Err(error) = fs::rename(&staging_path, &final_temp) {
            let _ = fs::remove_file(&staging_path);
            return Err(error.into());
        }

        let publication = self.publish(&final_temp, &destination, blob_id, byte_len);
        let _ = fs::remove_file(&final_temp);
        publication?;

        Ok(StoredBlob { blob_id, byte_len })
    }

    /// Reads and verifies a blob. A hash-path containing different bytes is
    /// reported as corruption rather than returned to callers.
    pub fn read(&self, blob_id: BlobId) -> Result<Vec<u8>, ArtifactStoreError> {
        let path = self.blob_path(blob_id);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ArtifactStoreError::NotFound(blob_id))
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let actual = BlobId::digest(&bytes);
        if actual != blob_id {
            return Err(ArtifactStoreError::CorruptBlob {
                expected: blob_id,
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn contains(&self, blob_id: BlobId) -> Result<bool, ArtifactStoreError> {
        match self.read(blob_id) {
            Ok(_) => Ok(true),
            Err(ArtifactStoreError::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn create_staging_file(&self) -> Result<(PathBuf, File), ArtifactStoreError> {
        let staging = self.root.join("staging");
        for _ in 0..100 {
            let path = unique_temp_path(&staging);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ArtifactStoreError::TemporaryNameExhausted)
    }

    fn publish(
        &self,
        temp: &Path,
        destination: &Path,
        expected: BlobId,
        byte_len: u64,
    ) -> Result<(), ArtifactStoreError> {
        // A hard link is an atomic create-if-absent publication on the same
        // filesystem. Unlike a plain rename, it cannot replace a corrupt blob
        // between an existence check and publication.
        match fs::hard_link(temp, destination) {
            Ok(()) => {
                sync_parent(destination)?;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_file(destination, expected, Some(byte_len))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn blob_path(&self, blob_id: BlobId) -> PathBuf {
        let hex = blob_id.hex();
        self.root
            .join("blobs")
            .join("sha256")
            .join(&hex[..2])
            .join(&hex[2..])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredBlob {
    pub blob_id: BlobId,
    pub byte_len: u64,
}

#[derive(Debug)]
pub enum ArtifactStoreError {
    Io(io::Error),
    NotFound(BlobId),
    CorruptBlob { expected: BlobId, actual: BlobId },
    TemporaryNameExhausted,
}

impl fmt::Display for ArtifactStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "artifact store I/O failed: {error}"),
            Self::NotFound(blob_id) => write!(formatter, "artifact blob {blob_id} was not found"),
            Self::CorruptBlob { expected, actual } => write!(
                formatter,
                "artifact blob at {expected} contains bytes hashing to {actual}"
            ),
            Self::TemporaryNameExhausted => {
                formatter.write_str("could not allocate a unique artifact temporary file")
            }
        }
    }
}

impl std::error::Error for ArtifactStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ArtifactStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn stream_to_file(reader: &mut impl Read, destination: &mut File) -> io::Result<(BlobId, u64)> {
    let mut hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut buffer = [0; COPY_BUFFER_SIZE];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        byte_len = byte_len
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "artifact is too large"))?;
    }
    Ok((BlobId(hasher.finalize().into()), byte_len))
}

fn verify_file(
    path: &Path,
    expected: BlobId,
    expected_len: Option<u64>,
) -> Result<(), ArtifactStoreError> {
    let mut file = File::open(path)?;
    if expected_len.is_some_and(|length| {
        file.metadata()
            .map(|metadata| metadata.len() != length)
            .unwrap_or(true)
    }) {
        let actual = hash_reader(&mut file)?;
        return Err(ArtifactStoreError::CorruptBlob { expected, actual });
    }
    let actual = hash_reader(&mut file)?;
    if actual != expected {
        return Err(ArtifactStoreError::CorruptBlob { expected, actual });
    }
    Ok(())
}

fn hash_reader(reader: &mut impl Read) -> io::Result<BlobId> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; COPY_BUFFER_SIZE];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(BlobId(hasher.finalize().into()))
}

fn unique_temp_path(parent: &Path) -> PathBuf {
    let counter = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".artifact-{}-{counter:016x}.tmp",
        std::process::id()
    ))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn sha256_abc_matches_the_golden_wire_id() {
        let id = BlobId::digest(b"abc");
        assert_eq!(
            id.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(id.to_string().parse::<BlobId>().unwrap(), id);
        assert!(id.to_string().to_uppercase().parse::<BlobId>().is_err());
    }

    #[test]
    fn fault_local_artifact_states_round_trip_without_collapsing() {
        let missing = ArtifactState::Missing {
            reason: "artifact directory was removed".into(),
        };
        let excluded = ArtifactState::Excluded {
            reason: "matched local secret policy".into(),
        };

        for state in [missing, excluded] {
            let json = serde_json::to_value(&state).unwrap();
            assert_eq!(
                serde_json::from_value::<ArtifactState>(json).unwrap(),
                state
            );
        }
    }

    #[test]
    fn identical_content_is_deduplicated() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let first = store.put_bytes(b"same bytes").unwrap();
        let second = store.put_bytes(b"same bytes").unwrap();

        assert_eq!(first, second);
        assert_eq!(store.read(first.blob_id).unwrap(), b"same bytes");
    }

    #[test]
    fn concurrent_identical_writers_converge() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::open(directory.path()).unwrap());
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store.put_bytes(b"concurrent content").unwrap()
                })
            })
            .collect();

        let blobs: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(blobs.iter().all(|blob| *blob == blobs[0]));
        assert_eq!(store.read(blobs[0].blob_id).unwrap(), b"concurrent content");
    }

    #[test]
    fn refuses_to_overwrite_corrupt_existing_blob() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let expected = BlobId::digest(b"expected");
        let destination = store.blob_path(expected);
        create_private_dir(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"corrupt!").unwrap();

        let error = store.put_bytes(b"expected").unwrap_err();
        assert!(matches!(
            error,
            ArtifactStoreError::CorruptBlob {
                expected: found,
                ..
            } if found == expected
        ));
        assert_eq!(fs::read(destination).unwrap(), b"corrupt!");
    }

    #[test]
    fn orphan_temporary_files_are_never_addressable() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let expected = BlobId::digest(b"unfinished");
        let shard = store.blob_path(expected).parent().unwrap().to_owned();
        create_private_dir(&shard).unwrap();
        fs::write(unique_temp_path(&shard), b"unfinished").unwrap();

        assert!(!store.contains(expected).unwrap());
    }
}
