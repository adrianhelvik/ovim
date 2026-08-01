use super::{
    AgentId, BranchId, ConversationId, RepositoryId, RunId, RunLogError, RunStorageLayout,
    WorkspaceId,
};
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CATALOG_DATABASE: &str = "index.sqlite";
const LATEST_MIGRATION: u32 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub trait CatalogClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

/// Stable catalog identity for a user-approved folder that is not a Git
/// repository. The folder root is an authorization boundary supplied by the
/// caller, not a path discovered from the process working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedFolderRecord {
    pub repository_id: RepositoryId,
    pub root: PathBuf,
}

#[derive(Default)]
pub struct SystemCatalogClock;

impl CatalogClock for SystemCatalogClock {
    fn now_millis(&self) -> i64 {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        i64::try_from(millis).unwrap_or(i64::MAX)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryRegistration {
    /// Canonical Git common directory. Linked worktrees of one repository
    /// share this value; independent clones do not, even with the same remote.
    pub common_git_dir: PathBuf,
    /// Canonical worktree roots and other caller-established aliases.
    pub worktree_aliases: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryRecord {
    pub repository_id: RepositoryId,
    pub common_git_dir: PathBuf,
    pub worktree_aliases: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConversationScope {
    NoFile,
    RepositoryPath(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConversationKey {
    pub repository_id: RepositoryId,
    pub scope: ConversationScope,
    pub logical_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationBinding {
    pub key: ConversationKey,
    pub conversation_id: ConversationId,
    pub run_id: RunId,
    pub root_agent_id: AgentId,
    pub workspace_id: WorkspaceId,
    pub selected_branch_id: BranchId,
}

/// Outcome of starting a fresh run without displacing prior history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreshConversationStart {
    /// No conversation existed for the key; the fresh run is now the one
    /// discovered by future opens.
    Bound(ConversationBinding),
    /// A prior conversation exists and remains discoverable; the fresh run is
    /// visible only to the process that created it.
    Unbound(ConversationBinding),
}

impl FreshConversationStart {
    pub fn is_bound(&self) -> bool {
        matches!(self, Self::Bound(_))
    }

    pub fn into_binding(self) -> ConversationBinding {
        match self {
            Self::Bound(binding) | Self::Unbound(binding) => binding,
        }
    }
}

/// A process that registered itself as a writer of a run. The PID plus the
/// process start time mirror the stale-session detection in `crate::session`:
/// a reused PID with a different start time is not mistaken for a live owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOwner {
    pub pid: u32,
    pub process_start_time: Option<u64>,
    pub registered_at_millis: i64,
}

/// Opaque coordinates for provider-owned resumable state.
///
/// These coordinates refer to ovim identities, but the provider thread and
/// turn identifiers stored beneath them remain unparsed provider data. They
/// must never be used as substitutes for an [`AgentId`] or [`BranchId`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderSessionKey {
    pub provider: String,
    pub agent_id: AgentId,
    pub branch_id: BranchId,
}

/// Versioned digest of every configuration input that affects whether a
/// provider session can be safely resumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfigurationFingerprint {
    pub version: u32,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSession {
    pub key: ProviderSessionKey,
    /// Provider-owned opaque identifier; it is not an ovim identity.
    pub provider_thread_id: String,
    pub configuration_fingerprint: ProviderConfigurationFingerprint,
    /// Provider-owned opaque identifier for the latest known turn.
    pub last_provider_turn_id: Option<String>,
    pub updated_at_millis: i64,
}

#[derive(Debug)]
pub enum CatalogError {
    RunLog(RunLogError),
    InvalidRepositoryPath(String),
    InvalidConversationKey(String),
    RepositoryAliasConflict,
    UnknownRepository(RepositoryId),
    InvalidProviderSession(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunLog(error) => write!(formatter, "catalog storage: {error}"),
            Self::InvalidRepositoryPath(detail) => {
                write!(formatter, "invalid repository path: {detail}")
            }
            Self::InvalidConversationKey(detail) => {
                write!(formatter, "invalid conversation key: {detail}")
            }
            Self::RepositoryAliasConflict => {
                formatter.write_str("repository aliases identify different repositories")
            }
            Self::UnknownRepository(id) => write!(formatter, "unknown repository {id}"),
            Self::InvalidProviderSession(detail) => {
                write!(formatter, "invalid provider session: {detail}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<RunLogError> for CatalogError {
    fn from(value: RunLogError) -> Self {
        Self::RunLog(value)
    }
}

pub struct RunCatalog {
    connection: Mutex<Connection>,
    clock: Arc<dyn CatalogClock>,
}

impl RunCatalog {
    pub fn open(layout: &RunStorageLayout) -> Result<Self, CatalogError> {
        Self::open_with_clock(layout, Arc::new(SystemCatalogClock))
    }

    pub fn open_with_clock(
        layout: &RunStorageLayout,
        clock: Arc<dyn CatalogClock>,
    ) -> Result<Self, CatalogError> {
        layout.ensure_root()?;
        let path = layout.root().join(CATALOG_DATABASE);
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| storage("open catalog", error))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|error| storage("set catalog busy timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| storage("enable catalog foreign keys", error))?;
        configure_wal(&connection)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|error| storage("enable catalog full synchronization", error))?;
        migrate(&mut connection)?;
        set_owner_only(&path)?;
        Ok(Self {
            connection: Mutex::new(connection),
            clock,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, CatalogError> {
        self.connection
            .lock()
            .map_err(|_| CatalogError::RunLog(RunLogError::Poisoned))
    }

    pub fn register_repository(
        &self,
        registration: RepositoryRegistration,
    ) -> Result<RepositoryRecord, CatalogError> {
        let common = path_key(&registration.common_git_dir)?;
        let mut aliases = registration
            .worktree_aliases
            .iter()
            .map(|path| path_key(path))
            .collect::<Result<Vec<_>, _>>()?;
        aliases.sort();
        aliases.dedup();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin repository registration", error))?;

        let mut candidates = Vec::new();
        if let Some(id) = transaction
            .query_row(
                "SELECT repository_id FROM repositories WHERE common_git_dir = ?1",
                [&common],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| storage("resolve repository common directory", error))?
        {
            candidates.push(id);
        }
        for alias in &aliases {
            if let Some(id) = transaction
                .query_row(
                    "SELECT repository_id FROM repository_aliases WHERE alias_path = ?1",
                    [alias],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| storage("resolve repository alias", error))?
            {
                candidates.push(id);
            }
        }
        candidates.sort();
        candidates.dedup();
        if candidates.len() > 1 {
            return Err(CatalogError::RepositoryAliasConflict);
        }
        let repository_id = match candidates.pop() {
            Some(value) => parse_repository_id(value)?,
            None => {
                let id = RepositoryId::new();
                transaction
                    .execute(
                        "INSERT INTO repositories(repository_id, common_git_dir, created_at_millis) VALUES (?1, ?2, ?3)",
                        params![id.as_str(), common, self.clock.now_millis()],
                    )
                    .map_err(|error| storage("insert repository", error))?;
                id
            }
        };
        for alias in &aliases {
            transaction
                .execute(
                    "INSERT INTO repository_aliases(alias_path, repository_id) VALUES (?1, ?2) \
                     ON CONFLICT(alias_path) DO UPDATE SET repository_id = excluded.repository_id \
                     WHERE repository_aliases.repository_id = excluded.repository_id",
                    params![alias, repository_id.as_str()],
                )
                .map_err(|error| storage("register repository alias", error))?;
        }
        transaction
            .commit()
            .map_err(|error| storage("commit repository registration", error))?;
        drop(connection);
        self.repository(&repository_id)?.ok_or_else(|| {
            CatalogError::RunLog(RunLogError::Corruption {
                detail: format!("registered repository {repository_id} disappeared"),
            })
        })
    }

    /// Registers a canonical, explicitly approved non-Git folder as a durable
    /// conversation anchor. Its repository identity remains stable across
    /// processes without claiming that the folder contains Git metadata.
    pub fn register_approved_folder(
        &self,
        root: &Path,
    ) -> Result<ApprovedFolderRecord, CatalogError> {
        let root = path_key(root)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin approved folder registration", error))?;

        let existing = transaction
            .query_row(
                "SELECT repository_id FROM approved_folder_roots WHERE root_path = ?1",
                [&root],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| storage("resolve approved folder", error))?;
        let repository_id = match existing {
            Some(value) => parse_repository_id(value)?,
            None => {
                let id = RepositoryId::new();
                // Valid Git registrations always use canonical absolute paths.
                // This reserved relative marker therefore cannot collide with
                // a real common Git directory, including one created later.
                let marker = format!("approved-folder/{}", id.as_str());
                transaction
                    .execute(
                        "INSERT INTO repositories(repository_id, common_git_dir, created_at_millis) VALUES (?1, ?2, ?3)",
                        params![id.as_str(), marker.as_bytes(), self.clock.now_millis()],
                    )
                    .map_err(|error| storage("insert approved folder identity", error))?;
                transaction
                    .execute(
                        "INSERT INTO approved_folder_roots(root_path, repository_id) VALUES (?1, ?2)",
                        params![root, id.as_str()],
                    )
                    .map_err(|error| storage("insert approved folder root", error))?;
                id
            }
        };
        transaction
            .commit()
            .map_err(|error| storage("commit approved folder registration", error))?;
        Ok(ApprovedFolderRecord {
            repository_id,
            root: bytes_path(root),
        })
    }

    pub fn repository(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Option<RepositoryRecord>, CatalogError> {
        let connection = self.connection()?;
        let common: Option<Vec<u8>> = connection
            .query_row(
                "SELECT common_git_dir FROM repositories WHERE repository_id = ?1",
                [repository_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| storage("read repository", error))?;
        let Some(common) = common else {
            return Ok(None);
        };
        let mut statement = connection
            .prepare("SELECT alias_path FROM repository_aliases WHERE repository_id = ?1 ORDER BY alias_path")
            .map_err(|error| storage("prepare repository alias read", error))?;
        let aliases = statement
            .query_map([repository_id.as_str()], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|error| storage("read repository aliases", error))?
            .map(|row| {
                row.map(bytes_path)
                    .map_err(|error| storage("decode repository alias", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(RepositoryRecord {
            repository_id: repository_id.clone(),
            common_git_dir: bytes_path(common),
            worktree_aliases: aliases,
        }))
    }

    pub fn open_conversation(
        &self,
        key: ConversationKey,
        initial_branch_id: BranchId,
    ) -> Result<ConversationBinding, CatalogError> {
        let (scope_kind, scope_path) = normalized_scope(&key.scope)?;
        if key.logical_name.trim().is_empty() {
            return Err(CatalogError::InvalidConversationKey(
                "logical name is empty".into(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin conversation open", error))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repositories WHERE repository_id = ?1)",
                [key.repository_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| storage("validate conversation repository", error))?;
        if !exists {
            return Err(CatalogError::UnknownRepository(key.repository_id));
        }
        let existing = read_binding(&transaction, &key, scope_kind, &scope_path)?;
        let binding = match existing {
            Some(binding) => binding,
            None => {
                let binding = ConversationBinding {
                    key: key.clone(),
                    conversation_id: ConversationId::new(),
                    run_id: RunId::new(),
                    root_agent_id: AgentId::new(),
                    workspace_id: WorkspaceId::new(),
                    selected_branch_id: initial_branch_id,
                };
                transaction
                    .execute(
                        "INSERT INTO conversation_bindings(\
                         repository_id, scope_kind, scope_path, logical_name, conversation_id, \
                         run_id, root_agent_id, workspace_id, selected_branch_id, updated_at_millis) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            binding.key.repository_id.as_str(), scope_kind, scope_path,
                            binding.key.logical_name, binding.conversation_id.as_str(),
                            binding.run_id.as_str(), binding.root_agent_id.as_str(),
                            binding.workspace_id.as_str(), binding.selected_branch_id.as_str(),
                            self.clock.now_millis(),
                        ],
                    )
                    .map_err(|error| storage("insert conversation binding", error))?;
                binding
            }
        };
        transaction
            .commit()
            .map_err(|error| storage("commit conversation open", error))?;
        Ok(binding)
    }

    /// Creates an independent run and makes it the conversation discovered by
    /// future opens. Existing editors retain their own immutable binding and
    /// can continue appending to their run even if another process calls this.
    pub fn start_conversation(
        &self,
        key: ConversationKey,
        initial_branch_id: BranchId,
    ) -> Result<ConversationBinding, CatalogError> {
        let (scope_kind, scope_path) = normalized_scope(&key.scope)?;
        if key.logical_name.trim().is_empty() {
            return Err(CatalogError::InvalidConversationKey(
                "logical name is empty".into(),
            ));
        }
        let now = self.clock.now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin conversation start", error))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repositories WHERE repository_id = ?1)",
                [key.repository_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| storage("validate conversation repository", error))?;
        if !exists {
            return Err(CatalogError::UnknownRepository(key.repository_id));
        }

        let fresh = ConversationBinding {
            key,
            conversation_id: ConversationId::new(),
            run_id: RunId::new(),
            root_agent_id: AgentId::new(),
            workspace_id: WorkspaceId::new(),
            selected_branch_id: initial_branch_id,
        };
        transaction
            .execute(
                "INSERT INTO conversation_bindings(\
                 repository_id, scope_kind, scope_path, logical_name, conversation_id, run_id, \
                 root_agent_id, workspace_id, selected_branch_id, updated_at_millis) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(repository_id, scope_kind, scope_path, logical_name) DO UPDATE SET \
                 conversation_id=excluded.conversation_id, run_id=excluded.run_id, \
                 root_agent_id=excluded.root_agent_id, workspace_id=excluded.workspace_id, \
                 selected_branch_id=excluded.selected_branch_id, \
                 updated_at_millis=excluded.updated_at_millis",
                params![
                    fresh.key.repository_id.as_str(),
                    scope_kind,
                    scope_path,
                    fresh.key.logical_name,
                    fresh.conversation_id.as_str(),
                    fresh.run_id.as_str(),
                    fresh.root_agent_id.as_str(),
                    fresh.workspace_id.as_str(),
                    fresh.selected_branch_id.as_str(),
                    now,
                ],
            )
            .map_err(|error| storage("store fresh conversation binding", error))?;
        transaction
            .commit()
            .map_err(|error| storage("commit conversation start", error))?;
        Ok(fresh)
    }

    /// Creates an independent run for the key without ever displacing an
    /// existing conversation. When no conversation exists the fresh run is
    /// bound and becomes discoverable by future opens; otherwise the prior
    /// binding — and therefore its history — stays reachable through
    /// [`Self::open_conversation`], and the fresh run remains unbound.
    pub fn start_fresh_conversation(
        &self,
        key: ConversationKey,
        initial_branch_id: BranchId,
    ) -> Result<FreshConversationStart, CatalogError> {
        let (scope_kind, scope_path) = normalized_scope(&key.scope)?;
        if key.logical_name.trim().is_empty() {
            return Err(CatalogError::InvalidConversationKey(
                "logical name is empty".into(),
            ));
        }
        let now = self.clock.now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin fresh conversation start", error))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repositories WHERE repository_id = ?1)",
                [key.repository_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| storage("validate conversation repository", error))?;
        if !exists {
            return Err(CatalogError::UnknownRepository(key.repository_id));
        }

        let fresh = ConversationBinding {
            key,
            conversation_id: ConversationId::new(),
            run_id: RunId::new(),
            root_agent_id: AgentId::new(),
            workspace_id: WorkspaceId::new(),
            selected_branch_id: initial_branch_id,
        };
        let inserted = transaction
            .execute(
                "INSERT INTO conversation_bindings(\
                 repository_id, scope_kind, scope_path, logical_name, conversation_id, run_id, \
                 root_agent_id, workspace_id, selected_branch_id, updated_at_millis) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(repository_id, scope_kind, scope_path, logical_name) DO NOTHING",
                params![
                    fresh.key.repository_id.as_str(),
                    scope_kind,
                    scope_path,
                    fresh.key.logical_name,
                    fresh.conversation_id.as_str(),
                    fresh.run_id.as_str(),
                    fresh.root_agent_id.as_str(),
                    fresh.workspace_id.as_str(),
                    fresh.selected_branch_id.as_str(),
                    now,
                ],
            )
            .map_err(|error| storage("store fresh unbound conversation", error))?;
        transaction
            .commit()
            .map_err(|error| storage("commit fresh conversation start", error))?;
        Ok(if inserted == 1 {
            FreshConversationStart::Bound(fresh)
        } else {
            FreshConversationStart::Unbound(fresh)
        })
    }

    /// Records the calling process as a live writer of the run so a later
    /// crash recovery can prove whether any writer is still alive.
    pub fn register_run_owner(
        &self,
        run_id: &RunId,
        pid: u32,
        process_start_time: Option<u64>,
    ) -> Result<(), CatalogError> {
        let now = self.clock.now_millis();
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO run_owners(run_id, pid, process_start_time, registered_at_millis) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(run_id, pid) DO UPDATE SET \
                 process_start_time=excluded.process_start_time, \
                 registered_at_millis=excluded.registered_at_millis",
                params![
                    run_id.as_str(),
                    i64::from(pid),
                    process_start_time.map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
                    now,
                ],
            )
            .map_err(|error| storage("register run owner", error))?;
        Ok(())
    }

    /// Every process that registered itself as a writer of the run. An empty
    /// result means ownership was never recorded (for example a store written
    /// before owner tracking existed) and liveness cannot be proven.
    pub fn run_owners(&self, run_id: &RunId) -> Result<Vec<RunOwner>, CatalogError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT pid, process_start_time, registered_at_millis FROM run_owners \
                 WHERE run_id = ?1 ORDER BY pid",
            )
            .map_err(|error| storage("prepare run owner read", error))?;
        let owners = statement
            .query_map([run_id.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| storage("read run owners", error))?
            .map(|row| {
                row.map_err(|error| storage("decode run owner", error)).map(
                    |(pid, start, registered)| RunOwner {
                        pid: u32::try_from(pid).unwrap_or(u32::MAX),
                        process_start_time: start.and_then(|value| u64::try_from(value).ok()),
                        registered_at_millis: registered,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(owners)
    }

    pub fn update_selected_branch(
        &self,
        binding: &ConversationBinding,
        branch_id: BranchId,
    ) -> Result<bool, CatalogError> {
        let (scope_kind, scope_path) = normalized_scope(&binding.key.scope)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin selected branch update", error))?;
        let changed = transaction
            .execute(
                "UPDATE conversation_bindings SET selected_branch_id = ?6, updated_at_millis = ?7 \
                 WHERE repository_id = ?1 AND scope_kind = ?2 AND scope_path = ?3 \
                 AND logical_name = ?4 AND run_id = ?5",
                params![
                    binding.key.repository_id.as_str(),
                    scope_kind,
                    scope_path,
                    binding.key.logical_name,
                    binding.run_id.as_str(),
                    branch_id.as_str(),
                    self.clock.now_millis()
                ],
            )
            .map_err(|error| storage("update selected branch", error))?;
        transaction
            .commit()
            .map_err(|error| storage("commit selected branch update", error))?;
        Ok(changed == 1)
    }

    /// Inserts or replaces provider-owned resume state for one ovim agent
    /// branch. The complete record is written atomically.
    pub fn upsert_provider_session(
        &self,
        key: ProviderSessionKey,
        provider_thread_id: String,
        configuration_fingerprint: ProviderConfigurationFingerprint,
        last_provider_turn_id: Option<String>,
    ) -> Result<ProviderSession, CatalogError> {
        validate_provider_session(
            &key,
            &provider_thread_id,
            &configuration_fingerprint,
            last_provider_turn_id.as_deref(),
        )?;
        let updated_at_millis = self.clock.now_millis();
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO provider_sessions(
                 provider, agent_id, branch_id, provider_thread_id, fingerprint_version,
                 fingerprint_value, last_provider_turn_id, updated_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(provider, agent_id, branch_id) DO UPDATE SET
                 provider_thread_id=excluded.provider_thread_id,
                 fingerprint_version=excluded.fingerprint_version,
                 fingerprint_value=excluded.fingerprint_value,
                 last_provider_turn_id=excluded.last_provider_turn_id,
                 updated_at_millis=excluded.updated_at_millis",
                params![
                    key.provider,
                    key.agent_id.as_str(),
                    key.branch_id.as_str(),
                    provider_thread_id,
                    configuration_fingerprint.version,
                    configuration_fingerprint.value,
                    last_provider_turn_id,
                    updated_at_millis,
                ],
            )
            .map_err(|error| storage("upsert provider session", error))?;
        Ok(ProviderSession {
            key,
            provider_thread_id,
            configuration_fingerprint,
            last_provider_turn_id,
            updated_at_millis,
        })
    }

    /// Returns provider state only when its versioned configuration
    /// fingerprint exactly equals the expected fingerprint.
    pub fn provider_session(
        &self,
        key: &ProviderSessionKey,
        expected_fingerprint: &ProviderConfigurationFingerprint,
    ) -> Result<Option<ProviderSession>, CatalogError> {
        validate_provider_key(key)?;
        validate_provider_fingerprint(expected_fingerprint)?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT provider_thread_id, fingerprint_version, fingerprint_value,
                 last_provider_turn_id, updated_at_millis FROM provider_sessions
                 WHERE provider=?1 AND agent_id=?2 AND branch_id=?3
                 AND fingerprint_version=?4 AND fingerprint_value=?5",
                params![
                    key.provider,
                    key.agent_id.as_str(),
                    key.branch_id.as_str(),
                    expected_fingerprint.version,
                    expected_fingerprint.value,
                ],
                |row| {
                    Ok(ProviderSession {
                        key: key.clone(),
                        provider_thread_id: row.get(0)?,
                        configuration_fingerprint: ProviderConfigurationFingerprint {
                            version: row.get(1)?,
                            value: row.get(2)?,
                        },
                        last_provider_turn_id: row.get(3)?,
                        updated_at_millis: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|error| storage("read provider session", error))
    }

    pub fn delete_provider_session(&self, key: &ProviderSessionKey) -> Result<bool, CatalogError> {
        validate_provider_key(key)?;
        let connection = self.connection()?;
        Ok(connection
            .execute(
                "DELETE FROM provider_sessions WHERE provider=?1 AND agent_id=?2 AND branch_id=?3",
                params![key.provider, key.agent_id.as_str(), key.branch_id.as_str()],
            )
            .map_err(|error| storage("delete provider session", error))?
            == 1)
    }
}

fn validate_provider_key(key: &ProviderSessionKey) -> Result<(), CatalogError> {
    if key.provider.trim().is_empty() {
        Err(CatalogError::InvalidProviderSession(
            "provider name is empty".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_provider_fingerprint(
    fingerprint: &ProviderConfigurationFingerprint,
) -> Result<(), CatalogError> {
    if fingerprint.version == 0 {
        return Err(CatalogError::InvalidProviderSession(
            "configuration fingerprint version is zero".into(),
        ));
    }
    if fingerprint.value.trim().is_empty() {
        return Err(CatalogError::InvalidProviderSession(
            "configuration fingerprint is empty".into(),
        ));
    }
    Ok(())
}

fn validate_provider_session(
    key: &ProviderSessionKey,
    provider_thread_id: &str,
    fingerprint: &ProviderConfigurationFingerprint,
    last_provider_turn_id: Option<&str>,
) -> Result<(), CatalogError> {
    validate_provider_key(key)?;
    if provider_thread_id.trim().is_empty() {
        return Err(CatalogError::InvalidProviderSession(
            "provider thread id is empty".into(),
        ));
    }
    validate_provider_fingerprint(fingerprint)?;
    if last_provider_turn_id.is_some_and(|turn_id| turn_id.trim().is_empty()) {
        return Err(CatalogError::InvalidProviderSession(
            "last provider turn id is empty".into(),
        ));
    }
    Ok(())
}

fn read_binding(
    connection: &Connection,
    key: &ConversationKey,
    scope_kind: i64,
    scope_path: &[u8],
) -> Result<Option<ConversationBinding>, CatalogError> {
    let row: Option<(String, String, String, String, String)> = connection
        .query_row(
            "SELECT conversation_id, run_id, root_agent_id, workspace_id, selected_branch_id \
             FROM conversation_bindings WHERE repository_id=?1 AND scope_kind=?2 AND scope_path=?3 AND logical_name=?4",
            params![key.repository_id.as_str(), scope_kind, scope_path, key.logical_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|error| storage("read conversation binding", error))?;
    row.map(|(conversation, run, agent, workspace, branch)| {
        Ok(ConversationBinding {
            key: key.clone(),
            conversation_id: ConversationId::parse(conversation).map_err(corrupt_id)?,
            run_id: RunId::parse(run).map_err(corrupt_id)?,
            root_agent_id: AgentId::parse(agent).map_err(corrupt_id)?,
            workspace_id: WorkspaceId::parse(workspace).map_err(corrupt_id)?,
            selected_branch_id: BranchId::parse(branch).map_err(corrupt_id)?,
        })
    })
    .transpose()
}

fn normalized_scope(scope: &ConversationScope) -> Result<(i64, Vec<u8>), CatalogError> {
    match scope {
        ConversationScope::NoFile => Ok((0, Vec::new())),
        ConversationScope::RepositoryPath(path) => {
            if path.as_os_str().is_empty() || path.is_absolute() {
                return Err(CatalogError::InvalidConversationKey(
                    "repository path must be non-empty and relative".into(),
                ));
            }
            let mut normalized = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::Normal(value) => normalized.push(value),
                    Component::CurDir => {}
                    Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        return Err(CatalogError::InvalidConversationKey(
                            "repository path may not escape its root".into(),
                        ));
                    }
                }
            }
            if normalized.as_os_str().is_empty() {
                return Err(CatalogError::InvalidConversationKey(
                    "repository path normalizes to empty".into(),
                ));
            }
            Ok((1, os_bytes(normalized.as_os_str())))
        }
    }
}

fn path_key(path: &Path) -> Result<Vec<u8>, CatalogError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(CatalogError::InvalidRepositoryPath(
            "caller must supply a non-empty canonical absolute path".into(),
        ));
    }
    Ok(os_bytes(path.as_os_str()))
}

#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn bytes_path(value: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(value))
}

#[cfg(not(unix))]
fn bytes_path(value: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&value).into_owned())
}

fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| storage("begin catalog migration", error))?;
    let current: u32 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| migration(0, error))?;
    if current > LATEST_MIGRATION {
        return Err(CatalogError::RunLog(RunLogError::Migration {
            version: current,
            detail: format!("catalog schema is newer than supported version {LATEST_MIGRATION}"),
        }));
    }
    if current < 1 {
        // Version-one catalogs included run_leases. Retain the unused table in
        // the bootstrap schema for backward compatibility with older binaries.
        transaction.execute_batch(
            "CREATE TABLE repositories(\
                 repository_id TEXT PRIMARY KEY, common_git_dir BLOB NOT NULL UNIQUE, created_at_millis INTEGER NOT NULL);\
             CREATE TABLE repository_aliases(\
                 alias_path BLOB PRIMARY KEY, repository_id TEXT NOT NULL REFERENCES repositories(repository_id) ON DELETE CASCADE);\
             CREATE INDEX repository_aliases_repository ON repository_aliases(repository_id);\
             CREATE TABLE conversation_bindings(\
                 repository_id TEXT NOT NULL REFERENCES repositories(repository_id) ON DELETE CASCADE,\
                 scope_kind INTEGER NOT NULL, scope_path BLOB NOT NULL, logical_name TEXT NOT NULL,\
                 conversation_id TEXT NOT NULL UNIQUE, run_id TEXT NOT NULL UNIQUE, root_agent_id TEXT NOT NULL,\
                 workspace_id TEXT NOT NULL, selected_branch_id TEXT NOT NULL, updated_at_millis INTEGER NOT NULL,\
                 PRIMARY KEY(repository_id, scope_kind, scope_path, logical_name));\
             CREATE TABLE run_leases(\
                 run_id TEXT PRIMARY KEY, instance_id TEXT NOT NULL, pid_marker INTEGER,\
                 heartbeat_at_millis INTEGER NOT NULL, expires_at_millis INTEGER NOT NULL);\
             PRAGMA user_version=1;"
        ).map_err(|error| migration(1, error))?;
    }
    if current < 2 {
        transaction
            .execute_batch(
                "CREATE TABLE provider_sessions(\
                 provider TEXT NOT NULL, agent_id TEXT NOT NULL, branch_id TEXT NOT NULL,\
                 provider_thread_id TEXT NOT NULL, fingerprint_version INTEGER NOT NULL,\
                 fingerprint_value TEXT NOT NULL, last_provider_turn_id TEXT,\
                 updated_at_millis INTEGER NOT NULL,\
                 PRIMARY KEY(provider, agent_id, branch_id));\
             PRAGMA user_version=2;",
            )
            .map_err(|error| migration(2, error))?;
    }
    // Owner liveness markers are created outside the versioned migrations so
    // stores remain readable by binaries that predate run-owner tracking
    // (the retained `run_leases` table follows the same precedent). The
    // statement is idempotent and tolerates stores created before this table
    // existed.
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS run_owners(\
             run_id TEXT NOT NULL, pid INTEGER NOT NULL, process_start_time INTEGER,\
             registered_at_millis INTEGER NOT NULL,\
             PRIMARY KEY(run_id, pid));",
        )
        .map_err(|error| migration(LATEST_MIGRATION, error))?;
    // Approved non-Git folders share the durable conversation machinery but
    // retain a distinct identity namespace. Keeping this additive and
    // idempotent preserves readability by binaries predating folder anchors.
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS approved_folder_roots(\
             root_path BLOB PRIMARY KEY, repository_id TEXT NOT NULL UNIQUE REFERENCES repositories(repository_id) ON DELETE CASCADE);",
        )
        .map_err(|error| migration(LATEST_MIGRATION, error))?;
    transaction.commit().map_err(|error| migration(0, error))
}

fn configure_wal(connection: &Connection) -> Result<(), CatalogError> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(error) if is_busy(&error) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(storage("enable catalog WAL", error)),
        }
    }
}

fn set_owner_only(path: &Path) -> Result<(), CatalogError> {
    #[cfg(unix)]
    {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            CatalogError::RunLog(RunLogError::Storage {
                operation: "set private catalog permissions".into(),
                detail: format!("{}: {error}", path.display()),
            })
        })?;
    }
    Ok(())
}

fn parse_repository_id(value: String) -> Result<RepositoryId, CatalogError> {
    RepositoryId::parse(value).map_err(corrupt_id)
}

fn corrupt_id(error: super::InvalidId) -> CatalogError {
    CatalogError::RunLog(RunLogError::Corruption {
        detail: error.to_string(),
    })
}

fn storage(operation: &str, error: rusqlite::Error) -> CatalogError {
    CatalogError::RunLog(RunLogError::Storage {
        operation: operation.into(),
        detail: error.to_string(),
    })
}

fn migration(version: u32, error: rusqlite::Error) -> CatalogError {
    CatalogError::RunLog(RunLogError::Migration {
        version,
        detail: error.to_string(),
    })
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(details, _) if matches!(details.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Barrier;
    use std::thread;

    struct TestClock(AtomicI64);

    impl TestClock {
        fn new(now: i64) -> Self {
            Self(AtomicI64::new(now))
        }
    }

    impl CatalogClock for TestClock {
        fn now_millis(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn registration(clone: &str) -> RepositoryRegistration {
        RepositoryRegistration {
            common_git_dir: PathBuf::from(format!("/{clone}/.git")),
            worktree_aliases: vec![PathBuf::from(format!("/{clone}"))],
        }
    }

    fn provider_key() -> ProviderSessionKey {
        ProviderSessionKey {
            provider: "codex".into(),
            agent_id: AgentId::new(),
            branch_id: BranchId::new(),
        }
    }

    fn fingerprint(version: u32, value: &str) -> ProviderConfigurationFingerprint {
        ProviderConfigurationFingerprint {
            version,
            value: value.into(),
        }
    }

    #[test]
    fn approved_folder_identity_is_stable_and_distinct_from_git_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let folder = temporary.path().join("workspace");
        std::fs::create_dir(&folder).unwrap();
        let folder = folder.canonicalize().unwrap();

        let first = RunCatalog::open(&layout)
            .unwrap()
            .register_approved_folder(&folder)
            .unwrap();
        let reopened = RunCatalog::open(&layout).unwrap();
        let second = reopened.register_approved_folder(&folder).unwrap();
        let git = reopened
            .register_repository(RepositoryRegistration {
                common_git_dir: folder.join(".git"),
                worktree_aliases: vec![folder.clone()],
            })
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.root, folder);
        assert_ne!(first.repository_id, git.repository_id);
    }

    #[test]
    fn provider_session_roundtrips_opaque_ids_across_reopen_and_deletes() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let clock = Arc::new(TestClock::new(7_000));
        let catalog = RunCatalog::open_with_clock(&layout, clock).unwrap();
        let key = provider_key();
        let configuration = fingerprint(3, "sha256:configuration");
        let expected = catalog
            .upsert_provider_session(
                key.clone(),
                "agt_provider-owned/thread:42".into(),
                configuration.clone(),
                Some("trn_provider-owned/turn:9".into()),
            )
            .unwrap();
        assert_eq!(expected.updated_at_millis, 7_000);
        drop(catalog);

        let reopened = RunCatalog::open(&layout).unwrap();
        assert_eq!(
            reopened.provider_session(&key, &configuration).unwrap(),
            Some(expected)
        );
        assert!(reopened.delete_provider_session(&key).unwrap());
        assert!(!reopened.delete_provider_session(&key).unwrap());
        assert_eq!(
            reopened.provider_session(&key, &configuration).unwrap(),
            None
        );
    }

    #[test]
    fn provider_session_requires_an_exact_versioned_fingerprint() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let key = provider_key();
        let original = fingerprint(1, "same-digest");
        catalog
            .upsert_provider_session(key.clone(), "thread-one".into(), original.clone(), None)
            .unwrap();

        assert!(catalog
            .provider_session(&key, &fingerprint(2, "same-digest"))
            .unwrap()
            .is_none());
        assert!(catalog
            .provider_session(&key, &fingerprint(1, "different-digest"))
            .unwrap()
            .is_none());
        assert_eq!(
            catalog
                .provider_session(&key, &original)
                .unwrap()
                .unwrap()
                .provider_thread_id,
            "thread-one"
        );

        let changed = fingerprint(2, "replacement");
        catalog
            .upsert_provider_session(
                key.clone(),
                "thread-two".into(),
                changed.clone(),
                Some("turn-two".into()),
            )
            .unwrap();
        assert!(catalog.provider_session(&key, &original).unwrap().is_none());
        assert_eq!(
            catalog
                .provider_session(&key, &changed)
                .unwrap()
                .unwrap()
                .last_provider_turn_id
                .as_deref(),
            Some("turn-two")
        );
    }

    #[test]
    fn provider_sessions_reject_blank_values_and_unversioned_fingerprints() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let key = provider_key();

        let invalid = [
            catalog.upsert_provider_session(
                ProviderSessionKey {
                    provider: " \t".into(),
                    ..key.clone()
                },
                "thread".into(),
                fingerprint(1, "config"),
                None,
            ),
            catalog.upsert_provider_session(
                key.clone(),
                " \n".into(),
                fingerprint(1, "config"),
                None,
            ),
            catalog.upsert_provider_session(
                key.clone(),
                "thread".into(),
                fingerprint(1, " \r"),
                None,
            ),
            catalog.upsert_provider_session(
                key.clone(),
                "thread".into(),
                fingerprint(0, "config"),
                None,
            ),
            catalog.upsert_provider_session(
                key.clone(),
                "thread".into(),
                fingerprint(1, "config"),
                Some("  ".into()),
            ),
        ];
        assert!(invalid
            .into_iter()
            .all(|result| matches!(result, Err(CatalogError::InvalidProviderSession(_)))));
        assert!(matches!(
            catalog.provider_session(&key, &fingerprint(0, "config")),
            Err(CatalogError::InvalidProviderSession(_))
        ));
    }

    #[test]
    fn migration_from_v1_adds_provider_sessions_without_replacing_catalog() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        layout.ensure_root().unwrap();
        let database = layout.root().join(CATALOG_DATABASE);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE preserved(value TEXT NOT NULL);\
             INSERT INTO preserved(value) VALUES ('still-here');\
             PRAGMA user_version=1;",
            )
            .unwrap();
        drop(connection);

        let catalog = RunCatalog::open(&layout).unwrap();
        let key = provider_key();
        let configuration = fingerprint(1, "config");
        catalog
            .upsert_provider_session(key.clone(), "thread".into(), configuration.clone(), None)
            .unwrap();
        assert!(catalog
            .provider_session(&key, &configuration)
            .unwrap()
            .is_some());
        assert_eq!(
            catalog
                .connection()
                .unwrap()
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "still-here"
        );
    }

    #[test]
    fn concurrent_provider_upserts_never_expose_a_torn_record() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        RunCatalog::open(&layout).unwrap();
        let key = provider_key();
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let layout = layout.clone();
                let key = key.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let catalog = RunCatalog::open(&layout).unwrap();
                    barrier.wait();
                    catalog
                        .upsert_provider_session(
                            key,
                            format!("thread-{index}"),
                            fingerprint(1, &format!("configuration-{index}")),
                            Some(format!("turn-{index}")),
                        )
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let catalog = RunCatalog::open(&layout).unwrap();
        let matches: Vec<_> = (0..8)
            .filter_map(|index| {
                catalog
                    .provider_session(&key, &fingerprint(1, &format!("configuration-{index}")))
                    .unwrap()
                    .map(|session| (index, session))
            })
            .collect();
        assert_eq!(matches.len(), 1);
        let (index, session) = &matches[0];
        assert_eq!(session.provider_thread_id, format!("thread-{index}"));
        assert_eq!(session.last_provider_turn_id, Some(format!("turn-{index}")));
    }

    #[test]
    fn repository_identity_survives_reopen_and_distinguishes_clones() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let first = RunCatalog::open(&layout).unwrap();
        let original = first.register_repository(registration("clone-a")).unwrap();
        let other = first.register_repository(registration("clone-b")).unwrap();
        assert_ne!(original.repository_id, other.repository_id);
        drop(first);

        let reopened = RunCatalog::open(&layout).unwrap();
        assert_eq!(
            reopened
                .register_repository(registration("clone-a"))
                .unwrap()
                .repository_id,
            original.repository_id
        );
    }

    #[test]
    fn file_and_no_file_conversations_have_stable_distinct_bindings() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository = catalog.register_repository(registration("repo")).unwrap();
        let no_file = ConversationKey {
            repository_id: repository.repository_id.clone(),
            scope: ConversationScope::NoFile,
            logical_name: "chat".into(),
        };
        let file = ConversationKey {
            repository_id: repository.repository_id,
            scope: ConversationScope::RepositoryPath("src/lib.rs".into()),
            logical_name: "chat".into(),
        };
        let no_file_binding = catalog
            .open_conversation(no_file.clone(), BranchId::new())
            .unwrap();
        let file_binding = catalog
            .open_conversation(file.clone(), BranchId::new())
            .unwrap();
        assert_ne!(no_file_binding.run_id, file_binding.run_id);
        assert_eq!(
            catalog
                .open_conversation(no_file, BranchId::new())
                .unwrap()
                .run_id,
            no_file_binding.run_id
        );
        assert_eq!(
            catalog
                .open_conversation(file, BranchId::new())
                .unwrap()
                .conversation_id,
            file_binding.conversation_id
        );
    }

    #[test]
    fn selected_branch_update_is_durable() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository = catalog.register_repository(registration("repo")).unwrap();
        let key = ConversationKey {
            repository_id: repository.repository_id,
            scope: ConversationScope::NoFile,
            logical_name: "default".into(),
        };
        let binding = catalog
            .open_conversation(key.clone(), BranchId::new())
            .unwrap();
        let selected = BranchId::new();
        assert!(catalog
            .update_selected_branch(&binding, selected.clone())
            .unwrap());
        drop(catalog);
        assert_eq!(
            RunCatalog::open(&layout)
                .unwrap()
                .open_conversation(key, BranchId::new())
                .unwrap()
                .selected_branch_id,
            selected
        );
    }

    #[test]
    fn starting_a_conversation_replaces_discovery_without_invalidating_the_old_run() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository = catalog.register_repository(registration("repo")).unwrap();
        let key = ConversationKey {
            repository_id: repository.repository_id,
            scope: ConversationScope::NoFile,
            logical_name: "chat".into(),
        };
        let original = catalog
            .open_conversation(key.clone(), BranchId::new())
            .unwrap();
        let fresh_branch = BranchId::new();
        let fresh = catalog
            .start_conversation(key.clone(), fresh_branch.clone())
            .unwrap();

        assert_eq!(fresh.key, key);
        assert_ne!(fresh.conversation_id, original.conversation_id);
        assert_ne!(fresh.run_id, original.run_id);
        assert_ne!(fresh.root_agent_id, original.root_agent_id);
        assert_ne!(fresh.workspace_id, original.workspace_id);
        assert_ne!(fresh.selected_branch_id, original.selected_branch_id);
        assert_eq!(fresh.selected_branch_id, fresh_branch);
        assert_eq!(
            catalog
                .open_conversation(key.clone(), BranchId::new())
                .unwrap(),
            fresh
        );
        assert!(!catalog
            .update_selected_branch(&original, BranchId::new())
            .unwrap());
        assert_eq!(
            catalog.open_conversation(key, BranchId::new()).unwrap(),
            fresh
        );
    }

    #[test]
    fn fresh_conversation_start_binds_only_when_no_conversation_exists() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository = catalog.register_repository(registration("repo")).unwrap();
        let key = ConversationKey {
            repository_id: repository.repository_id,
            scope: ConversationScope::NoFile,
            logical_name: "chat".into(),
        };

        let first = catalog
            .start_fresh_conversation(key.clone(), BranchId::new())
            .unwrap();
        assert!(first.is_bound());
        let first = first.into_binding();
        assert_eq!(
            catalog
                .open_conversation(key.clone(), BranchId::new())
                .unwrap(),
            first
        );

        let second = catalog
            .start_fresh_conversation(key.clone(), BranchId::new())
            .unwrap();
        assert!(!second.is_bound());
        let second = second.into_binding();
        assert_ne!(second.run_id, first.run_id);
        assert_ne!(second.conversation_id, first.conversation_id);
        // The prior conversation stays discoverable across reopen.
        drop(catalog);
        assert_eq!(
            RunCatalog::open(&layout)
                .unwrap()
                .open_conversation(key, BranchId::new())
                .unwrap(),
            first
        );
    }

    #[test]
    fn run_owner_markers_roundtrip_and_upsert_per_process() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let clock = Arc::new(TestClock::new(9_000));
        let catalog = RunCatalog::open_with_clock(&layout, clock).unwrap();
        let run_id = RunId::new();
        assert!(catalog.run_owners(&run_id).unwrap().is_empty());

        catalog
            .register_run_owner(&run_id, 41, Some(1_234))
            .unwrap();
        catalog.register_run_owner(&run_id, 42, None).unwrap();
        // Re-registering the same PID replaces its marker.
        catalog
            .register_run_owner(&run_id, 41, Some(5_678))
            .unwrap();
        drop(catalog);

        let owners = RunCatalog::open(&layout)
            .unwrap()
            .run_owners(&run_id)
            .unwrap();
        assert_eq!(
            owners,
            vec![
                RunOwner {
                    pid: 41,
                    process_start_time: Some(5_678),
                    registered_at_millis: 9_000,
                },
                RunOwner {
                    pid: 42,
                    process_start_time: None,
                    registered_at_millis: 9_000,
                },
            ]
        );
    }

    #[test]
    fn concurrent_conversation_starts_return_independent_runs() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = RunCatalog::open(&layout).unwrap();
        let repository = catalog.register_repository(registration("repo")).unwrap();
        let key = ConversationKey {
            repository_id: repository.repository_id,
            scope: ConversationScope::NoFile,
            logical_name: "chat".into(),
        };
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let layout = layout.clone();
                let key = key.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let catalog = RunCatalog::open(&layout).unwrap();
                    barrier.wait();
                    catalog.start_conversation(key, BranchId::new()).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let bindings = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        let run_ids = bindings
            .iter()
            .map(|binding| binding.run_id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(run_ids.len(), bindings.len());
        let discovered = catalog.open_conversation(key, BranchId::new()).unwrap();
        assert!(run_ids.contains(&discovered.run_id));
    }

    #[test]
    fn concurrent_registration_converges_on_one_repository_id() {
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let layout = layout.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    RunCatalog::open(&layout)
                        .unwrap()
                        .register_repository(registration("shared"))
                        .unwrap()
                        .repository_id
                })
            })
            .collect();
        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_database_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let layout = RunStorageLayout::new(temporary.path().join("runs"));
        RunCatalog::open(&layout).unwrap();
        assert_eq!(
            std::fs::metadata(layout.root().join(CATALOG_DATABASE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
