use super::{
    AgentId, AgentLifecycleState, BaseManifestId, BranchId, BranchLifecycleState, EventEnvelope,
    EventId, EventKind, MessageRole, OperationId, RepositoryId, RunEventSink, RunId,
    RunLifecycleState, RunLogError, ToolDecision, ToolOutcome, TurnId, TurnLifecycleState,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

/// Storage-neutral read service for run history and future replay UIs.
#[derive(Clone)]
pub struct RunHistoryQuery {
    sink: Arc<dyn RunEventSink>,
}

impl RunHistoryQuery {
    pub fn new(sink: Arc<dyn RunEventSink>) -> Self {
        Self { sink }
    }

    /// Lists runs in the sink's stable creation order. The cursor is the last
    /// run returned by the previous page, so appending newer runs cannot shift
    /// an in-progress traversal.
    pub fn list_runs(
        &self,
        after: Option<&RunListCursor>,
        limit: usize,
    ) -> Result<RunListPage, RunQueryError> {
        if limit == 0 {
            return Ok(RunListPage {
                runs: Vec::new(),
                next_cursor: None,
            });
        }

        let run_ids = self.sink.runs()?;
        let start = match after {
            Some(cursor) => run_ids
                .iter()
                .position(|run_id| run_id == &cursor.run_id)
                .map(|index| index + 1)
                .ok_or_else(|| RunQueryError::CursorNotFound(cursor.run_id.clone()))?,
            None => 0,
        };
        let end = start.saturating_add(limit).min(run_ids.len());
        let mut runs = Vec::with_capacity(end.saturating_sub(start));
        for run_id in &run_ids[start..end] {
            // A run returned by `runs()` may still be structurally incomplete;
            // summary folding deliberately does not require a Created event.
            runs.push(self.fold_summary(run_id.clone(), self.sink.events(run_id)?));
        }
        let next_cursor = (end < run_ids.len())
            .then(|| {
                runs.last().map(|summary| RunListCursor {
                    run_id: summary.run_id.clone(),
                })
            })
            .flatten();

        Ok(RunListPage { runs, next_cursor })
    }

    pub fn run_summary(&self, run_id: &RunId) -> Result<Option<RunSummary>, RunQueryError> {
        let events = self.sink.events(run_id)?;
        if events.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.fold_summary(run_id.clone(), events)))
    }

    /// Returns events whose sequence is strictly greater than
    /// `after_sequence`. A `None` continuation means the current end of the run
    /// has been reached; future appends can still be queried from the last item.
    pub fn timeline(
        &self,
        run_id: &RunId,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<TimelinePage, RunQueryError> {
        if limit == 0 {
            return Ok(TimelinePage {
                events: Vec::new(),
                next_after_sequence: None,
            });
        }

        let after = after_sequence.unwrap_or(0);
        let candidates: Vec<_> = self
            .sink
            .events(run_id)?
            .into_iter()
            .filter(|event| event.sequence > after)
            .collect();
        let has_more = candidates.len() > limit;
        let events: Vec<_> = candidates
            .into_iter()
            .take(limit)
            .map(TimelineEvent::from)
            .collect();
        let next_after_sequence = has_more
            .then(|| events.last().map(|event| event.sequence))
            .flatten();

        Ok(TimelinePage {
            events,
            next_after_sequence,
        })
    }

    /// Returns the full normalized envelope for an inspector/detail pane.
    pub fn event_detail(
        &self,
        run_id: &RunId,
        event_id: &EventId,
    ) -> Result<Option<EventEnvelope>, RunQueryError> {
        Ok(self.sink.event(run_id, event_id)?)
    }

    fn fold_summary(&self, run_id: RunId, events: Vec<EventEnvelope>) -> RunSummary {
        let mut objective = None;
        let mut status = RunStatus::Unknown;
        let mut created_at = events.first().map(|event| event.recorded_at.clone());
        let updated_at = events.last().map(|event| event.recorded_at.clone());
        let mut repository_id = None;
        let mut base_commit = None;
        let mut base_manifest_id = None;
        let mut initial_branch_id = None;
        let mut agents = HashSet::new();
        let mut turns = HashSet::new();

        for event in events {
            if let Some(agent_id) = event.agent_id {
                agents.insert(agent_id);
            }
            if let Some(turn_id) = event.turn_id {
                turns.insert(turn_id);
            }

            match event.kind {
                EventKind::RunLifecycle(lifecycle) => {
                    if lifecycle.objective.is_some() {
                        objective = lifecycle.objective;
                    }
                    status = RunStatus::from(lifecycle.state.clone());
                    if lifecycle.state == RunLifecycleState::Created {
                        created_at = Some(event.recorded_at);
                        if let Some(creation) = lifecycle.creation {
                            repository_id = creation.repository_id;
                            base_commit = creation.base_commit;
                            base_manifest_id = creation.base_manifest_id;
                            initial_branch_id = creation.initial_branch_id;
                        }
                    }
                }
                EventKind::AgentLifecycle(lifecycle) => {
                    agents.insert(lifecycle.agent_id);
                }
                _ => {}
            }
        }

        RunSummary {
            run_id,
            objective,
            status,
            created_at,
            updated_at,
            agent_count: agents.len(),
            turn_count: turns.len(),
            repository_id,
            base_commit,
            base_manifest_id,
            initial_branch_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunListCursor {
    pub run_id: RunId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunListPage {
    pub runs: Vec<RunSummary>,
    pub next_cursor: Option<RunListCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: RunId,
    pub objective: Option<String>,
    pub status: RunStatus,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub agent_count: usize,
    pub turn_count: usize,
    pub repository_id: Option<RepositoryId>,
    pub base_commit: Option<String>,
    pub base_manifest_id: Option<BaseManifestId>,
    pub initial_branch_id: Option<BranchId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Unknown,
    Active,
    Completed,
    Interrupted,
    Failed,
    Abandoned,
}

impl From<RunLifecycleState> for RunStatus {
    fn from(state: RunLifecycleState) -> Self {
        match state {
            RunLifecycleState::Created
            | RunLifecycleState::ObjectiveUpdated
            | RunLifecycleState::Recovered => Self::Active,
            RunLifecycleState::Completed => Self::Completed,
            RunLifecycleState::Interrupted => Self::Interrupted,
            RunLifecycleState::Failed => Self::Failed,
            RunLifecycleState::Abandoned => Self::Abandoned,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelinePage {
    pub events: Vec<TimelineEvent>,
    pub next_after_sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub event_id: EventId,
    pub sequence: u64,
    pub recorded_at: String,
    pub caused_by: Option<EventId>,
    pub branch_id: Option<BranchId>,
    pub agent_id: Option<AgentId>,
    pub turn_id: Option<TurnId>,
    pub operation_id: Option<OperationId>,
    pub kind_label: String,
}

impl From<EventEnvelope> for TimelineEvent {
    fn from(event: EventEnvelope) -> Self {
        Self {
            event_id: event.event_id,
            sequence: event.sequence,
            recorded_at: event.recorded_at,
            caused_by: event.caused_by,
            branch_id: event.branch_id,
            agent_id: event.agent_id,
            turn_id: event.turn_id,
            operation_id: event.operation_id,
            kind_label: event_kind_label(&event.kind),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunQueryError {
    Store(RunLogError),
    CursorNotFound(RunId),
}

impl fmt::Display for RunQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::CursorNotFound(run_id) => write!(formatter, "run cursor {run_id} was not found"),
        }
    }
}

impl std::error::Error for RunQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::CursorNotFound(_) => None,
        }
    }
}

impl From<RunLogError> for RunQueryError {
    fn from(error: RunLogError) -> Self {
        Self::Store(error)
    }
}

fn event_kind_label(kind: &EventKind) -> String {
    match kind {
        EventKind::RunLifecycle(event) => format!("run.{}", run_state_label(&event.state)),
        EventKind::BranchLifecycle(event) => {
            format!("branch.{}", branch_state_label(&event.state))
        }
        EventKind::AgentLifecycle(event) => {
            format!("agent.{}", agent_state_label(&event.state))
        }
        EventKind::AgentProvider(event) => {
            format!("agent.provider.{:?}", event.state).to_lowercase()
        }
        EventKind::AgentUsage(_) => "agent.usage".into(),
        EventKind::AgentProgress(_) => "agent.progress".into(),
        EventKind::AgentHandoff(_) => "agent.handoff".into(),
        EventKind::AgentHandoffValidationFailed(_) => "agent.handoff_validation_failed".into(),
        EventKind::AgentAttachment(_) => "agent.attachment".into(),
        EventKind::AgentFollowup(_) => "agent.followup".into(),
        EventKind::AgentApprovalRequested(_) => "agent.approval.requested".into(),
        EventKind::AgentApprovalResolved(_) => "agent.approval.resolved".into(),
        EventKind::AgentMessage(_) => "agent.message.queued".into(),
        EventKind::AgentMessageDelivery(event) => {
            format!("agent.message.{:?}", event.state).to_lowercase()
        }
        EventKind::MailboxNotification(_) => "mailbox.notification".into(),
        EventKind::MailboxConsumed(_) => "mailbox.consumed".into(),
        EventKind::TurnLifecycle(event) => format!("turn.{}", turn_state_label(&event.state)),
        EventKind::Message(event) => format!("message.{}", message_role_label(&event.role)),
        EventKind::ToolIntent(_) => "tool.intent".into(),
        EventKind::ToolDecision(event) => {
            format!("tool.decision.{}", tool_decision_label(&event.decision))
        }
        EventKind::ToolStarted(_) => "tool.started".into(),
        EventKind::ToolResult(event) => {
            format!("tool.result.{}", tool_outcome_label(&event.outcome))
        }
        EventKind::FileMutation(_) => "file.mutation".into(),
        EventKind::Checkpoint(_) => "checkpoint".into(),
        EventKind::Divergence(_) => "divergence".into(),
        EventKind::Unknown { name, .. } => name.clone(),
    }
}

fn run_state_label(state: &RunLifecycleState) -> &'static str {
    match state {
        RunLifecycleState::Created => "created",
        RunLifecycleState::ObjectiveUpdated => "objective_updated",
        RunLifecycleState::Completed => "completed",
        RunLifecycleState::Interrupted => "interrupted",
        RunLifecycleState::Failed => "failed",
        RunLifecycleState::Abandoned => "abandoned",
        RunLifecycleState::Recovered => "recovered",
    }
}

fn branch_state_label(state: &BranchLifecycleState) -> &'static str {
    match state {
        BranchLifecycleState::Created => "created",
        BranchLifecycleState::Forked => "forked",
        BranchLifecycleState::Selected => "selected",
    }
}

fn agent_state_label(state: &AgentLifecycleState) -> &'static str {
    match state {
        AgentLifecycleState::Created => "created",
        AgentLifecycleState::Dispatched => "dispatched",
        AgentLifecycleState::Started => "started",
        AgentLifecycleState::Queued => "queued",
        AgentLifecycleState::Starting => "starting",
        AgentLifecycleState::Running => "running",
        AgentLifecycleState::WaitingForAgent => "waiting_for_agent",
        AgentLifecycleState::WaitingForTool => "waiting_for_tool",
        AgentLifecycleState::WaitingForUser => "waiting_for_user",
        AgentLifecycleState::Waiting => "waiting",
        AgentLifecycleState::Blocked => "blocked",
        AgentLifecycleState::Completed => "completed",
        AgentLifecycleState::Interrupted => "interrupted",
        AgentLifecycleState::Failed => "failed",
    }
}

fn turn_state_label(state: &TurnLifecycleState) -> &'static str {
    match state {
        TurnLifecycleState::Started => "started",
        TurnLifecycleState::Steered => "steered",
        TurnLifecycleState::Completed => "completed",
        TurnLifecycleState::Interrupted => "interrupted",
        TurnLifecycleState::Failed => "failed",
    }
}

fn message_role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Agent => "agent",
        MessageRole::ReasoningSummary => "reasoning_summary",
        MessageRole::System => "system",
    }
}

fn tool_decision_label(decision: &ToolDecision) -> &'static str {
    match decision {
        ToolDecision::Allowed => "allowed",
        ToolDecision::Denied => "denied",
        ToolDecision::Escalated => "escalated",
        ToolDecision::RequiresUser => "requires_user",
    }
}

fn tool_outcome_label(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Completed => "completed",
        ToolOutcome::Failed => "failed",
        ToolOutcome::Interrupted => "interrupted",
        ToolOutcome::UnknownAfterCrash => "unknown_after_crash",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::{
        AgentLifecycleEvent, EventActor, EventClock, EventIdGenerator, InMemoryRunEventSink,
        MessageEvent, NewRunEvent, RunCreationMetadata, RunLifecycleEvent, TurnLifecycleEvent,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct SequentialIds(AtomicU64);

    impl EventIdGenerator for SequentialIds {
        fn next_event_id(&self) -> EventId {
            EventId::parse(format!(
                "evt_query_{:04}",
                self.0.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap()
        }
    }

    struct SequentialClock(AtomicU64);

    impl EventClock for SequentialClock {
        fn now(&self) -> String {
            format!(
                "2026-07-13T12:00:{:02}Z",
                self.0.fetch_add(1, Ordering::Relaxed)
            )
        }
    }

    fn sink() -> Arc<InMemoryRunEventSink> {
        Arc::new(InMemoryRunEventSink::with_sources(
            Arc::new(SequentialIds(AtomicU64::new(1))),
            Arc::new(SequentialClock(AtomicU64::new(1))),
        ))
    }

    fn append_created(
        sink: &InMemoryRunEventSink,
        run_id: RunId,
        objective: &str,
    ) -> EventEnvelope {
        sink.append(NewRunEvent::new(
            run_id,
            EventActor::User,
            EventKind::RunLifecycle(RunLifecycleEvent {
                state: RunLifecycleState::Created,
                objective: Some(objective.into()),
                detail: None,
                creation: Some(RunCreationMetadata {
                    repository_id: Some(RepositoryId::parse("repo_query").unwrap()),
                    base_commit: Some("abc123".into()),
                    base_manifest_id: Some(BaseManifestId::parse("bsm_query").unwrap()),
                    initial_branch_id: Some(BranchId::parse("brn_main").unwrap()),
                }),
            }),
        ))
        .unwrap()
    }

    #[test]
    fn paginates_in_stable_creation_order_and_folds_summaries() {
        let sink = sink();
        let first_run = RunId::parse("run_first").unwrap();
        let second_run = RunId::parse("run_second").unwrap();
        append_created(&sink, first_run.clone(), "build replay");
        let agent_id = AgentId::parse("agt_query").unwrap();
        let turn_id = TurnId::parse("trn_query").unwrap();
        sink.append(
            NewRunEvent::new(
                first_run.clone(),
                EventActor::System("runtime".into()),
                EventKind::AgentLifecycle(AgentLifecycleEvent {
                    agent_id: agent_id.clone(),
                    parent_agent_id: None,
                    state: AgentLifecycleState::Started,
                    kind: "primary".into(),
                    objective: None,
                    detail: None,
                    dispatch_spec: None,
                }),
            )
            .for_agent(agent_id)
            .in_turn(turn_id),
        )
        .unwrap();
        append_created(&sink, second_run.clone(), "ship it");
        sink.append(NewRunEvent::new(
            second_run.clone(),
            EventActor::System("runtime".into()),
            EventKind::RunLifecycle(RunLifecycleEvent {
                state: RunLifecycleState::Completed,
                objective: None,
                detail: None,
                creation: None,
            }),
        ))
        .unwrap();

        let query = RunHistoryQuery::new(sink);
        let first_page = query.list_runs(None, 1).unwrap();
        assert_eq!(first_page.runs.len(), 1);
        let summary = &first_page.runs[0];
        assert_eq!(summary.run_id, first_run);
        assert_eq!(summary.objective.as_deref(), Some("build replay"));
        assert_eq!(summary.status, RunStatus::Active);
        assert_eq!(summary.agent_count, 1);
        assert_eq!(summary.turn_count, 1);
        assert_eq!(summary.base_commit.as_deref(), Some("abc123"));
        assert_eq!(
            summary.repository_id.as_ref().map(RepositoryId::as_str),
            Some("repo_query")
        );

        let second_page = query.list_runs(first_page.next_cursor.as_ref(), 1).unwrap();
        assert_eq!(second_page.runs[0].run_id, second_run);
        assert_eq!(second_page.runs[0].status, RunStatus::Completed);
        assert_eq!(second_page.next_cursor, None);
    }

    #[test]
    fn timeline_is_sequence_paginated_and_preserves_correlation_ids() {
        let sink = sink();
        let run_id = RunId::parse("run_timeline").unwrap();
        append_created(&sink, run_id.clone(), "inspect");
        let branch_id = BranchId::parse("brn_timeline").unwrap();
        let agent_id = AgentId::parse("agt_timeline").unwrap();
        let turn_id = TurnId::parse("trn_timeline").unwrap();
        let operation_id = OperationId::parse("op_timeline").unwrap();
        let message = sink
            .append(
                NewRunEvent::new(
                    run_id.clone(),
                    EventActor::Agent(agent_id.clone()),
                    EventKind::Message(MessageEvent {
                        role: MessageRole::Agent,
                        content: "working".into(),
                    }),
                )
                .for_agent(agent_id.clone())
                .in_turn(turn_id.clone())
                .in_branch(branch_id.clone())
                .for_operation(operation_id.clone()),
            )
            .unwrap();
        sink.append(NewRunEvent::new(
            run_id.clone(),
            EventActor::System("extension".into()),
            EventKind::Unknown {
                name: "extension.observed".into(),
                payload: json!({"ok": true}),
            },
        ))
        .unwrap();

        let query = RunHistoryQuery::new(sink);
        let page = query.timeline(&run_id, Some(1), 1).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event_id, message.event_id);
        assert_eq!(page.events[0].kind_label, "message.agent");
        assert_eq!(page.events[0].branch_id.as_ref(), Some(&branch_id));
        assert_eq!(page.events[0].agent_id.as_ref(), Some(&agent_id));
        assert_eq!(page.events[0].turn_id.as_ref(), Some(&turn_id));
        assert_eq!(page.events[0].operation_id.as_ref(), Some(&operation_id));
        assert_eq!(page.next_after_sequence, Some(2));

        let final_page = query
            .timeline(&run_id, page.next_after_sequence, 10)
            .unwrap();
        assert_eq!(final_page.events[0].kind_label, "extension.observed");
        assert_eq!(final_page.next_after_sequence, None);
    }

    #[test]
    fn incomplete_and_unknown_runs_remain_queryable() {
        let sink = sink();
        let run_id = RunId::parse("run_crashed").unwrap();
        let event = sink
            .append(NewRunEvent::new(
                run_id.clone(),
                EventActor::User,
                EventKind::Message(MessageEvent {
                    role: MessageRole::User,
                    content: "continue".into(),
                }),
            ))
            .unwrap();
        let query = RunHistoryQuery::new(sink);

        let summary = query.run_summary(&run_id).unwrap().unwrap();
        assert_eq!(summary.status, RunStatus::Unknown);
        assert_eq!(
            summary.created_at.as_deref(),
            Some(event.recorded_at.as_str())
        );
        assert_eq!(summary.updated_at, summary.created_at);
        assert_eq!(
            query.event_detail(&run_id, &event.event_id).unwrap(),
            Some(event)
        );
    }

    #[test]
    fn counts_turns_even_when_only_a_terminal_turn_event_survived() {
        let sink = sink();
        let run_id = RunId::parse("run_partial_turn").unwrap();
        let turn_id = TurnId::parse("trn_partial").unwrap();
        sink.append(
            NewRunEvent::new(
                run_id.clone(),
                EventActor::System("recovery".into()),
                EventKind::TurnLifecycle(TurnLifecycleEvent {
                    state: TurnLifecycleState::Interrupted,
                    detail: Some("process exited".into()),
                }),
            )
            .in_turn(turn_id),
        )
        .unwrap();

        let query = RunHistoryQuery::new(sink);
        assert_eq!(query.run_summary(&run_id).unwrap().unwrap().turn_count, 1);
    }
}
