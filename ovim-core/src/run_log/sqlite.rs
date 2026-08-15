use super::{
    EventClock, EventEnvelope, EventId, EventIdGenerator, NewRunEvent, RunEventSink, RunId,
    RunLogError, SystemEventClock, SystemEventIdGenerator, EVENT_PAYLOAD_VERSION,
    EVENT_SCHEMA_VERSION,
};
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const LATEST_MIGRATION: u32 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Durable SQLite-backed event history.
///
/// The connection is guarded rather than exposed so a sink remains `Send +
/// Sync`, and each append holds one immediate transaction from run-local
/// sequence allocation through envelope insertion.
pub struct SqliteRunEventSink {
    connection: Mutex<Connection>,
    id_generator: Arc<dyn EventIdGenerator>,
    clock: Arc<dyn EventClock>,
}

impl SqliteRunEventSink {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RunLogError> {
        Self::open_with_sources(
            path,
            Arc::new(SystemEventIdGenerator),
            Arc::new(SystemEventClock),
        )
    }

    pub fn open_with_sources(
        path: impl AsRef<Path>,
        id_generator: Arc<dyn EventIdGenerator>,
        clock: Arc<dyn EventClock>,
    ) -> Result<Self, RunLogError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| storage("open database", error))?;
        Self::from_connection(connection, id_generator, clock)
    }

    pub fn open_in_memory() -> Result<Self, RunLogError> {
        Self::in_memory_with_sources(Arc::new(SystemEventIdGenerator), Arc::new(SystemEventClock))
    }

    pub fn in_memory_with_sources(
        id_generator: Arc<dyn EventIdGenerator>,
        clock: Arc<dyn EventClock>,
    ) -> Result<Self, RunLogError> {
        let connection =
            Connection::open_in_memory().map_err(|error| storage("open database", error))?;
        Self::from_connection(connection, id_generator, clock)
    }

    fn from_connection(
        mut connection: Connection,
        id_generator: Arc<dyn EventIdGenerator>,
        clock: Arc<dyn EventClock>,
    ) -> Result<Self, RunLogError> {
        configure(&connection)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            id_generator,
            clock,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, RunLogError> {
        self.connection.lock().map_err(|_| RunLogError::Poisoned)
    }

    /// The optional sequence guard is evaluated inside the same immediate
    /// transaction that inserts the event, so no other connection can advance
    /// the run between the check and the append.
    fn append_guarded(
        &self,
        event: NewRunEvent,
        expected_last_sequence: Option<Option<u64>>,
    ) -> Result<EventEnvelope, RunLogError> {
        let event_id = self.id_generator.next_event_id();
        let recorded_at = self.clock.now();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| storage("begin append transaction", error))?;

        if let Some(expected) = expected_last_sequence {
            let stored: Option<i64> = transaction
                .query_row(
                    "SELECT last_sequence FROM runs WHERE run_id = ?1",
                    [event.run_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| storage("read guarded run sequence", error))?;
            let actual = stored
                .map(|value| {
                    u64::try_from(value).map_err(|_| RunLogError::Corruption {
                        detail: format!("run {} has negative last sequence {value}", event.run_id),
                    })
                })
                .transpose()?;
            if actual != expected {
                return Err(RunLogError::SequenceConflict {
                    run_id: event.run_id,
                    expected,
                    actual,
                });
            }
        }

        if let Some(cause) = &event.caused_by {
            let cause_run: Option<String> = transaction
                .query_row(
                    "SELECT run_id FROM events WHERE event_id = ?1",
                    [cause.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| storage("validate causal event", error))?;
            if cause_run.as_deref() != Some(event.run_id.as_str()) {
                return Err(RunLogError::MissingCause {
                    run_id: event.run_id,
                    event_id: cause.clone(),
                });
            }
        }

        let duplicate: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
                [event_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| storage("check event identifier", error))?;
        if duplicate {
            return Err(RunLogError::DuplicateEventId { event_id });
        }

        transaction
            .execute(
                "INSERT INTO runs (run_id, last_sequence, created_at) VALUES (?1, 0, ?2)\
                 ON CONFLICT(run_id) DO NOTHING",
                params![event.run_id.as_str(), recorded_at],
            )
            .map_err(|error| storage("ensure run", error))?;
        let previous: i64 = transaction
            .query_row(
                "SELECT last_sequence FROM runs WHERE run_id = ?1",
                [event.run_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| storage("read run sequence", error))?;
        let sequence = previous
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| RunLogError::SequenceExhausted {
                run_id: event.run_id.clone(),
            })?;

        let envelope = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            payload_version: EVENT_PAYLOAD_VERSION,
            event_id,
            run_id: event.run_id,
            sequence,
            recorded_at,
            caused_by: event.caused_by,
            operation_id: event.operation_id,
            provider_call_id: event.provider_call_id,
            actor: event.actor,
            agent_id: event.agent_id,
            turn_id: event.turn_id,
            workspace_id: event.workspace_id,
            branch_id: event.branch_id,
            kind: event.kind,
        };
        let envelope_json =
            serde_json::to_string(&envelope).map_err(|error| RunLogError::Serialization {
                operation: "encode event envelope".into(),
                detail: error.to_string(),
            })?;

        transaction
            .execute(
                "UPDATE runs SET last_sequence = ?2 WHERE run_id = ?1",
                params![envelope.run_id.as_str(), sequence as i64],
            )
            .map_err(|error| storage("allocate run sequence", error))?;
        transaction
            .execute(
                "INSERT INTO events \
                 (event_id, run_id, sequence, caused_by, recorded_at, envelope_json, \
                  kind, agent_id, turn_id, operation_id, workspace_id, branch_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    envelope.event_id.as_str(),
                    envelope.run_id.as_str(),
                    envelope.sequence as i64,
                    envelope.caused_by.as_ref().map(EventId::as_str),
                    envelope.recorded_at,
                    envelope_json,
                    event_kind_name(&envelope.kind),
                    envelope.agent_id.as_ref().map(|id| id.as_str()),
                    envelope.turn_id.as_ref().map(|id| id.as_str()),
                    envelope.operation_id.as_ref().map(|id| id.as_str()),
                    envelope.workspace_id.as_ref().map(|id| id.as_str()),
                    envelope.branch_id.as_ref().map(|id| id.as_str()),
                ],
            )
            .map_err(|error| {
                if is_constraint(&error) {
                    RunLogError::DuplicateEventId {
                        event_id: envelope.event_id.clone(),
                    }
                } else {
                    storage("insert event", error)
                }
            })?;
        transaction
            .commit()
            .map_err(|error| storage("commit append transaction", error))?;
        Ok(envelope)
    }
}

impl RunEventSink for SqliteRunEventSink {
    fn append(&self, event: NewRunEvent) -> Result<EventEnvelope, RunLogError> {
        self.append_guarded(event, None)
    }

    fn append_if_last(
        &self,
        event: NewRunEvent,
        expected_last_sequence: Option<u64>,
    ) -> Result<EventEnvelope, RunLogError> {
        self.append_guarded(event, Some(expected_last_sequence))
    }

    fn event(
        &self,
        run_id: &RunId,
        event_id: &EventId,
    ) -> Result<Option<EventEnvelope>, RunLogError> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT run_id, sequence, event_id, envelope_json FROM events \
                 WHERE run_id = ?1 AND event_id = ?2",
                params![run_id.as_str(), event_id.as_str()],
                read_stored_event,
            )
            .optional()
            .map_err(|error| storage("read event", error))?;
        row.map(decode_stored_event).transpose()
    }

    fn events(&self, run_id: &RunId) -> Result<Vec<EventEnvelope>, RunLogError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT run_id, sequence, event_id, envelope_json FROM events \
                 WHERE run_id = ?1 ORDER BY sequence ASC",
            )
            .map_err(|error| storage("prepare run event read", error))?;
        let rows = statement
            .query_map([run_id.as_str()], read_stored_event)
            .map_err(|error| storage("read run events", error))?;
        rows.map(|row| {
            row.map_err(|error| storage("read run event row", error))
                .and_then(decode_stored_event)
        })
        .collect()
    }

    /// Indexed, bounded tail read. `events_run_sequence` covers
    /// `(run_id, sequence)`, so this seeks straight to `after_sequence` and
    /// stops after `limit` rows instead of decoding the whole run.
    fn events_after(
        &self,
        run_id: &RunId,
        after_sequence: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<EventEnvelope>, RunLogError> {
        let connection = self.connection()?;
        // SQLite treats a negative LIMIT as unbounded, which is what we want
        // when the caller did not ask for one. usize -> i64 saturates rather
        // than wrapping so a huge limit cannot become a small or negative one.
        let bound: i64 = match limit {
            Some(limit) => i64::try_from(limit).unwrap_or(i64::MAX),
            None => -1,
        };
        let after: i64 = match after_sequence {
            Some(after) => i64::try_from(after).unwrap_or(i64::MAX),
            None => -1,
        };
        let mut statement = connection
            .prepare(
                "SELECT run_id, sequence, event_id, envelope_json FROM events \
                 WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence ASC LIMIT ?3",
            )
            .map_err(|error| storage("prepare bounded run event read", error))?;
        let rows = statement
            .query_map(params![run_id.as_str(), after, bound], read_stored_event)
            .map_err(|error| storage("read bounded run events", error))?;
        rows.map(|row| {
            row.map_err(|error| storage("read bounded run event row", error))
                .and_then(decode_stored_event)
        })
        .collect()
    }

    fn last_sequence(&self, run_id: &RunId) -> Result<Option<u64>, RunLogError> {
        let connection = self.connection()?;
        let sequence: Option<i64> = connection
            .query_row(
                "SELECT last_sequence FROM runs WHERE run_id = ?1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| storage("read last run sequence", error))?;
        sequence
            .map(|value| {
                u64::try_from(value).map_err(|_| RunLogError::Corruption {
                    detail: format!("run {run_id} has negative last sequence {value}"),
                })
            })
            .transpose()
    }

    fn runs(&self) -> Result<Vec<RunId>, RunLogError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT run_id FROM runs ORDER BY rowid ASC")
            .map_err(|error| storage("prepare run discovery", error))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| storage("discover runs", error))?;
        rows.map(|row| {
            let value = row.map_err(|error| storage("read discovered run", error))?;
            RunId::parse(value.clone()).map_err(|error| RunLogError::Corruption {
                detail: format!("invalid persisted run identifier {value:?}: {error}"),
            })
        })
        .collect()
    }
}

type StoredEvent = (String, i64, String, String);

fn read_stored_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn decode_stored_event(
    (stored_run, stored_sequence, stored_event, json): StoredEvent,
) -> Result<EventEnvelope, RunLogError> {
    let envelope: EventEnvelope =
        serde_json::from_str(&json).map_err(|error| RunLogError::Corruption {
            detail: format!("event {stored_event} has invalid envelope JSON: {error}"),
        })?;
    let sequence = u64::try_from(stored_sequence).map_err(|_| RunLogError::Corruption {
        detail: format!("event {stored_event} has negative sequence {stored_sequence}"),
    })?;
    if envelope.run_id.as_str() != stored_run
        || envelope.event_id.as_str() != stored_event
        || envelope.sequence != sequence
    {
        return Err(RunLogError::Corruption {
            detail: format!(
                "event {stored_event} envelope metadata does not match its indexed row"
            ),
        });
    }
    Ok(envelope)
}

fn configure(connection: &Connection) -> Result<(), RunLogError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| storage("set busy timeout", error))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| storage("enable foreign keys", error))?;
    // In-memory databases report `memory`; file databases switch to WAL.
    configure_wal(connection)?;
    // Intent events must reach durable storage before their effects begin. FULL
    // synchronous mode keeps that ordering meaningful across power loss, not
    // merely process crashes.
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| storage("enable full synchronization", error))?;
    Ok(())
}

fn configure_wal(connection: &Connection) -> Result<(), RunLogError> {
    // Setting WAL itself needs an exclusive schema lock and SQLite does not
    // consistently invoke the busy handler for this PRAGMA. Retry within the
    // same bounded window used by normal database operations.
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(error) if is_busy(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(storage("enable WAL journal", error)),
        }
    }
}

fn migrate(connection: &mut Connection) -> Result<(), RunLogError> {
    // Serialize the version read and every migration step. Without this lock,
    // two first-open callers can both observe version zero and race the ALTERs.
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| migration(0, error))?;
    let current: u32 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| RunLogError::Migration {
            version: 0,
            detail: error.to_string(),
        })?;
    if current > LATEST_MIGRATION {
        return Err(RunLogError::Migration {
            version: current,
            detail: format!("database schema is newer than supported version {LATEST_MIGRATION}"),
        });
    }
    if current < 1 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (\
                     version INTEGER PRIMARY KEY,\
                     applied_at TEXT NOT NULL\
                 );\
                 CREATE TABLE IF NOT EXISTS runs (\
                     run_id TEXT PRIMARY KEY,\
                     last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0),\
                     created_at TEXT NOT NULL\
                 );\
                 CREATE TABLE IF NOT EXISTS events (\
                     event_id TEXT PRIMARY KEY,\
                     run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,\
                     sequence INTEGER NOT NULL CHECK(sequence > 0),\
                     caused_by TEXT REFERENCES events(event_id),\
                     recorded_at TEXT NOT NULL,\
                     envelope_json TEXT NOT NULL,\
                     UNIQUE(run_id, sequence)\
                 );\
                 CREATE INDEX IF NOT EXISTS events_run_sequence \
                     ON events(run_id, sequence);\
                 INSERT OR IGNORE INTO schema_migrations(version, applied_at) \
                     VALUES (1, CURRENT_TIMESTAMP);\
                 PRAGMA user_version = 1;",
            )
            .map_err(|error| RunLogError::Migration {
                version: 1,
                detail: error.to_string(),
            })?;
    }
    if current < 2 {
        migrate_indexed_event_dimensions(&transaction)?;
    }
    if current < 3 {
        migrate_branch_dimension(&transaction)?;
    }
    transaction.commit().map_err(|error| migration(0, error))
}

fn migrate_branch_dimension(transaction: &rusqlite::Transaction<'_>) -> Result<(), RunLogError> {
    transaction
        .execute_batch("ALTER TABLE events ADD COLUMN branch_id TEXT;")
        .map_err(|error| migration(3, error))?;

    let stored: Vec<(String, String)> = {
        let mut statement = transaction
            .prepare("SELECT event_id, envelope_json FROM events")
            .map_err(|error| migration(3, error))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| migration(3, error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| migration(3, error))?
    };
    for (event_id, json) in stored {
        let envelope: EventEnvelope =
            serde_json::from_str(&json).map_err(|error| RunLogError::Migration {
                version: 3,
                detail: format!("cannot index event {event_id}: {error}"),
            })?;
        transaction
            .execute(
                "UPDATE events SET branch_id = ?2 WHERE event_id = ?1",
                params![event_id, envelope.branch_id.as_ref().map(|id| id.as_str()),],
            )
            .map_err(|error| migration(3, error))?;
    }
    transaction
        .execute_batch(
            "CREATE INDEX events_branch_id ON events(branch_id);\
             INSERT OR IGNORE INTO schema_migrations(version, applied_at) \
                 VALUES (3, CURRENT_TIMESTAMP);\
             PRAGMA user_version = 3;",
        )
        .map_err(|error| migration(3, error))?;
    Ok(())
}

fn migrate_indexed_event_dimensions(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), RunLogError> {
    transaction
        .execute_batch(
            "ALTER TABLE events ADD COLUMN kind TEXT;\
             ALTER TABLE events ADD COLUMN agent_id TEXT;\
             ALTER TABLE events ADD COLUMN turn_id TEXT;\
             ALTER TABLE events ADD COLUMN operation_id TEXT;\
             ALTER TABLE events ADD COLUMN workspace_id TEXT;",
        )
        .map_err(|error| migration(2, error))?;

    let stored: Vec<(String, String)> = {
        let mut statement = transaction
            .prepare("SELECT event_id, envelope_json FROM events")
            .map_err(|error| migration(2, error))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| migration(2, error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| migration(2, error))?
    };
    for (event_id, json) in stored {
        let envelope: EventEnvelope =
            serde_json::from_str(&json).map_err(|error| RunLogError::Migration {
                version: 2,
                detail: format!("cannot index event {event_id}: {error}"),
            })?;
        transaction
            .execute(
                "UPDATE events SET kind = ?2, agent_id = ?3, turn_id = ?4, \
                 operation_id = ?5, workspace_id = ?6 WHERE event_id = ?1",
                params![
                    event_id,
                    event_kind_name(&envelope.kind),
                    envelope.agent_id.as_ref().map(|id| id.as_str()),
                    envelope.turn_id.as_ref().map(|id| id.as_str()),
                    envelope.operation_id.as_ref().map(|id| id.as_str()),
                    envelope.workspace_id.as_ref().map(|id| id.as_str()),
                ],
            )
            .map_err(|error| migration(2, error))?;
    }
    transaction
        .execute_batch(
            "CREATE INDEX events_kind ON events(kind);\
             CREATE INDEX events_agent_id ON events(agent_id);\
             CREATE INDEX events_turn_id ON events(turn_id);\
             CREATE INDEX events_operation_id ON events(operation_id);\
             CREATE INDEX events_workspace_id ON events(workspace_id);\
             INSERT OR IGNORE INTO schema_migrations(version, applied_at) \
                 VALUES (2, CURRENT_TIMESTAMP);\
             PRAGMA user_version = 2;",
        )
        .map_err(|error| migration(2, error))?;
    Ok(())
}

fn event_kind_name(kind: &super::EventKind) -> &str {
    match kind {
        super::EventKind::RunLifecycle(_) => "run_lifecycle",
        super::EventKind::BranchLifecycle(_) => "branch_lifecycle",
        super::EventKind::AgentLifecycle(_) => "agent_lifecycle",
        super::EventKind::AgentProvider(_) => "agent_provider",
        super::EventKind::AgentUsage(_) => "agent_usage",
        super::EventKind::AgentProgress(_) => "agent_progress",
        super::EventKind::AgentHandoff(_) => "agent_handoff",
        super::EventKind::AgentHandoffValidationFailed(_) => "agent_handoff_validation_failed",
        super::EventKind::AgentAttachment(_) => "agent_attachment",
        super::EventKind::AgentFollowup(_) => "agent_followup",
        super::EventKind::AgentApprovalRequested(_) => "agent_approval_requested",
        super::EventKind::AgentApprovalResolved(_) => "agent_approval_resolved",
        super::EventKind::AgentMessage(_) => "agent_message",
        super::EventKind::AgentMessageDelivery(_) => "agent_message_delivery",
        super::EventKind::MailboxNotification(_) => "mailbox_notification",
        super::EventKind::MailboxConsumed(_) => "mailbox_consumed",
        super::EventKind::TurnLifecycle(_) => "turn_lifecycle",
        super::EventKind::Message(_) => "message",
        super::EventKind::ToolIntent(_) => "tool_intent",
        super::EventKind::ToolDecision(_) => "tool_decision",
        super::EventKind::ToolStarted(_) => "tool_started",
        super::EventKind::ToolResult(_) => "tool_result",
        super::EventKind::FileMutation(_) => "file_mutation",
        super::EventKind::Checkpoint(_) => "checkpoint",
        super::EventKind::Divergence(_) => "divergence",
        super::EventKind::Unknown { name, .. } => name,
    }
}

fn migration(version: u32, error: rusqlite::Error) -> RunLogError {
    RunLogError::Migration {
        version,
        detail: error.to_string(),
    }
}

fn storage(operation: &str, error: rusqlite::Error) -> RunLogError {
    RunLogError::Storage {
        operation: operation.into(),
        detail: error.to_string(),
    }
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(details, _)
            if details.code == ErrorCode::ConstraintViolation
    )
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(details, _)
            if matches!(details.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::{
        AgentId, BranchId, EventActor, EventKind, InMemoryRunEventSink, MessageEvent, MessageRole,
        OperationId, TurnId, WorkspaceId,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Barrier;
    use std::thread;
    use tempfile::tempdir;

    struct SequentialIds(AtomicU64);

    impl EventIdGenerator for SequentialIds {
        fn next_event_id(&self) -> EventId {
            EventId::parse(format!(
                "evt_sqlite_{:04}",
                self.0.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap()
        }
    }

    struct FixedClock;

    impl EventClock for FixedClock {
        fn now(&self) -> String {
            "2026-07-13T12:00:00Z".into()
        }
    }

    fn message(run_id: RunId, content: &str) -> NewRunEvent {
        NewRunEvent::new(
            run_id,
            EventActor::User,
            EventKind::Message(MessageEvent {
                role: MessageRole::User,
                content: content.into(),
            }),
        )
    }

    #[test]
    fn persists_events_and_discovery_across_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        let run = RunId::parse("run_persistent").unwrap();
        let event = {
            let sink = SqliteRunEventSink::open(&path).unwrap();
            sink.append(message(run.clone(), "hello")).unwrap()
        };

        let reopened = SqliteRunEventSink::open(&path).unwrap();
        assert_eq!(reopened.runs().unwrap(), vec![run.clone()]);
        assert_eq!(reopened.events(&run).unwrap(), vec![event]);
        assert_eq!(reopened.last_sequence(&run).unwrap(), Some(1));
    }

    // The indexed SQLite override and the filter-based default in the trait must
    // be observationally identical, or a store swap silently changes what
    // callers see. Checked against InMemoryRunEventSink, which uses the default.
    #[test]
    fn bounded_reads_match_the_default_implementation_and_full_reads() {
        let sqlite = SqliteRunEventSink::open_in_memory().unwrap();
        let memory = InMemoryRunEventSink::new();
        let run = RunId::parse("run_bounded").unwrap();
        let other = RunId::parse("run_other").unwrap();
        for index in 0..25 {
            let content = format!("event {index}");
            sqlite.append(message(run.clone(), &content)).unwrap();
            memory.append(message(run.clone(), &content)).unwrap();
            // Interleave a second run so a missing run_id predicate would show up.
            sqlite.append(message(other.clone(), &content)).unwrap();
            memory.append(message(other.clone(), &content)).unwrap();
        }

        let full = sqlite.events(&run).unwrap();
        assert_eq!(full.len(), 25);

        for after in [None, Some(0), Some(1), Some(12), Some(25), Some(99)] {
            for limit in [None, Some(1), Some(7), Some(1000)] {
                let from_sqlite = sqlite.events_after(&run, after, limit).unwrap();
                let from_default = memory.events_after(&run, after, limit).unwrap();
                // The two sinks mint their own event ids and timestamps, so only
                // the selected sequence window is comparable across them. That
                // window is precisely what the two implementations must agree on.
                let sqlite_window: Vec<u64> =
                    from_sqlite.iter().map(|event| event.sequence).collect();
                let default_window: Vec<u64> =
                    from_default.iter().map(|event| event.sequence).collect();
                assert_eq!(
                    sqlite_window, default_window,
                    "sqlite and default disagree for after={after:?} limit={limit:?}"
                );

                let expected: Vec<_> = full
                    .iter()
                    .filter(|event| after.is_none_or(|after| event.sequence > after))
                    .take(limit.unwrap_or(usize::MAX))
                    .cloned()
                    .collect();
                assert_eq!(
                    from_sqlite, expected,
                    "bounded read disagrees with the filtered full read for \
                     after={after:?} limit={limit:?}"
                );
                assert!(from_sqlite.iter().all(|event| event.run_id == run));
            }
        }

        // An unbounded bounded-read is exactly the full read.
        assert_eq!(sqlite.events_after(&run, None, None).unwrap(), full);
    }

    #[test]
    fn allocates_independent_monotonic_sequences() {
        let sink = SqliteRunEventSink::open_in_memory().unwrap();
        let first = RunId::parse("run_first").unwrap();
        let second = RunId::parse("run_second").unwrap();
        assert_eq!(
            sink.append(message(first.clone(), "1")).unwrap().sequence,
            1
        );
        assert_eq!(sink.append(message(second, "1")).unwrap().sequence, 1);
        assert_eq!(sink.append(message(first, "2")).unwrap().sequence, 2);
    }

    #[test]
    fn guarded_append_rejects_a_stale_expected_sequence() {
        let sink = SqliteRunEventSink::open_in_memory().unwrap();
        let run = RunId::parse("run_guarded").unwrap();
        assert_eq!(
            sink.append_if_last(message(run.clone(), "first"), None)
                .unwrap()
                .sequence,
            1
        );

        let error = sink
            .append_if_last(message(run.clone(), "stale"), None)
            .unwrap_err();
        assert_eq!(
            error,
            RunLogError::SequenceConflict {
                run_id: run.clone(),
                expected: None,
                actual: Some(1),
            }
        );
        assert_eq!(sink.events(&run).unwrap().len(), 1);

        assert_eq!(
            sink.append_if_last(message(run, "second"), Some(1))
                .unwrap()
                .sequence,
            2
        );
    }

    #[test]
    fn rejects_missing_and_cross_run_causes() {
        let sink = SqliteRunEventSink::open_in_memory().unwrap();
        let first = RunId::parse("run_first").unwrap();
        let second = RunId::parse("run_second").unwrap();
        let cause = sink.append(message(first, "cause")).unwrap();
        let error = sink
            .append(message(second.clone(), "effect").caused_by(cause.event_id.clone()))
            .unwrap_err();
        assert_eq!(
            error,
            RunLogError::MissingCause {
                run_id: second,
                event_id: cause.event_id
            }
        );
    }

    #[test]
    fn preserves_unknown_event_payloads() {
        let sink = SqliteRunEventSink::in_memory_with_sources(
            Arc::new(SequentialIds(AtomicU64::new(1))),
            Arc::new(FixedClock),
        )
        .unwrap();
        let run = RunId::parse("run_unknown").unwrap();
        let inserted = sink
            .append(NewRunEvent::new(
                run.clone(),
                EventActor::System("extension".into()),
                EventKind::Unknown {
                    name: "future.transition".into(),
                    payload: json!({"paths": ["src/lib.rs"], "revision": 7}),
                },
            ))
            .unwrap();
        assert_eq!(
            sink.event(&run, &inserted.event_id).unwrap(),
            Some(inserted)
        );
    }

    #[test]
    fn indexed_dimensions_match_the_stored_envelope() {
        let sink = SqliteRunEventSink::open_in_memory().unwrap();
        let run = RunId::parse("run_indexed").unwrap();
        let agent = AgentId::parse("agt_indexed").unwrap();
        let turn = TurnId::parse("trn_indexed").unwrap();
        let operation = OperationId::parse("op_indexed").unwrap();
        let workspace = WorkspaceId::parse("wsp_indexed").unwrap();
        let branch = BranchId::parse("brn_indexed").unwrap();
        let envelope = sink
            .append(
                message(run, "indexed")
                    .for_agent(agent.clone())
                    .in_turn(turn.clone())
                    .for_operation(operation.clone())
                    .in_workspace(workspace.clone())
                    .in_branch(branch.clone()),
            )
            .unwrap();

        let connection = sink.connection().unwrap();
        let indexed: (String, String, String, String, String, String) = connection
            .query_row(
                "SELECT kind, agent_id, turn_id, operation_id, workspace_id, branch_id \
                 FROM events WHERE event_id = ?1",
                [envelope.event_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            indexed,
            (
                "message".into(),
                agent.to_string(),
                turn.to_string(),
                operation.to_string(),
                workspace.to_string(),
                branch.to_string(),
            )
        );
        assert_eq!(envelope.agent_id, Some(agent));
        assert_eq!(envelope.turn_id, Some(turn));
        assert_eq!(envelope.operation_id, Some(operation));
        assert_eq!(envelope.workspace_id, Some(workspace));
        assert_eq!(envelope.branch_id, Some(branch));
    }

    #[test]
    fn migrations_are_idempotent_on_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        SqliteRunEventSink::open(&path).unwrap();
        SqliteRunEventSink::open(&path).unwrap();
        let connection = Connection::open(path).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, LATEST_MIGRATION);
    }

    #[test]
    fn upgrades_v1_and_backfills_indexed_dimensions() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        let run = RunId::parse("run_v1").unwrap();
        let branch = BranchId::parse("brn_from_v1_envelope").unwrap();
        let envelope = InMemoryRunEventSink::new()
            .append(message(run.clone(), "from v1").in_branch(branch.clone()))
            .unwrap();
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                     CREATE TABLE runs (run_id TEXT PRIMARY KEY, last_sequence INTEGER NOT NULL, created_at TEXT NOT NULL);\
                     CREATE TABLE events (\
                         event_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(run_id),\
                         sequence INTEGER NOT NULL, caused_by TEXT REFERENCES events(event_id),\
                         recorded_at TEXT NOT NULL, envelope_json TEXT NOT NULL,\
                         UNIQUE(run_id, sequence)\
                     );\
                     INSERT INTO schema_migrations VALUES (1, CURRENT_TIMESTAMP);\
                     PRAGMA user_version = 1;",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO runs VALUES (?1, 1, ?2)",
                    params![run.as_str(), envelope.recorded_at],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events VALUES (?1, ?2, 1, NULL, ?3, ?4)",
                    params![
                        envelope.event_id.as_str(),
                        run.as_str(),
                        envelope.recorded_at,
                        serde_json::to_string(&envelope).unwrap()
                    ],
                )
                .unwrap();
        }

        let sink = SqliteRunEventSink::open(path).unwrap();
        assert_eq!(sink.events(&run).unwrap(), vec![envelope.clone()]);
        let connection = sink.connection().unwrap();
        let indexed: (String, String) = connection
            .query_row(
                "SELECT kind, branch_id FROM events WHERE event_id = ?1",
                [envelope.event_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(indexed, ("message".into(), branch.to_string()));
    }

    #[test]
    fn concurrent_file_writers_allocate_each_sequence_once() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        SqliteRunEventSink::open(&path).unwrap();
        let run = RunId::parse("run_concurrent").unwrap();
        let handles: Vec<_> = (0..12)
            .map(|index| {
                let path = path.clone();
                let run = run.clone();
                thread::spawn(move || {
                    let sink = SqliteRunEventSink::open(path).unwrap();
                    sink.append(message(run, &index.to_string())).unwrap()
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let sink = SqliteRunEventSink::open(path).unwrap();
        assert_eq!(
            sink.events(&run)
                .unwrap()
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            (1..=12).collect::<Vec<_>>()
        );
    }

    #[test]
    fn concurrent_first_open_serializes_schema_migrations() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("brand-new.sqlite");
        let barrier = Arc::new(Barrier::new(12));
        let handles: Vec<_> = (0..12)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    SqliteRunEventSink::open(path)
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let connection = Connection::open(path).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, LATEST_MIGRATION);
    }
}
