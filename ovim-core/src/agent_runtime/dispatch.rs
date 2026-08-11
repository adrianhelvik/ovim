//! Provider-independent multi-agent dispatch and scheduling policy.
//!
//! This module allocates ovim agent identities and records scheduling state. It
//! deliberately does not start provider sessions or create Git worktrees.

use super::{
    AgentLoopBudget, HandoffConfidence, HandoffStatus, HandoffValidator, ModelFallbackPolicy,
    ModelRouteError, ModelRouteResolution, ReasoningEffort, RequestedModelRoute,
    ResolvedModelRoute, StructuredHandoffV1, SubagentModelCatalog, ValidatedHandoff,
};
use crate::run_log::{
    AgentCapabilitySnapshot, AgentCompletionContractSnapshot, AgentDispatchSpecSnapshot,
    AgentFollowupBudgetSnapshot, AgentFollowupEvent, AgentHandoffEvent, AgentId,
    AgentLifecycleEvent, AgentLifecycleState, AgentModelEffortSnapshot,
    AgentModelFallbackPolicySnapshot, AgentModelRouteResolutionSnapshot,
    AgentRequestedModelRouteSnapshot, AgentResolvedModelRouteSnapshot,
    AgentWorkspacePolicySnapshot, AgentWorkspaceStrategySnapshot, EventActor, EventEnvelope,
    EventId, EventKind, ManifestId, NewRunEvent, OperationId, RunEventSink, RunId, RunLogError,
    ToolOutcome, TurnId, WorkspaceId, AGENT_FOLLOWUP_EVENT_VERSION,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentCapability {
    Read,
    Navigate,
    SafeShell,
    Shell,
    WorkspaceWrite,
    ExternalEffects,
    DispatchAgents,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentKindName {
    Implementer,
    Explorer,
    Verifier,
    Reviewer,
    Safety,
    Planner,
    Custom(String),
}

impl fmt::Display for AgentKindName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Implementer => formatter.write_str("implementer"),
            Self::Explorer => formatter.write_str("explorer"),
            Self::Verifier => formatter.write_str("verifier"),
            Self::Reviewer => formatter.write_str("reviewer"),
            Self::Safety => formatter.write_str("safety"),
            Self::Planner => formatter.write_str("planner"),
            Self::Custom(name) => write!(formatter, "custom:{name}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspacePolicy {
    SharedWorkspace,
    IsolatedWorktree,
    ReadOnlyProjection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionContract {
    StructuredHandoff,
    ReviewReport,
    SafetyVerdict,
    Plan,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRoleTemplate {
    pub name: AgentKindName,
    pub instructions: String,
    pub capabilities: BTreeSet<AgentCapability>,
    pub workspace_policy: WorkspacePolicy,
    pub completion_contract: CompletionContract,
}

impl AgentRoleTemplate {
    pub fn built_in(name: AgentKindName) -> Self {
        let capabilities = |values: &[AgentCapability]| values.iter().cloned().collect();
        match name {
            AgentKindName::Implementer => Self {
                name,
                instructions: "Implement the delegated objective and verify recorded changes."
                    .into(),
                capabilities: capabilities(&[
                    AgentCapability::Read,
                    AgentCapability::Navigate,
                    AgentCapability::Shell,
                    AgentCapability::WorkspaceWrite,
                ]),
                workspace_policy: WorkspacePolicy::IsolatedWorktree,
                completion_contract: CompletionContract::StructuredHandoff,
            },
            AgentKindName::Explorer => Self {
                name,
                instructions: "Explore the delegated question without mutating source.".into(),
                capabilities: capabilities(&[
                    AgentCapability::Read,
                    AgentCapability::Navigate,
                    AgentCapability::SafeShell,
                ]),
                workspace_policy: WorkspacePolicy::ReadOnlyProjection,
                completion_contract: CompletionContract::ReviewReport,
            },
            AgentKindName::Verifier => Self {
                name,
                instructions: "Run verification and report evidence and failures.".into(),
                capabilities: capabilities(&[
                    AgentCapability::Read,
                    AgentCapability::Navigate,
                    AgentCapability::Shell,
                ]),
                workspace_policy: WorkspacePolicy::IsolatedWorktree,
                completion_contract: CompletionContract::StructuredHandoff,
            },
            AgentKindName::Reviewer => Self {
                name,
                instructions: "Review the selected source state without changing it.".into(),
                capabilities: capabilities(&[AgentCapability::Read, AgentCapability::Navigate]),
                workspace_policy: WorkspacePolicy::ReadOnlyProjection,
                completion_contract: CompletionContract::ReviewReport,
            },
            AgentKindName::Safety => Self {
                name,
                instructions: "Classify the proposed action against explicit user authorization."
                    .into(),
                capabilities: capabilities(&[AgentCapability::Read]),
                workspace_policy: WorkspacePolicy::ReadOnlyProjection,
                completion_contract: CompletionContract::SafetyVerdict,
            },
            AgentKindName::Planner => Self {
                name,
                instructions: "Decompose objectives and dispatch appropriately scoped agents."
                    .into(),
                capabilities: capabilities(&[
                    AgentCapability::Read,
                    AgentCapability::Navigate,
                    AgentCapability::DispatchAgents,
                ]),
                workspace_policy: WorkspacePolicy::ReadOnlyProjection,
                completion_contract: CompletionContract::Plan,
            },
            AgentKindName::Custom(_) => Self {
                name,
                instructions: String::new(),
                capabilities: BTreeSet::new(),
                workspace_policy: WorkspacePolicy::ReadOnlyProjection,
                completion_contract: CompletionContract::Custom("unspecified".into()),
            },
        }
    }

    pub fn can_write(&self) -> bool {
        self.capabilities.contains(&AgentCapability::WorkspaceWrite)
            // Arbitrary shell can mutate files even when the agent's primary
            // role is verification rather than implementation.
            || self.capabilities.contains(&AgentCapability::Shell)
    }
}

/// Compatibility name for callers that constructed role presets before
/// routing moved to `DispatchRequest`. It no longer contains model policy.
pub type AgentKind = AgentRoleTemplate;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceStrategy {
    SharedWorkspace,
    IsolatedWorktree {
        base_manifest_id: Option<ManifestId>,
    },
    ReadOnlySnapshot {
        manifest_id: Option<ManifestId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAssignment {
    pub workspace_id: WorkspaceId,
    pub strategy: WorkspaceStrategy,
}

#[derive(Clone, Debug)]
pub struct DispatchRequest {
    pub task_name: String,
    pub objective: String,
    pub role: AgentRoleTemplate,
    pub requested_route: RequestedModelRoute,
    pub parent_agent_id: Option<AgentId>,
    pub causing_turn_id: Option<TurnId>,
    pub caused_by_event: Option<EventId>,
    pub workspace: WorkspaceAssignment,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchHandle {
    pub run_id: RunId,
    pub agent_id: AgentId,
    pub workspace: WorkspaceAssignment,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchState {
    Created,
    Queued,
    Starting,
    Running,
    WaitingForAgent,
    WaitingForTool,
    WaitingForUser,
    Completed,
    Interrupted,
    Failed,
}

impl DispatchState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Interrupted | Self::Failed)
    }
}

#[derive(Clone, Debug)]
struct ScheduledAgent {
    handle: DispatchHandle,
    role: AgentRoleTemplate,
    requested_route: RequestedModelRoute,
    resolved_route: ResolvedModelRoute,
    task_name: String,
    objective: String,
    parent_agent_id: Option<AgentId>,
    causing_turn_id: Option<TurnId>,
    turn_generation: u32,
    followup: Option<AgentFollowupTurn>,
    followup_authorized: bool,
    state: DispatchState,
    queue_sequence: u64,
    last_event_id: EventId,
    pending_handoff: Option<PendingHandoff>,
    runtime_operations: HashMap<OperationId, RuntimeOperationState>,
}

/// A validated handoff that is durable but whose terminal lifecycle event has
/// not been appended yet — the window between the two appends in
/// `finish_with_handoff_and_warnings`. The envelope is retained so the
/// transition can be resumed in-process via `complete_pending_handoff`
/// (with the parent still notified) instead of waiting for a restart
/// rehydrate. (OV-00318)
#[derive(Clone, Debug)]
struct PendingHandoff {
    status: HandoffStatus,
    handoff_event: EventEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFollowupTurn {
    pub turn_generation: u32,
    pub followup_turn_id: TurnId,
    pub parent_turn_id: TurnId,
    pub parent_event_id: EventId,
    pub prior_terminal_event_id: EventId,
    pub prior_handoff_event_id: EventId,
    pub objective: String,
    pub prior_handoff_summary: String,
    pub budget: AgentLoopBudget,
    pub retained_session_requested: bool,
}

#[derive(Clone, Debug)]
pub struct FollowupAgentRequest {
    pub agent_id: AgentId,
    pub parent_agent_id: AgentId,
    pub causing_turn_id: TurnId,
    pub caused_by_event: EventId,
    pub objective: String,
    /// Omitted to preserve the current capability ceiling. A supplied set may
    /// only narrow it.
    pub capabilities: Option<BTreeSet<AgentCapability>>,
    pub budget: AgentLoopBudget,
    pub retained_session_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowupAgentHandle {
    pub handle: DispatchHandle,
    pub followup_turn_id: TurnId,
    pub turn_generation: u32,
    pub followup_event_id: EventId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeOperationState {
    Intended,
    Started,
    Terminal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchTerminalRecord {
    pub handoff_event: EventEnvelope,
    pub terminal_event: EventEnvelope,
}

pub struct AgentDispatchScheduler {
    run_id: RunId,
    sink: Arc<dyn RunEventSink>,
    model_catalog: Arc<SubagentModelCatalog>,
    external_parent: Option<AgentId>,
    agents: HashMap<AgentId, ScheduledAgent>,
    shared_writer: HashMap<WorkspaceId, AgentId>,
}

impl AgentDispatchScheduler {
    pub fn new(
        run_id: RunId,
        sink: Arc<dyn RunEventSink>,
        model_catalog: Arc<SubagentModelCatalog>,
    ) -> Self {
        Self {
            run_id,
            sink,
            model_catalog,
            external_parent: None,
            agents: HashMap::new(),
            shared_writer: HashMap::new(),
        }
    }

    /// Admit one root agent owned by the interactive runtime rather than this
    /// scheduler. Child lifecycle still records that exact parent and causing
    /// turn; only execution ownership stays outside the delegated scheduler.
    pub fn set_external_parent(&mut self, parent: AgentId) {
        self.external_parent = Some(parent);
    }

    /// Rebuild scheduler ownership from normalized durable lifecycle events.
    /// Queued work remains recoverable. Any state that may have crossed a
    /// provider/effect boundary is durably interrupted because this process
    /// cannot prove the old provider session is still attached.
    pub fn rehydrate(
        run_id: RunId,
        sink: Arc<dyn RunEventSink>,
        model_catalog: Arc<SubagentModelCatalog>,
    ) -> Result<Self, DispatchError> {
        let mut events = sink.events(&run_id).map_err(DispatchError::RunLog)?;
        events.sort_by_key(|event| event.sequence);
        let mut scheduler = Self::new(run_id.clone(), sink, model_catalog);
        let mut ignored_agents = BTreeSet::new();

        for event in &events {
            if event.run_id != run_id {
                return Err(DispatchError::InvalidHistory(format!(
                    "event {} belongs to run {}, expected {}",
                    event.event_id, event.run_id, run_id
                )));
            }
            if let EventKind::AgentHandoff(recorded) = &event.kind {
                let agent_id = event.agent_id.as_ref().ok_or_else(|| {
                    DispatchError::InvalidHistory(format!(
                        "handoff {} has no agent identity",
                        event.event_id
                    ))
                })?;
                if ignored_agents.contains(agent_id) {
                    continue;
                }
                let agent = scheduler.agents.get_mut(agent_id).ok_or_else(|| {
                    DispatchError::InvalidHistory(format!(
                        "handoff {} belongs to unknown scheduler agent {}",
                        event.event_id, agent_id
                    ))
                })?;
                if event.workspace_id.as_ref() != Some(&agent.handle.workspace.workspace_id) {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} handoff changed workspace identity",
                        agent_id
                    )));
                }
                if event.caused_by.as_ref() != Some(&agent.last_event_id) {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} handoff is not causally contiguous",
                        agent_id
                    )));
                }
                if agent.pending_handoff.is_some() {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} recorded more than one pending handoff",
                        agent_id
                    )));
                }
                if agent
                    .runtime_operations
                    .values()
                    .any(|state| *state != RuntimeOperationState::Terminal)
                {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {agent_id} recorded a handoff with an outstanding tool operation"
                    )));
                }
                let target = terminal_state_for_handoff(recorded.handoff.status());
                if !valid_transition(&agent.state, &target) {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} recorded a handoff from invalid state {:?}",
                        agent_id, agent.state
                    )));
                }
                agent.last_event_id = event.event_id.clone();
                agent.pending_handoff = Some(PendingHandoff {
                    status: recorded.handoff.status(),
                    handoff_event: event.clone(),
                });
                continue;
            }

            if let EventKind::AgentFollowup(followup) = &event.kind {
                scheduler.apply_rehydrated_followup(event, followup)?;
                continue;
            }

            let EventKind::AgentLifecycle(lifecycle) = &event.kind else {
                if let Some(agent_id) = event.agent_id.as_ref()
                    && scheduler.agents.contains_key(agent_id)
                    && is_runtime_trace_event(&event.kind)
                {
                    scheduler.apply_rehydrated_runtime_event(event)?;
                }
                continue;
            };
            if event.agent_id.as_ref() != Some(&lifecycle.agent_id) {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent lifecycle {} disagrees with its envelope",
                    lifecycle.agent_id
                )));
            }

            if !scheduler.agents.contains_key(&lifecycle.agent_id) {
                let Some(spec) = lifecycle.dispatch_spec.as_ref() else {
                    // Root/provider agents are normalized by the runtime too,
                    // but are outside scheduler ownership.
                    ignored_agents.insert(lifecycle.agent_id.clone());
                    continue;
                };
                if lifecycle.state != AgentLifecycleState::Created {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} has a dispatch spec outside Created",
                        lifecycle.agent_id
                    )));
                }
                let workspace_id = event.workspace_id.clone().ok_or_else(|| {
                    DispatchError::InvalidHistory(format!(
                        "agent {} has no persisted workspace identity",
                        lifecycle.agent_id
                    ))
                })?;
                let objective = lifecycle
                    .objective
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        DispatchError::InvalidHistory(format!(
                            "agent {} has no persisted objective",
                            lifecycle.agent_id
                        ))
                    })?;
                let (role, strategy, requested_route, resolved_route) =
                    restore_dispatch_spec(&lifecycle.kind, spec)?;
                if role.can_write()
                    && matches!(strategy, WorkspaceStrategy::ReadOnlySnapshot { .. })
                {
                    return Err(DispatchError::InvalidHistory(format!(
                        "write-capable agent {} was persisted in a read-only workspace",
                        lifecycle.agent_id
                    )));
                }
                scheduler.agents.insert(
                    lifecycle.agent_id.clone(),
                    ScheduledAgent {
                        handle: DispatchHandle {
                            run_id: run_id.clone(),
                            agent_id: lifecycle.agent_id.clone(),
                            workspace: WorkspaceAssignment {
                                workspace_id,
                                strategy,
                            },
                        },
                        role,
                        requested_route,
                        resolved_route,
                        task_name: spec
                            .task_name
                            .clone()
                            .unwrap_or_else(|| lifecycle.agent_id.to_string()),
                        objective,
                        parent_agent_id: lifecycle.parent_agent_id.clone(),
                        causing_turn_id: event.turn_id.clone(),
                        turn_generation: 0,
                        followup: None,
                        followup_authorized: false,
                        state: DispatchState::Created,
                        queue_sequence: event.sequence,
                        last_event_id: event.event_id.clone(),
                        pending_handoff: None,
                        runtime_operations: HashMap::new(),
                    },
                );
                continue;
            }
            if ignored_agents.contains(&lifecycle.agent_id) {
                continue;
            }

            let agent = scheduler
                .agents
                .get_mut(&lifecycle.agent_id)
                .expect("scheduler agent checked above");
            if lifecycle.dispatch_spec.is_some() {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent {} repeats its dispatch spec",
                    lifecycle.agent_id
                )));
            }
            if event.workspace_id.as_ref() != Some(&agent.handle.workspace.workspace_id) {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent {} changed workspace identity",
                    lifecycle.agent_id
                )));
            }
            if lifecycle.parent_agent_id != agent.parent_agent_id
                || event.turn_id != agent.causing_turn_id
                || lifecycle.kind != agent.role.name.to_string()
                || lifecycle.objective.as_deref() != Some(agent.objective.as_str())
            {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent {} changed immutable dispatch identity",
                    lifecycle.agent_id
                )));
            }
            if event.caused_by.as_ref() != Some(&agent.last_event_id) {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent {} lifecycle is not causally contiguous",
                    lifecycle.agent_id
                )));
            }
            let next = projected_state(&lifecycle.state);
            match (agent.pending_handoff.as_ref().map(|p| p.status), &next) {
                (None, DispatchState::Completed) => {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} completed without a validated handoff",
                        lifecycle.agent_id
                    )));
                }
                (Some(status), next) if *next != terminal_state_for_handoff(status) => {
                    return Err(DispatchError::InvalidHistory(format!(
                        "agent {} lifecycle {:?} contradicts handoff status {:?}",
                        lifecycle.agent_id, next, status
                    )));
                }
                _ => {}
            }
            let legacy_start = agent.state == DispatchState::Queued
                && lifecycle.state == AgentLifecycleState::Started;
            let authorized_followup = agent.followup_authorized
                && agent.state.is_terminal()
                && next == DispatchState::Queued;
            if !legacy_start && !authorized_followup && !valid_transition(&agent.state, &next) {
                return Err(DispatchError::InvalidHistory(format!(
                    "agent {} has invalid transition {:?} -> {:?}",
                    lifecycle.agent_id, agent.state, next
                )));
            }
            agent.state = next.clone();
            if next == DispatchState::Queued {
                agent.queue_sequence = event.sequence;
            }
            if authorized_followup {
                agent.followup_authorized = false;
            }
            agent.last_event_id = event.event_id.clone();
            if next.is_terminal() {
                agent.pending_handoff = None;
            }
        }

        // A crash can land between the durable validated handoff and its
        // lifecycle event. The handoff is already the completion gate, so
        // recovery finishes that exact terminal transition rather than
        // downgrading it to an ambiguous interruption.
        let pending_handoffs = scheduler
            .agents
            .values()
            .filter_map(|agent| {
                agent
                    .pending_handoff
                    .as_ref()
                    .map(|pending| (agent.handle.clone(), pending.status))
            })
            .collect::<Vec<_>>();
        for (handle, status) in pending_handoffs {
            scheduler.transition_internal(
                &handle,
                terminal_state_for_handoff(status),
                Some("recovered terminal state from durable validated handoff".into()),
                true,
                false,
            )?;
        }

        // A crash can also land after durable follow-up authorization and
        // before the Queued lifecycle. Resume only that never-started turn.
        let pending_followups = scheduler
            .agents
            .values()
            .filter(|agent| agent.followup_authorized && agent.state.is_terminal())
            .map(|agent| agent.handle.clone())
            .collect::<Vec<_>>();
        for handle in pending_followups {
            scheduler.transition_internal(
                &handle,
                DispatchState::Queued,
                Some("recovered queued state from durable follow-up authorization".into()),
                false,
                true,
            )?;
        }

        let ambiguous = scheduler
            .agents
            .values()
            .filter(|agent| {
                matches!(
                    agent.state,
                    DispatchState::Created
                        | DispatchState::Starting
                        | DispatchState::Running
                        | DispatchState::WaitingForAgent
                        | DispatchState::WaitingForTool
                        | DispatchState::WaitingForUser
                )
            })
            .map(|agent| agent.handle.clone())
            .collect::<Vec<_>>();
        for handle in ambiguous {
            scheduler.terminate_conservatively(
                &handle,
                HandoffStatus::Interrupted,
                "interrupted while recovering scheduler after process restart",
            )?;
        }

        // A never-started child may be retried only if the current catalog
        // still resolves the exact effective route recorded at dispatch.
        // Configuration drift must not silently reroute durable queued work.
        let invalid_queued_routes = scheduler
            .agents
            .values()
            .filter(|agent| agent.state == DispatchState::Queued)
            .filter_map(|agent| {
                let current = scheduler
                    .model_catalog
                    .resolve(&agent.requested_route, !agent.role.capabilities.is_empty());
                match current {
                    Ok(current) if same_effective_route(&current, &agent.resolved_route) => None,
                    Ok(_) => Some((
                        agent.handle.clone(),
                        "queued child route changed under the current model catalog".to_string(),
                    )),
                    Err(error) => Some((
                        agent.handle.clone(),
                        format!(
                            "queued child route is unavailable under the current model catalog: {error}"
                        ),
                    )),
                }
            })
            .collect::<Vec<_>>();
        for (handle, detail) in invalid_queued_routes {
            scheduler.terminate_conservatively(&handle, HandoffStatus::Interrupted, &detail)?;
        }

        for agent in scheduler.agents.values() {
            if agent.state.is_terminal()
                || !agent.role.can_write()
                || !matches!(
                    agent.handle.workspace.strategy,
                    WorkspaceStrategy::SharedWorkspace
                )
            {
                continue;
            }
            if let Some(holder) = scheduler.shared_writer.insert(
                agent.handle.workspace.workspace_id.clone(),
                agent.handle.agent_id.clone(),
            ) {
                return Err(DispatchError::InvalidHistory(format!(
                    "shared workspace {} has nonterminal writers {} and {}",
                    agent.handle.workspace.workspace_id, holder, agent.handle.agent_id
                )));
            }
        }
        Ok(scheduler)
    }

    pub fn dispatch(&mut self, request: DispatchRequest) -> Result<DispatchHandle, DispatchError> {
        self.validate_request(&request)?;
        let resolved_route = self
            .model_catalog
            .resolve(
                &request.requested_route,
                !request.role.capabilities.is_empty(),
            )
            .map_err(DispatchError::ModelRoute)?;
        let agent_id = AgentId::new();
        if request.role.can_write()
            && matches!(
                request.workspace.strategy,
                WorkspaceStrategy::SharedWorkspace
            )
        {
            if let Some(holder) = self.shared_writer.get(&request.workspace.workspace_id) {
                return Err(DispatchError::SharedWorkspaceWriterHeld {
                    workspace_id: request.workspace.workspace_id.clone(),
                    holder: holder.clone(),
                });
            }
        }
        let handle = DispatchHandle {
            run_id: self.run_id.clone(),
            agent_id: agent_id.clone(),
            workspace: request.workspace.clone(),
        };
        let created_event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: request.caused_by_event.clone(),
                operation_id: None,
                provider_call_id: None,
                actor: request
                    .parent_agent_id
                    .clone()
                    .map(EventActor::Agent)
                    .unwrap_or_else(|| EventActor::System("agent_scheduler".into())),
                agent_id: Some(agent_id.clone()),
                turn_id: request.causing_turn_id.clone(),
                workspace_id: Some(request.workspace.workspace_id.clone()),
                branch_id: None,
                kind: lifecycle_event(
                    &agent_id,
                    request.parent_agent_id.clone(),
                    &request.role,
                    Some(request.objective.clone()),
                    Some(dispatch_spec_snapshot(
                        &request.role,
                        &request.task_name,
                        &request.requested_route,
                        &resolved_route,
                        &request.workspace.strategy,
                    )),
                    DispatchState::Created,
                    None,
                ),
            })
            .map_err(DispatchError::RunLog)?;
        let queued_event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: Some(created_event.event_id.clone()),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("agent_scheduler".into()),
                agent_id: Some(agent_id.clone()),
                turn_id: request.causing_turn_id.clone(),
                workspace_id: Some(request.workspace.workspace_id.clone()),
                branch_id: None,
                kind: lifecycle_event(
                    &agent_id,
                    request.parent_agent_id.clone(),
                    &request.role,
                    Some(request.objective.clone()),
                    None,
                    DispatchState::Queued,
                    None,
                ),
            })
            .map_err(|source| DispatchError::QueueAfterCreationFailed {
                agent_id: agent_id.clone(),
                created_event_id: created_event.event_id,
                source,
            })?;
        if request.role.can_write()
            && matches!(
                request.workspace.strategy,
                WorkspaceStrategy::SharedWorkspace
            )
        {
            self.shared_writer
                .insert(request.workspace.workspace_id.clone(), agent_id.clone());
        }
        self.agents.insert(
            agent_id,
            ScheduledAgent {
                handle: handle.clone(),
                role: request.role,
                requested_route: request.requested_route,
                resolved_route,
                task_name: request.task_name,
                objective: request.objective,
                parent_agent_id: request.parent_agent_id,
                causing_turn_id: request.causing_turn_id,
                turn_generation: 0,
                followup: None,
                followup_authorized: false,
                state: DispatchState::Queued,
                queue_sequence: queued_event.sequence,
                last_event_id: queued_event.event_id,
                pending_handoff: None,
                runtime_operations: HashMap::new(),
            },
        );
        Ok(handle)
    }

    /// Reopen one completed/interrupted child as a fresh turn on the same
    /// Ovim identity. The authorization event is durable before Queued, so a
    /// stop at either edge is replayable without interpreting a terminal child
    /// as still running.
    pub fn begin_followup(
        &mut self,
        request: FollowupAgentRequest,
    ) -> Result<FollowupAgentHandle, DispatchError> {
        if request.objective.trim().is_empty() || request.objective.len() > 8 * 1024 {
            return Err(DispatchError::InvalidFollowupObjective);
        }
        if request.budget.timeout.is_zero()
            || request.budget.max_provider_events == 0
            || request.budget.max_tool_calls == 0
        {
            return Err(DispatchError::InvalidFollowupBudget);
        }
        let agent = self
            .agents
            .get(&request.agent_id)
            .ok_or_else(|| DispatchError::UnknownAgent(request.agent_id.clone()))?;
        if !matches!(
            agent.state,
            DispatchState::Completed | DispatchState::Interrupted
        ) || agent.followup_authorized
        {
            return Err(DispatchError::FollowupIneligible {
                agent_id: request.agent_id,
                state: agent.state.clone(),
            });
        }
        let expected_parent = agent
            .parent_agent_id
            .as_ref()
            .or(self.external_parent.as_ref())
            .ok_or_else(|| DispatchError::UnknownParent(request.parent_agent_id.clone()))?;
        if expected_parent != &request.parent_agent_id {
            return Err(DispatchError::FollowupParentMismatch {
                agent_id: request.agent_id,
                parent_agent_id: request.parent_agent_id,
            });
        }
        let parent_event = self
            .sink
            .event(&self.run_id, &request.caused_by_event)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| DispatchError::CausingEventNotFound(request.caused_by_event.clone()))?;
        if parent_event.agent_id.as_ref() != Some(expected_parent)
            || parent_event.turn_id.as_ref() != Some(&request.causing_turn_id)
        {
            return Err(DispatchError::FollowupParentCauseMismatch(
                request.caused_by_event,
            ));
        }
        let capabilities = request
            .capabilities
            .unwrap_or_else(|| agent.role.capabilities.clone());
        if capabilities.is_empty() || !capabilities.is_subset(&agent.role.capabilities) {
            return Err(DispatchError::FollowupCapabilityWidening(
                agent.handle.agent_id.clone(),
            ));
        }
        let resolved = self
            .model_catalog
            .resolve(&agent.requested_route, true)
            .map_err(DispatchError::ModelRoute)?;
        if !same_effective_route(&resolved, &agent.resolved_route) {
            return Err(DispatchError::FollowupRouteChanged(
                agent.handle.agent_id.clone(),
            ));
        }
        let prior_terminal_event = self
            .sink
            .event(&self.run_id, &agent.last_event_id)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| DispatchError::InvalidHistory("terminal event is missing".into()))?;
        let prior_handoff_event_id = prior_terminal_event.caused_by.clone().ok_or_else(|| {
            DispatchError::InvalidHistory("terminal event has no handoff cause".into())
        })?;
        let prior_handoff_event = self
            .sink
            .event(&self.run_id, &prior_handoff_event_id)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| DispatchError::InvalidHistory("prior handoff is missing".into()))?;
        let EventKind::AgentHandoff(prior_handoff) = prior_handoff_event.kind else {
            return Err(DispatchError::InvalidHistory(
                "terminal cause is not a handoff".into(),
            ));
        };
        let turn_generation = agent
            .turn_generation
            .checked_add(1)
            .ok_or(DispatchError::FollowupGenerationOverflow)?;
        let followup_turn_id = TurnId::new();
        let timeout_millis = u64::try_from(request.budget.timeout.as_millis())
            .map_err(|_| DispatchError::InvalidFollowupBudget)?;
        let followup_event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: Some(prior_terminal_event.event_id.clone()),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::Agent(request.parent_agent_id.clone()),
                agent_id: Some(agent.handle.agent_id.clone()),
                turn_id: Some(followup_turn_id.clone()),
                workspace_id: Some(agent.handle.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::AgentFollowup(AgentFollowupEvent {
                    version: AGENT_FOLLOWUP_EVENT_VERSION,
                    agent_id: agent.handle.agent_id.clone(),
                    turn_generation,
                    followup_turn_id: followup_turn_id.clone(),
                    parent_agent_id: request.parent_agent_id,
                    parent_turn_id: request.causing_turn_id.clone(),
                    parent_event_id: parent_event.event_id,
                    prior_terminal_event_id: prior_terminal_event.event_id.clone(),
                    prior_handoff_event_id: prior_handoff_event_id.clone(),
                    objective: request.objective.clone(),
                    effective_capabilities: capabilities.iter().map(capability_snapshot).collect(),
                    budget: AgentFollowupBudgetSnapshot {
                        timeout_millis,
                        max_provider_events: request.budget.max_provider_events,
                        max_tool_calls: request.budget.max_tool_calls,
                    },
                    retained_session_requested: request.retained_session_requested,
                }),
            })
            .map_err(DispatchError::RunLog)?;
        let agent = self
            .agents
            .get_mut(&request.agent_id)
            .expect("follow-up agent checked above");
        agent.role.capabilities = capabilities;
        agent.objective = request.objective.clone();
        agent.causing_turn_id = Some(followup_turn_id.clone());
        agent.turn_generation = turn_generation;
        agent.followup = Some(AgentFollowupTurn {
            turn_generation,
            followup_turn_id: followup_turn_id.clone(),
            parent_turn_id: request.causing_turn_id,
            parent_event_id: request.caused_by_event,
            prior_terminal_event_id: prior_terminal_event.event_id,
            prior_handoff_event_id,
            objective: request.objective,
            prior_handoff_summary: prior_handoff.handoff.as_handoff().summary.clone(),
            budget: request.budget,
            retained_session_requested: request.retained_session_requested,
        });
        agent.followup_authorized = true;
        agent.last_event_id = followup_event.event_id.clone();
        let handle = agent.handle.clone();
        self.transition_internal(
            &handle,
            DispatchState::Queued,
            Some(format!(
                "follow-up turn generation {turn_generation} queued"
            )),
            false,
            true,
        )?;
        Ok(FollowupAgentHandle {
            handle,
            followup_turn_id,
            turn_generation,
            followup_event_id: followup_event.event_id,
        })
    }

    pub fn transition(
        &mut self,
        handle: &DispatchHandle,
        next: DispatchState,
        detail: Option<String>,
    ) -> Result<EventEnvelope, DispatchError> {
        self.transition_internal(handle, next, detail, false, false)
    }

    /// Record a completed result only after the provider payload has become a
    /// [`ValidatedHandoff`]. The handoff event is appended first and is the
    /// causal parent of the Completed lifecycle event.
    pub fn complete_with_handoff(
        &mut self,
        handle: &DispatchHandle,
        handoff: ValidatedHandoff,
    ) -> Result<DispatchTerminalRecord, DispatchError> {
        if handoff.status() != HandoffStatus::Completed {
            return Err(DispatchError::HandoffStatusMismatch {
                expected: HandoffStatus::Completed,
                actual: handoff.status(),
            });
        }
        self.finish_with_handoff(handle, handoff)
    }

    /// Failed, interrupted, and timed-out children retain the same structured
    /// partial evidence contract. Timed-out handoffs project to Interrupted in
    /// the current lifecycle vocabulary.
    pub fn finish_with_handoff(
        &mut self,
        handle: &DispatchHandle,
        handoff: ValidatedHandoff,
    ) -> Result<DispatchTerminalRecord, DispatchError> {
        self.finish_with_handoff_and_warnings(handle, handoff, Vec::new())
    }

    pub fn finish_with_handoff_and_warnings(
        &mut self,
        handle: &DispatchHandle,
        handoff: ValidatedHandoff,
        workspace_warnings: Vec<super::AgentWorkspaceWarning>,
    ) -> Result<DispatchTerminalRecord, DispatchError> {
        let agent = self
            .agents
            .get(&handle.agent_id)
            .ok_or_else(|| DispatchError::UnknownAgent(handle.agent_id.clone()))?;
        if agent.handle.run_id != handle.run_id || agent.handle.workspace != handle.workspace {
            return Err(DispatchError::HandleMismatch(handle.agent_id.clone()));
        }
        let handoff_status = handoff.status();
        let handoff_version = handoff.as_handoff().version;
        let terminal_state = terminal_state_for_handoff(handoff_status);
        if !valid_transition(&agent.state, &terminal_state) {
            return Err(DispatchError::InvalidTransition {
                agent_id: handle.agent_id.clone(),
                from: agent.state.clone(),
                to: terminal_state,
            });
        }
        if agent.pending_handoff.is_some() {
            return Err(DispatchError::PendingHandoff(handle.agent_id.clone()));
        }
        if agent
            .runtime_operations
            .values()
            .any(|state| *state != RuntimeOperationState::Terminal)
        {
            return Err(DispatchError::OutstandingRuntimeOperations(
                handle.agent_id.clone(),
            ));
        }
        let handoff_event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: Some(agent.last_event_id.clone()),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("agent_scheduler".into()),
                agent_id: Some(handle.agent_id.clone()),
                turn_id: agent.causing_turn_id.clone(),
                workspace_id: Some(handle.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::AgentHandoff(AgentHandoffEvent {
                    handoff,
                    workspace_warnings,
                }),
            })
            .map_err(DispatchError::RunLog)?;
        let agent = self
            .agents
            .get_mut(&handle.agent_id)
            .expect("checked above");
        agent.last_event_id = handoff_event.event_id.clone();
        agent.pending_handoff = Some(PendingHandoff {
            status: handoff_status,
            handoff_event: handoff_event.clone(),
        });
        let terminal_event = self
            .transition_internal(
                handle,
                terminal_state,
                Some(format!(
                    "terminal state recorded from structured handoff v{}",
                    handoff_version
                )),
                true,
                false,
            )
            .map_err(|error| DispatchError::TerminalAfterHandoffFailed {
                handoff_event_id: handoff_event.event_id.clone(),
                source: Box::new(error),
            })?;
        Ok(DispatchTerminalRecord {
            handoff_event,
            terminal_event,
        })
    }

    fn transition_internal(
        &mut self,
        handle: &DispatchHandle,
        next: DispatchState,
        detail: Option<String>,
        from_validated_handoff: bool,
        from_followup: bool,
    ) -> Result<EventEnvelope, DispatchError> {
        let agent = self
            .agents
            .get(&handle.agent_id)
            .ok_or_else(|| DispatchError::UnknownAgent(handle.agent_id.clone()))?;
        if agent.handle.run_id != handle.run_id || agent.handle.workspace != handle.workspace {
            return Err(DispatchError::HandleMismatch(handle.agent_id.clone()));
        }
        let authorized_followup = from_followup
            && agent.followup_authorized
            && agent.state.is_terminal()
            && next == DispatchState::Queued;
        if !valid_transition(&agent.state, &next) && !authorized_followup {
            return Err(DispatchError::InvalidTransition {
                agent_id: handle.agent_id.clone(),
                from: agent.state.clone(),
                to: next,
            });
        }
        if next == DispatchState::Completed && !from_validated_handoff {
            return Err(DispatchError::CompletionRequiresValidatedHandoff(
                handle.agent_id.clone(),
            ));
        }
        if from_validated_handoff {
            let status = agent
                .pending_handoff
                .as_ref()
                .map(|pending| pending.status)
                .ok_or_else(|| DispatchError::MissingPendingHandoff(handle.agent_id.clone()))?;
            if terminal_state_for_handoff(status) != next {
                return Err(DispatchError::PendingHandoffTransitionMismatch {
                    status,
                    lifecycle: next,
                });
            }
        } else if agent.pending_handoff.is_some() {
            return Err(DispatchError::PendingHandoff(handle.agent_id.clone()));
        }
        if from_followup && !authorized_followup {
            return Err(DispatchError::FollowupNotAuthorized(
                handle.agent_id.clone(),
            ));
        }
        let event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: Some(agent.last_event_id.clone()),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("agent_scheduler".into()),
                agent_id: Some(handle.agent_id.clone()),
                turn_id: agent.causing_turn_id.clone(),
                workspace_id: Some(handle.workspace.workspace_id.clone()),
                branch_id: None,
                kind: lifecycle_event(
                    &handle.agent_id,
                    agent.parent_agent_id.clone(),
                    &agent.role,
                    Some(agent.objective.clone()),
                    None,
                    next.clone(),
                    detail,
                ),
            })
            .map_err(DispatchError::RunLog)?;
        let agent = self
            .agents
            .get_mut(&handle.agent_id)
            .expect("checked above");
        agent.state = next.clone();
        agent.last_event_id = event.event_id.clone();
        if next.is_terminal() {
            agent.pending_handoff = None;
            if self.shared_writer.get(&handle.workspace.workspace_id) == Some(&handle.agent_id) {
                self.shared_writer.remove(&handle.workspace.workspace_id);
            }
        }
        if authorized_followup {
            agent.followup_authorized = false;
            agent.queue_sequence = event.sequence;
        }
        Ok(event)
    }

    pub fn state(&self, agent_id: &AgentId) -> Option<&DispatchState> {
        self.agents.get(agent_id).map(|agent| &agent.state)
    }

    pub fn requested_route(&self, agent_id: &AgentId) -> Option<&RequestedModelRoute> {
        self.agents
            .get(agent_id)
            .map(|agent| &agent.requested_route)
    }

    pub fn resolved_route(&self, agent_id: &AgentId) -> Option<&ResolvedModelRoute> {
        self.agents.get(agent_id).map(|agent| &agent.resolved_route)
    }

    /// FIFO-ready queued work for the supervisor.
    pub fn queued_dispatches(&self) -> Vec<AgentDispatchRecord> {
        let mut queued = self
            .agents
            .values()
            .filter(|agent| agent.state == DispatchState::Queued)
            .map(dispatch_record)
            .collect::<Vec<_>>();
        queued.sort_by_key(|agent| agent.queue_sequence);
        queued
    }

    pub fn dispatch_record(&self, agent_id: &AgentId) -> Option<AgentDispatchRecord> {
        self.agents.get(agent_id).map(dispatch_record)
    }

    pub fn dispatch_records(&self) -> Vec<AgentDispatchRecord> {
        let mut records = self
            .agents
            .values()
            .map(dispatch_record)
            .collect::<Vec<_>>();
        records.sort_by_key(|agent| agent.queue_sequence);
        records
    }

    /// Append a provider/tool event to this child’s causal trace and advance
    /// the scheduler-owned tip used by handoff completion and recovery.
    pub fn record_runtime_event(
        &mut self,
        handle: &DispatchHandle,
        kind: EventKind,
        operation_id: Option<OperationId>,
        provider_call_id: Option<String>,
    ) -> Result<EventEnvelope, DispatchError> {
        let agent = self
            .agents
            .get(&handle.agent_id)
            .ok_or_else(|| DispatchError::UnknownAgent(handle.agent_id.clone()))?;
        if agent.handle != *handle {
            return Err(DispatchError::HandleMismatch(handle.agent_id.clone()));
        }
        if agent.state.is_terminal() || !is_runtime_trace_event(&kind) {
            return Err(DispatchError::InvalidRuntimeEvent(handle.agent_id.clone()));
        }
        validate_runtime_operation(agent, &kind, operation_id.as_ref())?;
        let event = self
            .sink
            .append(NewRunEvent {
                run_id: self.run_id.clone(),
                caused_by: Some(agent.last_event_id.clone()),
                operation_id: operation_id.clone(),
                provider_call_id,
                actor: EventActor::Agent(handle.agent_id.clone()),
                agent_id: Some(handle.agent_id.clone()),
                turn_id: agent.causing_turn_id.clone(),
                workspace_id: Some(handle.workspace.workspace_id.clone()),
                branch_id: None,
                kind: kind.clone(),
            })
            .map_err(DispatchError::RunLog)?;
        let agent = self
            .agents
            .get_mut(&handle.agent_id)
            .expect("checked above");
        apply_runtime_operation(agent, &kind, operation_id.as_ref())?;
        agent.last_event_id = event.event_id.clone();
        Ok(event)
    }

    fn apply_rehydrated_runtime_event(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<(), DispatchError> {
        let agent_id = event.agent_id.as_ref().expect("caller checked identity");
        let agent = self.agents.get(agent_id).expect("caller checked agent");
        if event.workspace_id.as_ref() != Some(&agent.handle.workspace.workspace_id)
            || event.turn_id != agent.causing_turn_id
            || event.caused_by.as_ref() != Some(&agent.last_event_id)
            || agent.state.is_terminal()
        {
            return Err(DispatchError::InvalidHistory(format!(
                "agent {agent_id} has a non-contiguous runtime event"
            )));
        }
        validate_runtime_operation(agent, &event.kind, event.operation_id.as_ref())?;
        let agent = self.agents.get_mut(agent_id).expect("checked above");
        apply_runtime_operation(agent, &event.kind, event.operation_id.as_ref())?;
        agent.last_event_id = event.event_id.clone();
        Ok(())
    }

    fn apply_rehydrated_followup(
        &mut self,
        envelope: &EventEnvelope,
        followup: &AgentFollowupEvent,
    ) -> Result<(), DispatchError> {
        if followup.version != AGENT_FOLLOWUP_EVENT_VERSION {
            return Err(DispatchError::InvalidHistory(format!(
                "unsupported follow-up event version {}",
                followup.version
            )));
        }
        let agent = self.agents.get(&followup.agent_id).ok_or_else(|| {
            DispatchError::InvalidHistory(format!(
                "follow-up {} belongs to unknown agent {}",
                envelope.event_id, followup.agent_id
            ))
        })?;
        // Standalone schedulers may use an external parent that is supplied
        // by the supervisor rather than persisted in the original child spec.
        // In that case the durable follow-up event and its validated parent
        // event are the recovery authority.
        let expected_parent = agent
            .parent_agent_id
            .as_ref()
            .unwrap_or(&followup.parent_agent_id);
        if envelope.agent_id.as_ref() != Some(&followup.agent_id)
            || envelope.turn_id.as_ref() != Some(&followup.followup_turn_id)
            || envelope.workspace_id.as_ref() != Some(&agent.handle.workspace.workspace_id)
            || envelope.caused_by.as_ref() != Some(&agent.last_event_id)
            || envelope.actor != EventActor::Agent(followup.parent_agent_id.clone())
            || expected_parent != &followup.parent_agent_id
            || followup.prior_terminal_event_id != agent.last_event_id
            || !matches!(
                agent.state,
                DispatchState::Completed | DispatchState::Interrupted
            )
            || agent.followup_authorized
        {
            return Err(DispatchError::InvalidHistory(format!(
                "follow-up {} is not authorized from the recorded terminal boundary",
                envelope.event_id
            )));
        }
        if followup.turn_generation != agent.turn_generation.saturating_add(1)
            || followup.objective.trim().is_empty()
            || followup.objective.len() > 8 * 1024
            || followup.budget.timeout_millis == 0
            || followup.budget.max_provider_events == 0
            || followup.budget.max_tool_calls == 0
        {
            return Err(DispatchError::InvalidHistory(format!(
                "follow-up {} has an invalid generation, objective, or budget",
                envelope.event_id
            )));
        }
        let parent_event = self
            .sink
            .event(&self.run_id, &followup.parent_event_id)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| {
                DispatchError::InvalidHistory("follow-up parent event is missing".into())
            })?;
        if parent_event.agent_id.as_ref() != Some(&followup.parent_agent_id)
            || parent_event.turn_id.as_ref() != Some(&followup.parent_turn_id)
        {
            return Err(DispatchError::InvalidHistory(
                "follow-up parent cause has the wrong agent or turn".into(),
            ));
        }
        let prior_terminal = self
            .sink
            .event(&self.run_id, &followup.prior_terminal_event_id)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| {
                DispatchError::InvalidHistory("follow-up terminal event is missing".into())
            })?;
        if prior_terminal.caused_by.as_ref() != Some(&followup.prior_handoff_event_id) {
            return Err(DispatchError::InvalidHistory(
                "follow-up terminal does not reference its recorded handoff".into(),
            ));
        }
        let prior_handoff = self
            .sink
            .event(&self.run_id, &followup.prior_handoff_event_id)
            .map_err(DispatchError::RunLog)?
            .ok_or_else(|| {
                DispatchError::InvalidHistory("follow-up handoff event is missing".into())
            })?;
        let EventKind::AgentHandoff(prior_handoff) = prior_handoff.kind else {
            return Err(DispatchError::InvalidHistory(
                "follow-up handoff ID is not an agent handoff".into(),
            ));
        };
        let capabilities = followup
            .effective_capabilities
            .iter()
            .map(restore_capability)
            .collect::<BTreeSet<_>>();
        if capabilities.len() != followup.effective_capabilities.len()
            || capabilities.is_empty()
            || !capabilities.is_subset(&agent.role.capabilities)
        {
            return Err(DispatchError::InvalidHistory(
                "follow-up capabilities widen or repeat the prior ceiling".into(),
            ));
        }
        let timeout = std::time::Duration::from_millis(followup.budget.timeout_millis);
        let agent = self
            .agents
            .get_mut(&followup.agent_id)
            .expect("follow-up agent checked above");
        agent.role.capabilities = capabilities;
        agent.objective = followup.objective.clone();
        agent.causing_turn_id = Some(followup.followup_turn_id.clone());
        agent.turn_generation = followup.turn_generation;
        agent.followup = Some(AgentFollowupTurn {
            turn_generation: followup.turn_generation,
            followup_turn_id: followup.followup_turn_id.clone(),
            parent_turn_id: followup.parent_turn_id.clone(),
            parent_event_id: followup.parent_event_id.clone(),
            prior_terminal_event_id: followup.prior_terminal_event_id.clone(),
            prior_handoff_event_id: followup.prior_handoff_event_id.clone(),
            objective: followup.objective.clone(),
            prior_handoff_summary: prior_handoff.handoff.as_handoff().summary.clone(),
            budget: AgentLoopBudget {
                timeout,
                max_provider_events: followup.budget.max_provider_events,
                max_tool_calls: followup.budget.max_tool_calls,
            },
            retained_session_requested: followup.retained_session_requested,
        });
        agent.followup_authorized = true;
        agent.last_event_id = envelope.event_id.clone();
        Ok(())
    }

    /// Resume the terminal lifecycle transition for a dispatch whose validated
    /// handoff is already durable but whose lifecycle append failed — the
    /// in-process counterpart of the rehydrate repair. Returns `Ok(None)` when
    /// no handoff is pending. (OV-00318)
    pub(crate) fn complete_pending_handoff(
        &mut self,
        handle: &DispatchHandle,
    ) -> Result<Option<DispatchTerminalRecord>, DispatchError> {
        let Some(pending) = self
            .agents
            .get(&handle.agent_id)
            .and_then(|agent| agent.pending_handoff.clone())
        else {
            return Ok(None);
        };
        let terminal_event = self.transition_internal(
            handle,
            terminal_state_for_handoff(pending.status),
            Some("terminal state resumed from durable validated handoff".into()),
            true,
            false,
        )?;
        Ok(Some(DispatchTerminalRecord {
            handoff_event: pending.handoff_event,
            terminal_event,
        }))
    }

    pub(crate) fn terminate_conservatively(
        &mut self,
        handle: &DispatchHandle,
        status: HandoffStatus,
        detail: &str,
    ) -> Result<DispatchTerminalRecord, DispatchError> {
        // A durable handoff may already exist from an earlier attempt that
        // failed between its two appends; finishing that transition is the
        // only valid way forward (a second handoff would be refused).
        if let Some(terminal) = self.complete_pending_handoff(handle)? {
            return Ok(terminal);
        }
        let open = self
            .agents
            .get(&handle.agent_id)
            .ok_or_else(|| DispatchError::UnknownAgent(handle.agent_id.clone()))?
            .runtime_operations
            .iter()
            .filter(|(_, state)| **state != RuntimeOperationState::Terminal)
            .map(|(operation_id, state)| (operation_id.clone(), *state))
            .collect::<Vec<_>>();
        for (operation_id, state) in open {
            self.record_runtime_event(
                handle,
                EventKind::ToolResult(crate::run_log::ToolResultEvent {
                    outcome: if state == RuntimeOperationState::Started {
                        ToolOutcome::UnknownAfterCrash
                    } else {
                        ToolOutcome::Failed
                    },
                    summary: Some(
                        "tool operation interrupted while recovering after process restart".into(),
                    ),
                    result: None,
                }),
                Some(operation_id),
                None,
            )?;
        }
        self.finish_with_handoff(handle, validated_terminal_handoff(status, detail)?)
    }

    fn validate_request(&self, request: &DispatchRequest) -> Result<(), DispatchError> {
        if !valid_task_name(&request.task_name) {
            return Err(DispatchError::InvalidTaskName(request.task_name.clone()));
        }
        if self
            .agents
            .values()
            .any(|agent| agent.task_name == request.task_name)
        {
            return Err(DispatchError::DuplicateTaskName(request.task_name.clone()));
        }
        if request.objective.trim().is_empty() {
            return Err(DispatchError::EmptyObjective);
        }
        match (&request.parent_agent_id, &request.causing_turn_id) {
            (Some(parent), Some(turn))
                if !self.agents.contains_key(parent)
                    && self.external_parent.as_ref() != Some(parent) =>
            {
                let _ = turn;
                return Err(DispatchError::UnknownParent(parent.clone()));
            }
            (Some(parent), Some(turn)) => {
                let cause_id = request
                    .caused_by_event
                    .as_ref()
                    .ok_or(DispatchError::ChildMissingCausingEvent)?;
                let cause = self
                    .sink
                    .event(&self.run_id, cause_id)
                    .map_err(DispatchError::RunLog)?
                    .ok_or_else(|| DispatchError::CausingEventNotFound(cause_id.clone()))?;
                if cause.run_id != self.run_id {
                    return Err(DispatchError::CausingEventWrongRun(cause_id.clone()));
                }
                if cause.agent_id.as_ref() != Some(parent) {
                    return Err(DispatchError::CausingEventAgentMismatch {
                        event_id: cause_id.clone(),
                        expected: parent.clone(),
                        actual: cause.agent_id,
                    });
                }
                if cause.turn_id.as_ref() != Some(turn) {
                    return Err(DispatchError::CausingEventTurnMismatch {
                        event_id: cause_id.clone(),
                        expected: turn.clone(),
                        actual: cause.turn_id,
                    });
                }
            }
            (Some(_), None) => return Err(DispatchError::ChildMissingCausingTurn),
            (None, Some(_)) => return Err(DispatchError::TurnWithoutParent),
            _ => {}
        }
        if request.role.can_write()
            && matches!(
                request.workspace.strategy,
                WorkspaceStrategy::ReadOnlySnapshot { .. }
            )
        {
            return Err(DispatchError::WriteCapabilityInReadOnlyWorkspace);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct AgentDispatchRecord {
    pub handle: DispatchHandle,
    pub role: AgentRoleTemplate,
    pub requested_route: RequestedModelRoute,
    pub resolved_route: ResolvedModelRoute,
    pub task_name: String,
    pub objective: String,
    pub parent_agent_id: Option<AgentId>,
    pub causing_turn_id: Option<TurnId>,
    pub turn_generation: u32,
    pub followup: Option<AgentFollowupTurn>,
    pub state: DispatchState,
    pub queue_sequence: u64,
}

fn dispatch_record(agent: &ScheduledAgent) -> AgentDispatchRecord {
    AgentDispatchRecord {
        handle: agent.handle.clone(),
        role: agent.role.clone(),
        requested_route: agent.requested_route.clone(),
        resolved_route: agent.resolved_route.clone(),
        task_name: agent.task_name.clone(),
        objective: agent.objective.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        causing_turn_id: agent.causing_turn_id.clone(),
        turn_generation: agent.turn_generation,
        followup: agent.followup.clone(),
        state: agent.state.clone(),
        queue_sequence: agent.queue_sequence,
    }
}

fn is_runtime_trace_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentProvider(_)
            | EventKind::AgentUsage(_)
            | EventKind::AgentProgress(_)
            | EventKind::ToolIntent(_)
            | EventKind::ToolStarted(_)
            | EventKind::ToolResult(_)
    )
}

fn validate_runtime_operation(
    agent: &ScheduledAgent,
    kind: &EventKind,
    operation_id: Option<&OperationId>,
) -> Result<(), DispatchError> {
    match kind {
        EventKind::AgentProvider(_) | EventKind::AgentUsage(_) | EventKind::AgentProgress(_)
            if operation_id.is_none() =>
        {
            Ok(())
        }
        EventKind::ToolIntent(_) => {
            let operation_id = operation_id
                .ok_or_else(|| DispatchError::InvalidRuntimeEvent(agent.handle.agent_id.clone()))?;
            if agent.runtime_operations.contains_key(operation_id) {
                return Err(DispatchError::InvalidRuntimeOperation(operation_id.clone()));
            }
            Ok(())
        }
        EventKind::ToolStarted(_) => {
            let operation_id = operation_id
                .ok_or_else(|| DispatchError::InvalidRuntimeEvent(agent.handle.agent_id.clone()))?;
            if agent.runtime_operations.get(operation_id) != Some(&RuntimeOperationState::Intended)
            {
                return Err(DispatchError::InvalidRuntimeOperation(operation_id.clone()));
            }
            Ok(())
        }
        EventKind::ToolResult(result) => {
            let operation_id = operation_id
                .ok_or_else(|| DispatchError::InvalidRuntimeEvent(agent.handle.agent_id.clone()))?;
            let current = agent.runtime_operations.get(operation_id);
            let valid = match result.outcome {
                ToolOutcome::Completed => current == Some(&RuntimeOperationState::Started),
                ToolOutcome::Failed | ToolOutcome::Interrupted => matches!(
                    current,
                    Some(RuntimeOperationState::Intended | RuntimeOperationState::Started)
                ),
                ToolOutcome::UnknownAfterCrash => current == Some(&RuntimeOperationState::Started),
            };
            if !valid {
                return Err(DispatchError::InvalidRuntimeOperation(operation_id.clone()));
            }
            Ok(())
        }
        _ => Err(DispatchError::InvalidRuntimeEvent(
            agent.handle.agent_id.clone(),
        )),
    }
}

fn apply_runtime_operation(
    agent: &mut ScheduledAgent,
    kind: &EventKind,
    operation_id: Option<&OperationId>,
) -> Result<(), DispatchError> {
    match kind {
        EventKind::AgentProvider(_) | EventKind::AgentUsage(_) | EventKind::AgentProgress(_) => {}
        EventKind::ToolIntent(_) => {
            agent.runtime_operations.insert(
                operation_id.expect("validated operation ID").clone(),
                RuntimeOperationState::Intended,
            );
        }
        EventKind::ToolStarted(_) => {
            *agent
                .runtime_operations
                .get_mut(operation_id.expect("validated operation ID"))
                .expect("validated operation") = RuntimeOperationState::Started;
        }
        EventKind::ToolResult(_) => {
            *agent
                .runtime_operations
                .get_mut(operation_id.expect("validated operation ID"))
                .expect("validated operation") = RuntimeOperationState::Terminal;
        }
        _ => {
            return Err(DispatchError::InvalidRuntimeEvent(
                agent.handle.agent_id.clone(),
            ))
        }
    }
    Ok(())
}

fn valid_transition(from: &DispatchState, to: &DispatchState) -> bool {
    use DispatchState::*;
    matches!(
        (from, to),
        (Created, Queued | Interrupted | Failed)
            | (Queued, Starting | Interrupted | Failed)
            | (Starting, Running | Interrupted | Failed)
            | (
                Running,
                WaitingForAgent
                    | WaitingForTool
                    | WaitingForUser
                    | Completed
                    | Interrupted
                    | Failed
            )
            | (
                WaitingForAgent | WaitingForTool | WaitingForUser,
                Running | Completed | Interrupted | Failed
            )
    )
}

fn terminal_state_for_handoff(status: HandoffStatus) -> DispatchState {
    match status {
        HandoffStatus::Completed => DispatchState::Completed,
        HandoffStatus::Failed => DispatchState::Failed,
        HandoffStatus::Interrupted | HandoffStatus::TimedOut => DispatchState::Interrupted,
    }
}

pub(crate) fn validated_terminal_handoff(
    status: HandoffStatus,
    detail: impl Into<String>,
) -> Result<ValidatedHandoff, DispatchError> {
    let detail = detail.into();
    HandoffValidator::default()
        .validate(
            StructuredHandoffV1 {
                version: 1,
                status,
                summary: detail.clone(),
                evidence: Vec::new(),
                changed_files: Vec::new(),
                verification: Vec::new(),
                blockers: vec![detail],
                followups: Vec::new(),
                confidence: HandoffConfidence::Low,
            },
            Some(status),
        )
        .map_err(|error| DispatchError::InvalidHistory(format!("terminal handoff: {error}")))
}

fn lifecycle_event(
    agent_id: &AgentId,
    parent_agent_id: Option<AgentId>,
    role: &AgentRoleTemplate,
    objective: Option<String>,
    dispatch_spec: Option<AgentDispatchSpecSnapshot>,
    state: DispatchState,
    detail: Option<String>,
) -> EventKind {
    EventKind::AgentLifecycle(AgentLifecycleEvent {
        agent_id: agent_id.clone(),
        parent_agent_id,
        state: lifecycle_state(state),
        kind: role.name.to_string(),
        objective,
        detail,
        dispatch_spec: dispatch_spec.map(Box::new),
    })
}

fn dispatch_spec_snapshot(
    role: &AgentRoleTemplate,
    task_name: &str,
    requested_route: &RequestedModelRoute,
    resolved_route: &ResolvedModelRoute,
    assigned_workspace: &WorkspaceStrategy,
) -> AgentDispatchSpecSnapshot {
    AgentDispatchSpecSnapshot {
        version: 2,
        task_name: Some(task_name.into()),
        legacy_model: None,
        requested_route: Some(requested_route_snapshot(requested_route)),
        resolved_route: Some(resolved_route_snapshot(resolved_route)),
        instructions: role.instructions.clone(),
        capabilities: role.capabilities.iter().map(capability_snapshot).collect(),
        kind_workspace_policy: match role.workspace_policy {
            WorkspacePolicy::SharedWorkspace => AgentWorkspacePolicySnapshot::SharedWorkspace,
            WorkspacePolicy::IsolatedWorktree => AgentWorkspacePolicySnapshot::IsolatedWorktree,
            WorkspacePolicy::ReadOnlyProjection => AgentWorkspacePolicySnapshot::ReadOnlyProjection,
        },
        assigned_workspace: match assigned_workspace {
            WorkspaceStrategy::SharedWorkspace => AgentWorkspaceStrategySnapshot::SharedWorkspace,
            WorkspaceStrategy::IsolatedWorktree { base_manifest_id } => {
                AgentWorkspaceStrategySnapshot::IsolatedWorktree {
                    base_manifest_id: base_manifest_id.clone(),
                }
            }
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id } => {
                AgentWorkspaceStrategySnapshot::ReadOnlySnapshot {
                    manifest_id: manifest_id.clone(),
                }
            }
        },
        completion_contract: match &role.completion_contract {
            CompletionContract::StructuredHandoff => {
                AgentCompletionContractSnapshot::StructuredHandoff
            }
            CompletionContract::ReviewReport => AgentCompletionContractSnapshot::ReviewReport,
            CompletionContract::SafetyVerdict => AgentCompletionContractSnapshot::SafetyVerdict,
            CompletionContract::Plan => AgentCompletionContractSnapshot::Plan,
            CompletionContract::Custom(value) => {
                AgentCompletionContractSnapshot::Custom(value.clone())
            }
        },
    }
}

fn capability_snapshot(capability: &AgentCapability) -> AgentCapabilitySnapshot {
    match capability {
        AgentCapability::Read => AgentCapabilitySnapshot::Read,
        AgentCapability::Navigate => AgentCapabilitySnapshot::Navigate,
        AgentCapability::SafeShell => AgentCapabilitySnapshot::SafeShell,
        AgentCapability::Shell => AgentCapabilitySnapshot::Shell,
        AgentCapability::WorkspaceWrite => AgentCapabilitySnapshot::WorkspaceWrite,
        AgentCapability::ExternalEffects => AgentCapabilitySnapshot::ExternalEffects,
        AgentCapability::DispatchAgents => AgentCapabilitySnapshot::DispatchAgents,
    }
}

fn restore_capability(capability: &AgentCapabilitySnapshot) -> AgentCapability {
    match capability {
        AgentCapabilitySnapshot::Read => AgentCapability::Read,
        AgentCapabilitySnapshot::Navigate => AgentCapability::Navigate,
        AgentCapabilitySnapshot::SafeShell => AgentCapability::SafeShell,
        AgentCapabilitySnapshot::Shell => AgentCapability::Shell,
        AgentCapabilitySnapshot::WorkspaceWrite => AgentCapability::WorkspaceWrite,
        AgentCapabilitySnapshot::ExternalEffects => AgentCapability::ExternalEffects,
        AgentCapabilitySnapshot::DispatchAgents => AgentCapability::DispatchAgents,
    }
}

fn same_effective_route(left: &ResolvedModelRoute, right: &ResolvedModelRoute) -> bool {
    left.catalog_model_id == right.catalog_model_id
        && left.profile_name == right.profile_name
        && left.provider == right.provider
        && left.model == right.model
        && left.reasoning_effort == right.reasoning_effort
}

fn valid_task_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn requested_route_snapshot(route: &RequestedModelRoute) -> AgentRequestedModelRouteSnapshot {
    AgentRequestedModelRouteSnapshot {
        catalog_model_id: route.catalog_model_id.clone(),
        reasoning_effort: route.reasoning_effort.as_str().into(),
        fallback_policy: match &route.fallback_policy {
            ModelFallbackPolicy::FailClosed => AgentModelFallbackPolicySnapshot::FailClosed,
            ModelFallbackPolicy::Explicit {
                catalog_model_id,
                reasoning_effort,
            } => AgentModelFallbackPolicySnapshot::Explicit {
                catalog_model_id: catalog_model_id.clone(),
                reasoning_effort: reasoning_effort.as_str().into(),
            },
        },
    }
}

fn resolved_route_snapshot(route: &ResolvedModelRoute) -> AgentResolvedModelRouteSnapshot {
    AgentResolvedModelRouteSnapshot {
        catalog_generation: route.catalog_generation.clone(),
        catalog_model_id: route.catalog_model_id.clone(),
        profile_name: route.profile_name.clone(),
        provider: route.provider.clone(),
        model: route.model.clone(),
        reasoning_effort: route.reasoning_effort.as_str().into(),
        resolution: match route.resolution {
            ModelRouteResolution::Exact => AgentModelRouteResolutionSnapshot::Exact,
            ModelRouteResolution::ConfiguredFallback => {
                AgentModelRouteResolutionSnapshot::ConfiguredFallback
            }
            ModelRouteResolution::HistoricV1 => AgentModelRouteResolutionSnapshot::HistoricV1,
        },
        fallback_reason: route.fallback_reason.clone(),
    }
}

fn restore_dispatch_spec(
    kind_name: &str,
    snapshot: &AgentDispatchSpecSnapshot,
) -> Result<
    (
        AgentRoleTemplate,
        WorkspaceStrategy,
        RequestedModelRoute,
        ResolvedModelRoute,
    ),
    DispatchError,
> {
    let capabilities = snapshot
        .capabilities
        .iter()
        .map(restore_capability)
        .collect::<BTreeSet<_>>();
    if capabilities.len() != snapshot.capabilities.len() {
        return Err(DispatchError::InvalidHistory(
            "dispatch spec repeats a capability".into(),
        ));
    }
    let name = match kind_name {
        "implementer" => AgentKindName::Implementer,
        "explorer" => AgentKindName::Explorer,
        "verifier" => AgentKindName::Verifier,
        "reviewer" => AgentKindName::Reviewer,
        "safety" => AgentKindName::Safety,
        "planner" => AgentKindName::Planner,
        custom if custom.starts_with("custom:") && custom.len() > "custom:".len() => {
            AgentKindName::Custom(custom["custom:".len()..].into())
        }
        other => {
            return Err(DispatchError::InvalidHistory(format!(
                "unknown persisted agent kind {other:?}"
            )))
        }
    };
    let workspace_policy = match snapshot.kind_workspace_policy {
        AgentWorkspacePolicySnapshot::SharedWorkspace => WorkspacePolicy::SharedWorkspace,
        AgentWorkspacePolicySnapshot::IsolatedWorktree => WorkspacePolicy::IsolatedWorktree,
        AgentWorkspacePolicySnapshot::ReadOnlyProjection => WorkspacePolicy::ReadOnlyProjection,
    };
    let strategy = match &snapshot.assigned_workspace {
        AgentWorkspaceStrategySnapshot::SharedWorkspace => WorkspaceStrategy::SharedWorkspace,
        AgentWorkspaceStrategySnapshot::IsolatedWorktree { base_manifest_id } => {
            WorkspaceStrategy::IsolatedWorktree {
                base_manifest_id: base_manifest_id.clone(),
            }
        }
        AgentWorkspaceStrategySnapshot::ReadOnlySnapshot { manifest_id } => {
            WorkspaceStrategy::ReadOnlySnapshot {
                manifest_id: manifest_id.clone(),
            }
        }
    };
    let role = AgentRoleTemplate {
        name,
        instructions: snapshot.instructions.clone(),
        capabilities,
        workspace_policy,
        completion_contract: match &snapshot.completion_contract {
            AgentCompletionContractSnapshot::StructuredHandoff => {
                CompletionContract::StructuredHandoff
            }
            AgentCompletionContractSnapshot::ReviewReport => CompletionContract::ReviewReport,
            AgentCompletionContractSnapshot::SafetyVerdict => CompletionContract::SafetyVerdict,
            AgentCompletionContractSnapshot::Plan => CompletionContract::Plan,
            AgentCompletionContractSnapshot::Custom(value) => {
                CompletionContract::Custom(value.clone())
            }
        },
    };
    let (requested_route, resolved_route) = match snapshot.version {
        1 => restore_v1_model_route(snapshot)?,
        2 => restore_v2_model_route(snapshot)?,
        version => {
            return Err(DispatchError::InvalidHistory(format!(
                "unsupported dispatch spec version {version}"
            )))
        }
    };
    Ok((role, strategy, requested_route, resolved_route))
}

fn restore_v1_model_route(
    snapshot: &AgentDispatchSpecSnapshot,
) -> Result<(RequestedModelRoute, ResolvedModelRoute), DispatchError> {
    if snapshot.requested_route.is_some() || snapshot.resolved_route.is_some() {
        return Err(DispatchError::InvalidHistory(
            "version-one dispatch spec contains version-two routes".into(),
        ));
    }
    let model = snapshot.legacy_model.as_ref().ok_or_else(|| {
        DispatchError::InvalidHistory("version-one dispatch spec has no model".into())
    })?;
    if model.model.trim().is_empty() {
        return Err(DispatchError::InvalidHistory(
            "version-one dispatch spec has an empty model".into(),
        ));
    }
    let effort = match model.effort {
        AgentModelEffortSnapshot::Low => ReasoningEffort::low(),
        AgentModelEffortSnapshot::Medium => ReasoningEffort::medium(),
        AgentModelEffortSnapshot::High => ReasoningEffort::high(),
    };
    let fallback_policy = model
        .fallback_model
        .as_ref()
        .map(|fallback| ModelFallbackPolicy::Explicit {
            catalog_model_id: fallback.clone(),
            reasoning_effort: effort.clone(),
        })
        .unwrap_or(ModelFallbackPolicy::FailClosed);
    Ok((
        RequestedModelRoute {
            // V1 did not persist a catalog/profile identity. Preserve its raw
            // value exactly and mark the resolved route as historic.
            catalog_model_id: model.model.clone(),
            reasoning_effort: effort.clone(),
            fallback_policy,
        },
        ResolvedModelRoute {
            catalog_generation: "historic-v1".into(),
            catalog_model_id: model.model.clone(),
            profile_name: "historic-v1".into(),
            provider: "historic-v1".into(),
            model: model.model.clone(),
            reasoning_effort: effort,
            resolution: ModelRouteResolution::HistoricV1,
            fallback_reason: None,
        },
    ))
}

fn restore_v2_model_route(
    snapshot: &AgentDispatchSpecSnapshot,
) -> Result<(RequestedModelRoute, ResolvedModelRoute), DispatchError> {
    if snapshot.legacy_model.is_some() {
        return Err(DispatchError::InvalidHistory(
            "version-two dispatch spec contains a version-one model".into(),
        ));
    }
    let requested = snapshot.requested_route.as_ref().ok_or_else(|| {
        DispatchError::InvalidHistory("version-two dispatch spec has no requested route".into())
    })?;
    let resolved = snapshot.resolved_route.as_ref().ok_or_else(|| {
        DispatchError::InvalidHistory("version-two dispatch spec has no resolved route".into())
    })?;
    require_nonempty_route_field("requested catalog model ID", &requested.catalog_model_id)?;
    let requested_effort = parse_historic_effort(&requested.reasoning_effort)?;
    let fallback_policy = match &requested.fallback_policy {
        AgentModelFallbackPolicySnapshot::FailClosed => ModelFallbackPolicy::FailClosed,
        AgentModelFallbackPolicySnapshot::Explicit {
            catalog_model_id,
            reasoning_effort,
        } => {
            require_nonempty_route_field("fallback catalog model ID", catalog_model_id)?;
            ModelFallbackPolicy::Explicit {
                catalog_model_id: catalog_model_id.clone(),
                reasoning_effort: parse_historic_effort(reasoning_effort)?,
            }
        }
    };
    for (label, value) in [
        ("catalog generation", resolved.catalog_generation.as_str()),
        (
            "resolved catalog model ID",
            resolved.catalog_model_id.as_str(),
        ),
        ("profile name", resolved.profile_name.as_str()),
        ("provider", resolved.provider.as_str()),
        ("model", resolved.model.as_str()),
    ] {
        require_nonempty_route_field(label, value)?;
    }
    let resolved_effort = parse_historic_effort(&resolved.reasoning_effort)?;
    let resolution = match resolved.resolution {
        AgentModelRouteResolutionSnapshot::Exact => {
            if requested.catalog_model_id != resolved.catalog_model_id
                || requested_effort != resolved_effort
                || resolved.fallback_reason.is_some()
            {
                return Err(DispatchError::InvalidHistory(
                    "exact version-two route differs from its request".into(),
                ));
            }
            ModelRouteResolution::Exact
        }
        AgentModelRouteResolutionSnapshot::ConfiguredFallback => {
            let ModelFallbackPolicy::Explicit {
                catalog_model_id,
                reasoning_effort,
            } = &fallback_policy
            else {
                return Err(DispatchError::InvalidHistory(
                    "resolved fallback has fail-closed request policy".into(),
                ));
            };
            if catalog_model_id != &resolved.catalog_model_id
                || reasoning_effort != &resolved_effort
                || resolved
                    .fallback_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(DispatchError::InvalidHistory(
                    "resolved fallback does not match its explicit request policy".into(),
                ));
            }
            ModelRouteResolution::ConfiguredFallback
        }
        AgentModelRouteResolutionSnapshot::HistoricV1 => {
            return Err(DispatchError::InvalidHistory(
                "version-two dispatch spec uses a historic-v1 resolution".into(),
            ))
        }
    };
    Ok((
        RequestedModelRoute {
            catalog_model_id: requested.catalog_model_id.clone(),
            reasoning_effort: requested_effort,
            fallback_policy,
        },
        ResolvedModelRoute {
            catalog_generation: resolved.catalog_generation.clone(),
            catalog_model_id: resolved.catalog_model_id.clone(),
            profile_name: resolved.profile_name.clone(),
            provider: resolved.provider.clone(),
            model: resolved.model.clone(),
            reasoning_effort: resolved_effort,
            resolution,
            fallback_reason: resolved.fallback_reason.clone(),
        },
    ))
}

fn parse_historic_effort(value: &str) -> Result<ReasoningEffort, DispatchError> {
    ReasoningEffort::new(value).map_err(|error| {
        DispatchError::InvalidHistory(format!("dispatch spec has invalid effort: {error}"))
    })
}

fn require_nonempty_route_field(label: &str, value: &str) -> Result<(), DispatchError> {
    if value.trim().is_empty() {
        return Err(DispatchError::InvalidHistory(format!(
            "dispatch spec has an empty {label}"
        )));
    }
    Ok(())
}

fn lifecycle_state(state: DispatchState) -> AgentLifecycleState {
    match state {
        DispatchState::Created => AgentLifecycleState::Created,
        DispatchState::Queued => AgentLifecycleState::Queued,
        DispatchState::Starting => AgentLifecycleState::Starting,
        DispatchState::Running => AgentLifecycleState::Running,
        DispatchState::WaitingForAgent => AgentLifecycleState::WaitingForAgent,
        DispatchState::WaitingForTool => AgentLifecycleState::WaitingForTool,
        DispatchState::WaitingForUser => AgentLifecycleState::WaitingForUser,
        DispatchState::Completed => AgentLifecycleState::Completed,
        DispatchState::Interrupted => AgentLifecycleState::Interrupted,
        DispatchState::Failed => AgentLifecycleState::Failed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    InvalidHistory(String),
    ModelRoute(ModelRouteError),
    InvalidTaskName(String),
    DuplicateTaskName(String),
    EmptyObjective,
    UnknownParent(AgentId),
    ChildMissingCausingTurn,
    ChildMissingCausingEvent,
    TurnWithoutParent,
    CausingEventNotFound(EventId),
    CausingEventWrongRun(EventId),
    CausingEventAgentMismatch {
        event_id: EventId,
        expected: AgentId,
        actual: Option<AgentId>,
    },
    CausingEventTurnMismatch {
        event_id: EventId,
        expected: TurnId,
        actual: Option<TurnId>,
    },
    WriteCapabilityInReadOnlyWorkspace,
    SharedWorkspaceWriterHeld {
        workspace_id: WorkspaceId,
        holder: AgentId,
    },
    UnknownAgent(AgentId),
    HandleMismatch(AgentId),
    InvalidTransition {
        agent_id: AgentId,
        from: DispatchState,
        to: DispatchState,
    },
    FollowupIneligible {
        agent_id: AgentId,
        state: DispatchState,
    },
    FollowupParentMismatch {
        agent_id: AgentId,
        parent_agent_id: AgentId,
    },
    FollowupParentCauseMismatch(EventId),
    FollowupCapabilityWidening(AgentId),
    FollowupRouteChanged(AgentId),
    InvalidFollowupObjective,
    InvalidFollowupBudget,
    FollowupGenerationOverflow,
    FollowupNotAuthorized(AgentId),
    CompletionRequiresValidatedHandoff(AgentId),
    HandoffStatusMismatch {
        expected: HandoffStatus,
        actual: HandoffStatus,
    },
    PendingHandoff(AgentId),
    MissingPendingHandoff(AgentId),
    OutstandingRuntimeOperations(AgentId),
    InvalidRuntimeEvent(AgentId),
    InvalidRuntimeOperation(OperationId),
    PendingHandoffTransitionMismatch {
        status: HandoffStatus,
        lifecycle: DispatchState,
    },
    TerminalAfterHandoffFailed {
        handoff_event_id: EventId,
        source: Box<DispatchError>,
    },
    RunLog(RunLogError),
    QueueAfterCreationFailed {
        agent_id: AgentId,
        created_event_id: EventId,
        source: RunLogError,
    },
}

impl fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHistory(detail) => write!(formatter, "invalid dispatch history: {detail}"),
            Self::ModelRoute(error) => write!(formatter, "could not resolve agent model: {error}"),
            Self::InvalidTaskName(name) => write!(
                formatter,
                "invalid agent task name {name:?}; expected 1-64 lowercase letters, digits, or '_' starting with a letter"
            ),
            Self::DuplicateTaskName(name) => {
                write!(formatter, "agent task name {name:?} is already allocated")
            }
            Self::EmptyObjective => formatter.write_str("agent objective is empty"),
            Self::UnknownParent(id) => write!(formatter, "parent agent {id} is unknown"),
            Self::ChildMissingCausingTurn => {
                formatter.write_str("child dispatch has no causing turn")
            }
            Self::ChildMissingCausingEvent => {
                formatter.write_str("child dispatch has no causing event")
            }
            Self::TurnWithoutParent => {
                formatter.write_str("dispatch has a causing turn but no parent agent")
            }
            Self::CausingEventNotFound(id) => {
                write!(formatter, "causing event {id} is not present in this run")
            }
            Self::CausingEventWrongRun(id) => {
                write!(formatter, "causing event {id} belongs to another run")
            }
            Self::CausingEventAgentMismatch {
                event_id,
                expected,
                actual,
            } => write!(
                formatter,
                "causing event {event_id} belongs to agent {actual:?}, expected {expected}"
            ),
            Self::CausingEventTurnMismatch {
                event_id,
                expected,
                actual,
            } => write!(
                formatter,
                "causing event {event_id} belongs to turn {actual:?}, expected {expected}"
            ),
            Self::WriteCapabilityInReadOnlyWorkspace => formatter
                .write_str("write-capable agent cannot use a read-only workspace projection"),
            Self::SharedWorkspaceWriterHeld {
                workspace_id,
                holder,
            } => write!(
                formatter,
                "shared workspace {workspace_id} writer lease is held by agent {holder}"
            ),
            Self::UnknownAgent(id) => write!(formatter, "agent {id} is unknown"),
            Self::HandleMismatch(id) => write!(
                formatter,
                "dispatch handle for agent {id} does not match scheduler state"
            ),
            Self::InvalidTransition { agent_id, from, to } => write!(
                formatter,
                "agent {agent_id} cannot transition from {from:?} to {to:?}"
            ),
            Self::FollowupIneligible { agent_id, state } => write!(
                formatter,
                "agent {agent_id} cannot start a follow-up from state {state:?}"
            ),
            Self::FollowupParentMismatch {
                agent_id,
                parent_agent_id,
            } => write!(
                formatter,
                "agent {parent_agent_id} is not authorized to follow up child {agent_id}"
            ),
            Self::FollowupParentCauseMismatch(event_id) => write!(
                formatter,
                "follow-up parent event {event_id} has the wrong agent or turn"
            ),
            Self::FollowupCapabilityWidening(agent_id) => write!(
                formatter,
                "follow-up for agent {agent_id} widens or empties its capability ceiling"
            ),
            Self::FollowupRouteChanged(agent_id) => write!(
                formatter,
                "follow-up for agent {agent_id} cannot preserve its effective model route"
            ),
            Self::InvalidFollowupObjective => {
                formatter.write_str("follow-up objective is empty or oversized")
            }
            Self::InvalidFollowupBudget => {
                formatter.write_str("follow-up budget must contain positive bounded limits")
            }
            Self::FollowupGenerationOverflow => {
                formatter.write_str("follow-up turn generation overflowed")
            }
            Self::FollowupNotAuthorized(agent_id) => write!(
                formatter,
                "agent {agent_id} has no durable follow-up authorization for terminal-to-queued"
            ),
            Self::CompletionRequiresValidatedHandoff(agent_id) => write!(
                formatter,
                "agent {agent_id} cannot complete without a validated structured handoff"
            ),
            Self::HandoffStatusMismatch { expected, actual } => write!(
                formatter,
                "handoff status {actual:?} does not match expected status {expected:?}"
            ),
            Self::PendingHandoff(agent_id) => write!(
                formatter,
                "agent {agent_id} already has a durable pending handoff"
            ),
            Self::MissingPendingHandoff(agent_id) => write!(
                formatter,
                "agent {agent_id} has no durable validated handoff for its terminal transition"
            ),
            Self::OutstandingRuntimeOperations(agent_id) => write!(
                formatter,
                "agent {agent_id} cannot finish with runtime tool operations still open"
            ),
            Self::InvalidRuntimeEvent(agent_id) => {
                write!(formatter, "agent {agent_id} emitted an invalid runtime event")
            }
            Self::InvalidRuntimeOperation(operation_id) => write!(
                formatter,
                "runtime tool operation {operation_id} has an invalid event transition"
            ),
            Self::PendingHandoffTransitionMismatch { status, lifecycle } => write!(
                formatter,
                "handoff status {status:?} does not permit lifecycle state {lifecycle:?}"
            ),
            Self::TerminalAfterHandoffFailed {
                handoff_event_id,
                source,
            } => write!(
                formatter,
                "handoff {handoff_event_id} was durable but its terminal lifecycle failed: {source}"
            ),
            Self::RunLog(error) => write!(formatter, "could not record agent lifecycle: {error}"),
            Self::QueueAfterCreationFailed {
                agent_id,
                created_event_id,
                source,
            } => write!(
                formatter,
                "agent {agent_id} was durably created by {created_event_id} but could not be queued: {source}"
            ),
        }
    }
}

impl std::error::Error for DispatchError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedAgent {
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub causing_turn_id: Option<TurnId>,
    pub kind: String,
    pub objective: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    pub dispatch_spec: Option<Box<AgentDispatchSpecSnapshot>>,
    pub state: DispatchState,
    pub turn_generation: u32,
}

/// UI-facing selection is deliberately separate from scheduler ownership and
/// writer leases. Switching it cannot pause, resume, or cancel an agent.
#[derive(Clone, Debug, Default)]
pub struct AgentFollowProjection {
    agents: BTreeMap<AgentId, ProjectedAgent>,
    children: BTreeMap<AgentId, Vec<AgentId>>,
    selected: Option<AgentId>,
    followed: Option<AgentId>,
}

impl AgentFollowProjection {
    pub fn rehydrate(events: &[EventEnvelope]) -> Result<Self, ProjectionError> {
        let mut ordered: Vec<_> = events.iter().collect();
        ordered.sort_by_key(|event| event.sequence);
        let mut projection = Self::default();
        let mut followup_authorized = BTreeSet::new();
        for event in ordered {
            if let EventKind::AgentFollowup(followup) = &event.kind {
                let agent = projection
                    .agents
                    .get_mut(&followup.agent_id)
                    .ok_or_else(|| ProjectionError::UnknownAgent(followup.agent_id.clone()))?;
                if !agent.state.is_terminal()
                    || followup.turn_generation != agent.turn_generation.saturating_add(1)
                    || event.agent_id.as_ref() != Some(&followup.agent_id)
                    || event.turn_id.as_ref() != Some(&followup.followup_turn_id)
                    || event.workspace_id != agent.workspace_id
                {
                    return Err(ProjectionError::InvalidFollowup(followup.agent_id.clone()));
                }
                agent.turn_generation = followup.turn_generation;
                agent.causing_turn_id = Some(followup.followup_turn_id.clone());
                agent.objective = Some(followup.objective.clone());
                followup_authorized.insert(followup.agent_id.clone());
                continue;
            }
            let EventKind::AgentLifecycle(lifecycle) = &event.kind else {
                continue;
            };
            if event.agent_id.as_ref() != Some(&lifecycle.agent_id) {
                return Err(ProjectionError::EnvelopeAgentMismatch(
                    lifecycle.agent_id.clone(),
                ));
            }
            let state = projected_state(&lifecycle.state);
            match projection.agents.get_mut(&lifecycle.agent_id) {
                None => {
                    if !matches!(state, DispatchState::Created | DispatchState::Queued) {
                        return Err(ProjectionError::LifecycleBeforeDispatch(
                            lifecycle.agent_id.clone(),
                        ));
                    }
                    if let Some(parent) = &lifecycle.parent_agent_id {
                        if !projection.agents.contains_key(parent) {
                            return Err(ProjectionError::UnknownParent(parent.clone()));
                        }
                        projection
                            .children
                            .entry(parent.clone())
                            .or_default()
                            .push(lifecycle.agent_id.clone());
                    }
                    projection.agents.insert(
                        lifecycle.agent_id.clone(),
                        ProjectedAgent {
                            agent_id: lifecycle.agent_id.clone(),
                            parent_agent_id: lifecycle.parent_agent_id.clone(),
                            causing_turn_id: event.turn_id.clone(),
                            kind: lifecycle.kind.clone(),
                            objective: lifecycle.objective.clone(),
                            workspace_id: event.workspace_id.clone(),
                            dispatch_spec: lifecycle.dispatch_spec.clone(),
                            state,
                            turn_generation: 0,
                        },
                    );
                    if projection.selected.is_none() {
                        projection.selected = Some(lifecycle.agent_id.clone());
                    }
                }
                Some(agent) => {
                    if agent.parent_agent_id != lifecycle.parent_agent_id {
                        return Err(ProjectionError::ParentChanged(agent.agent_id.clone()));
                    }
                    if lifecycle.dispatch_spec.is_some() {
                        return Err(ProjectionError::DispatchSpecRepeated(
                            agent.agent_id.clone(),
                        ));
                    }
                    let legacy_start = agent.state == DispatchState::Queued
                        && lifecycle.state == AgentLifecycleState::Started;
                    let authorized_followup = followup_authorized.contains(&agent.agent_id)
                        && agent.state.is_terminal()
                        && state == DispatchState::Queued;
                    if !legacy_start
                        && !authorized_followup
                        && !valid_transition(&agent.state, &state)
                    {
                        return Err(ProjectionError::InvalidLifecycle {
                            agent_id: agent.agent_id.clone(),
                            from: agent.state.clone(),
                            to: state,
                        });
                    }
                    agent.state = state;
                    if authorized_followup {
                        followup_authorized.remove(&agent.agent_id);
                    }
                }
            }
        }
        projection.followed = projection.selected.clone();
        Ok(projection)
    }

    pub fn agents(&self) -> &BTreeMap<AgentId, ProjectedAgent> {
        &self.agents
    }

    pub fn children(&self, agent_id: &AgentId) -> &[AgentId] {
        self.children
            .get(agent_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn selected(&self) -> Option<&AgentId> {
        self.selected.as_ref()
    }

    pub fn followed(&self) -> Option<&AgentId> {
        self.followed.as_ref()
    }

    pub fn select(&mut self, agent_id: AgentId) -> Result<(), ProjectionError> {
        if !self.agents.contains_key(&agent_id) {
            return Err(ProjectionError::UnknownAgent(agent_id));
        }
        self.selected = Some(agent_id);
        Ok(())
    }

    pub fn follow(&mut self, agent_id: AgentId) -> Result<(), ProjectionError> {
        if !self.agents.contains_key(&agent_id) {
            return Err(ProjectionError::UnknownAgent(agent_id));
        }
        self.followed = Some(agent_id);
        Ok(())
    }
}

fn projected_state(state: &AgentLifecycleState) -> DispatchState {
    match state {
        AgentLifecycleState::Created => DispatchState::Created,
        AgentLifecycleState::Dispatched | AgentLifecycleState::Queued => DispatchState::Queued,
        AgentLifecycleState::Starting => DispatchState::Starting,
        AgentLifecycleState::Started | AgentLifecycleState::Running => DispatchState::Running,
        AgentLifecycleState::Waiting | AgentLifecycleState::WaitingForAgent => {
            DispatchState::WaitingForAgent
        }
        AgentLifecycleState::WaitingForTool => DispatchState::WaitingForTool,
        AgentLifecycleState::Blocked | AgentLifecycleState::WaitingForUser => {
            DispatchState::WaitingForUser
        }
        AgentLifecycleState::Completed => DispatchState::Completed,
        AgentLifecycleState::Interrupted => DispatchState::Interrupted,
        AgentLifecycleState::Failed => DispatchState::Failed,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    EnvelopeAgentMismatch(AgentId),
    LifecycleBeforeDispatch(AgentId),
    UnknownParent(AgentId),
    ParentChanged(AgentId),
    DispatchSpecRepeated(AgentId),
    InvalidFollowup(AgentId),
    InvalidLifecycle {
        agent_id: AgentId,
        from: DispatchState,
        to: DispatchState,
    },
    UnknownAgent(AgentId),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ProjectionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{
        HandoffConfidence, HandoffEvidence, HandoffValidator, StructuredHandoffV1,
    };
    use crate::ai::AiConfig;
    use crate::run_log::{
        AgentModelProfileSnapshot, InMemoryRunEventSink, MessageEvent, MessageRole,
    };

    fn catalog() -> Arc<SubagentModelCatalog> {
        Arc::new(SubagentModelCatalog::from_config(&AiConfig::default()).unwrap())
    }

    fn scheduler() -> (AgentDispatchScheduler, Arc<InMemoryRunEventSink>) {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let scheduler = AgentDispatchScheduler::new(
            RunId::parse("run_dispatch_test").unwrap(),
            sink.clone(),
            catalog(),
        );
        (scheduler, sink)
    }

    fn request(
        kind: AgentKind,
        workspace_id: WorkspaceId,
        strategy: WorkspaceStrategy,
    ) -> DispatchRequest {
        static NEXT_TASK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        DispatchRequest {
            task_name: format!(
                "task_{}",
                NEXT_TASK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            objective: "bounded delegated objective".into(),
            role: kind,
            requested_route: RequestedModelRoute::exact(
                super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
                ReasoningEffort::none(),
            ),
            parent_agent_id: None,
            causing_turn_id: None,
            caused_by_event: None,
            workspace: WorkspaceAssignment {
                workspace_id,
                strategy,
            },
        }
    }

    fn handoff(status: HandoffStatus) -> ValidatedHandoff {
        let completed = status == HandoffStatus::Completed;
        HandoffValidator::default()
            .validate(
                StructuredHandoffV1 {
                    version: 1,
                    status,
                    summary: if completed {
                        "Completed the bounded delegated objective."
                    } else {
                        "Stopped with partial evidence."
                    }
                    .into(),
                    evidence: completed
                        .then(|| HandoffEvidence {
                            path: "ovim-core/src/agent_runtime/dispatch.rs".into(),
                            line: Some(1),
                            claim: "The scheduler owns the completion gate.".into(),
                        })
                        .into_iter()
                        .collect(),
                    changed_files: vec![],
                    verification: vec![],
                    blockers: (!completed)
                        .then(|| "Provider did not complete the objective.".into())
                        .into_iter()
                        .collect(),
                    followups: vec![],
                    confidence: HandoffConfidence::High,
                },
                Some(status),
            )
            .unwrap()
    }

    fn record_parent_turn(
        sink: &InMemoryRunEventSink,
        parent: &DispatchHandle,
        turn_id: TurnId,
    ) -> EventEnvelope {
        let prior = sink
            .events(&parent.run_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| event.agent_id.as_ref() == Some(&parent.agent_id))
            .unwrap();
        sink.append(NewRunEvent {
            run_id: parent.run_id.clone(),
            caused_by: Some(prior.event_id),
            operation_id: None,
            provider_call_id: None,
            actor: EventActor::Agent(parent.agent_id.clone()),
            agent_id: Some(parent.agent_id.clone()),
            turn_id: Some(turn_id),
            workspace_id: Some(parent.workspace.workspace_id.clone()),
            branch_id: None,
            kind: EventKind::Message(MessageEvent {
                role: MessageRole::Agent,
                content: "dispatch child".into(),
            }),
        })
        .unwrap()
    }

    fn start_running(scheduler: &mut AgentDispatchScheduler, handle: &DispatchHandle) {
        scheduler
            .transition(handle, DispatchState::Starting, None)
            .unwrap();
        scheduler
            .transition(handle, DispatchState::Running, None)
            .unwrap();
    }

    /// OV-00318: an append failure between the durable handoff and its
    /// terminal lifecycle event must be recoverable in-process. A retry must
    /// resume the recorded handoff (here: Completed) rather than append a
    /// second one or stay wedged behind `DispatchError::PendingHandoff`.
    #[test]
    fn interrupted_terminal_append_resumes_from_durable_handoff() {
        use crate::agent_runtime::test_support::FlakySink;

        let inner = Arc::new(crate::run_log::InMemoryRunEventSink::new());
        let flaky = Arc::new(FlakySink::new(inner.clone()));
        let run_id = RunId::parse("run_pending_handoff_test").unwrap();
        let mut scheduler = AgentDispatchScheduler::new(run_id.clone(), flaky.clone(), catalog());
        let child = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::new(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        start_running(&mut scheduler, &child);

        // Let the handoff append land, then fail the terminal lifecycle append.
        flaky.fail_after(1);
        let error = scheduler
            .finish_with_handoff(&child, handoff(HandoffStatus::Completed))
            .unwrap_err();
        assert!(
            matches!(error, DispatchError::TerminalAfterHandoffFailed { .. }),
            "expected the terminal append to fail after the handoff landed; got {error:?}"
        );
        assert_eq!(
            scheduler.state(&child.agent_id),
            Some(&DispatchState::Running),
            "dispatch must stay non-terminal until the lifecycle event lands"
        );

        // Appending a second handoff is still refused: the durable one is the gate.
        let refused = scheduler
            .finish_with_handoff(&child, handoff(HandoffStatus::Completed))
            .unwrap_err();
        assert!(matches!(refused, DispatchError::PendingHandoff(_)));

        // Once appends work again, the conservative retry resumes the durable
        // handoff: terminal state comes from the recorded Completed handoff,
        // not from the retry's Failed status, and no second handoff appears.
        flaky.heal();
        let terminal = scheduler
            .terminate_conservatively(&child, HandoffStatus::Failed, "retry after disk full")
            .unwrap();
        assert_eq!(
            scheduler.state(&child.agent_id),
            Some(&DispatchState::Completed)
        );
        assert!(matches!(
            terminal.handoff_event.kind,
            EventKind::AgentHandoff(_)
        ));
        let handoff_count = inner
            .events(&run_id)
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::AgentHandoff(_)))
            .count();
        assert_eq!(
            handoff_count, 1,
            "the durable handoff must not be duplicated"
        );
    }

    fn followup_budget() -> AgentLoopBudget {
        AgentLoopBudget {
            timeout: std::time::Duration::from_secs(45),
            max_provider_events: 32,
            max_tool_calls: 8,
        }
    }

    #[test]
    fn followup_reopens_same_agent_with_fresh_turn_and_narrowed_ceiling() {
        let (mut scheduler, sink) = scheduler();
        let workspace_id = WorkspaceId::new();
        let parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace_id.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let first_parent_turn = TurnId::new();
        let first_cause = record_parent_turn(&sink, &parent, first_parent_turn.clone());
        let mut child_request = request(
            AgentKind::built_in(AgentKindName::Planner),
            workspace_id,
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        );
        child_request.parent_agent_id = Some(parent.agent_id.clone());
        child_request.causing_turn_id = Some(first_parent_turn);
        child_request.caused_by_event = Some(first_cause.event_id);
        let child = scheduler.dispatch(child_request).unwrap();
        start_running(&mut scheduler, &child);
        scheduler
            .finish_with_handoff(&child, handoff(HandoffStatus::Completed))
            .unwrap();

        let followup_parent_turn = TurnId::new();
        let followup_cause = record_parent_turn(&sink, &parent, followup_parent_turn.clone());
        let before = scheduler.dispatch_record(&child.agent_id).unwrap();
        let followup = scheduler
            .begin_followup(FollowupAgentRequest {
                agent_id: child.agent_id.clone(),
                parent_agent_id: parent.agent_id.clone(),
                causing_turn_id: followup_parent_turn,
                caused_by_event: followup_cause.event_id,
                objective: "Check the restart boundary too.".into(),
                capabilities: Some(BTreeSet::from([AgentCapability::Read])),
                budget: followup_budget(),
                retained_session_requested: true,
            })
            .unwrap();
        let after = scheduler.dispatch_record(&child.agent_id).unwrap();

        assert_eq!(followup.handle.agent_id, child.agent_id);
        assert_ne!(followup.followup_turn_id, before.causing_turn_id.unwrap());
        assert_eq!(followup.turn_generation, 1);
        assert_eq!(after.state, DispatchState::Queued);
        assert_eq!(after.turn_generation, 1);
        assert_eq!(after.handle.workspace, before.handle.workspace);
        assert_eq!(after.resolved_route, before.resolved_route);
        assert_eq!(
            after.role.capabilities,
            BTreeSet::from([AgentCapability::Read])
        );
        assert_eq!(after.followup.unwrap().budget, followup_budget());

        let events = sink.events(&child.run_id).unwrap();
        let projection = AgentFollowProjection::rehydrate(&events).unwrap();
        let projected = projection.agents().get(&child.agent_id).unwrap();
        assert_eq!(projected.turn_generation, 1);
        assert_eq!(projected.state, DispatchState::Queued);
        let restored = AgentDispatchScheduler::rehydrate(child.run_id, sink, catalog()).unwrap();
        let restored = restored.dispatch_record(&child.agent_id).unwrap();
        assert_eq!(restored.turn_generation, 1);
        assert_eq!(restored.state, DispatchState::Queued);
        assert_eq!(restored.handle.agent_id, child.agent_id);
    }

    #[test]
    fn followup_rejects_live_failed_and_capability_widening_targets() {
        let (mut scheduler, sink) = scheduler();
        let workspace_id = WorkspaceId::new();
        let parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace_id.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let parent_turn = TurnId::new();
        let cause = record_parent_turn(&sink, &parent, parent_turn.clone());
        let mut child_request = request(
            AgentKind::built_in(AgentKindName::Explorer),
            workspace_id,
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        );
        child_request.parent_agent_id = Some(parent.agent_id.clone());
        child_request.causing_turn_id = Some(parent_turn.clone());
        child_request.caused_by_event = Some(cause.event_id.clone());
        let child = scheduler.dispatch(child_request).unwrap();
        assert!(matches!(
            scheduler.begin_followup(FollowupAgentRequest {
                agent_id: child.agent_id.clone(),
                parent_agent_id: parent.agent_id.clone(),
                causing_turn_id: parent_turn.clone(),
                caused_by_event: cause.event_id.clone(),
                objective: "too soon".into(),
                capabilities: None,
                budget: followup_budget(),
                retained_session_requested: false,
            }),
            Err(DispatchError::FollowupIneligible {
                state: DispatchState::Queued,
                ..
            })
        ));
        start_running(&mut scheduler, &child);
        scheduler
            .finish_with_handoff(&child, handoff(HandoffStatus::Failed))
            .unwrap();
        let second_turn = TurnId::new();
        let second_cause = record_parent_turn(&sink, &parent, second_turn.clone());
        assert!(matches!(
            scheduler.begin_followup(FollowupAgentRequest {
                agent_id: child.agent_id,
                parent_agent_id: parent.agent_id,
                causing_turn_id: second_turn,
                caused_by_event: second_cause.event_id,
                objective: "not after failure".into(),
                capabilities: Some(BTreeSet::from([
                    AgentCapability::Read,
                    AgentCapability::Navigate,
                ])),
                budget: followup_budget(),
                retained_session_requested: false,
            }),
            Err(DispatchError::FollowupIneligible {
                state: DispatchState::Failed,
                ..
            })
        ));
    }

    #[test]
    fn child_has_ovim_identity_and_durable_parent_turn_causality() {
        let (mut scheduler, sink) = scheduler();
        let workspace = WorkspaceId::parse("wsp_parent_child").unwrap();
        let parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let turn = TurnId::parse("trn_causing_dispatch").unwrap();
        let cause = record_parent_turn(&sink, &parent, turn.clone());
        let child = scheduler
            .dispatch(DispatchRequest {
                task_name: "inspect_storage".into(),
                objective: "inspect the storage layer".into(),
                role: AgentKind::built_in(AgentKindName::Explorer),
                requested_route: RequestedModelRoute::exact(
                    super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
                    ReasoningEffort::none(),
                ),
                parent_agent_id: Some(parent.agent_id.clone()),
                causing_turn_id: Some(turn.clone()),
                caused_by_event: Some(cause.event_id.clone()),
                workspace: WorkspaceAssignment {
                    workspace_id: workspace,
                    strategy: WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
                },
            })
            .unwrap();

        assert_ne!(parent.agent_id, child.agent_id);
        let events = sink.events(&parent.run_id).unwrap();
        let child_event = events
            .iter()
            .find(|event| event.agent_id.as_ref() == Some(&child.agent_id))
            .unwrap();
        let EventKind::AgentLifecycle(lifecycle) = &child_event.kind else {
            panic!("child dispatch was not a lifecycle event")
        };
        assert_eq!(lifecycle.parent_agent_id.as_ref(), Some(&parent.agent_id));
        assert_eq!(child_event.turn_id.as_ref(), Some(&turn));
        assert_eq!(child_event.actor, EventActor::Agent(parent.agent_id));
        assert_eq!(child_event.caused_by.as_ref(), Some(&cause.event_id));
        assert!(child_event.provider_call_id.is_none());
    }

    #[test]
    fn concurrent_read_agents_share_a_snapshot() {
        let (mut scheduler, _) = scheduler();
        let workspace = WorkspaceId::parse("wsp_shared_readers").unwrap();
        let first = scheduler.dispatch(request(
            AgentKind::built_in(AgentKindName::Explorer),
            workspace.clone(),
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        ));
        let second = scheduler.dispatch(request(
            AgentKind::built_in(AgentKindName::Reviewer),
            workspace,
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        ));
        assert!(first.is_ok());
        assert!(second.is_ok());
    }

    #[test]
    fn task_names_are_strict_and_unique_before_durable_allocation() {
        let (mut scheduler, sink) = scheduler();
        let workspace = WorkspaceId::parse("wsp_task_names").unwrap();
        let mut first = request(
            AgentKind::built_in(AgentKindName::Explorer),
            workspace.clone(),
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        );
        first.task_name = "inspect_store".into();
        scheduler.dispatch(first.clone()).unwrap();
        let durable_count = sink
            .events(&RunId::parse("run_dispatch_test").unwrap())
            .unwrap()
            .len();

        assert_eq!(
            scheduler.dispatch(first).unwrap_err(),
            DispatchError::DuplicateTaskName("inspect_store".into())
        );
        let mut invalid = request(
            AgentKind::built_in(AgentKindName::Reviewer),
            workspace,
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        );
        invalid.task_name = "Not Valid".into();
        assert!(matches!(
            scheduler.dispatch(invalid),
            Err(DispatchError::InvalidTaskName(_))
        ));
        assert_eq!(
            sink.events(&RunId::parse("run_dispatch_test").unwrap())
                .unwrap()
                .len(),
            durable_count
        );
    }

    #[test]
    fn child_dispatch_requires_a_matching_parent_turn_event() {
        let (mut scheduler, sink) = scheduler();
        let workspace = WorkspaceId::parse("wsp_causal_rejections").unwrap();
        let parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let turn = TurnId::parse("trn_expected_cause").unwrap();
        let child_request = |causing_turn_id, caused_by_event| DispatchRequest {
            task_name: "causal_child".into(),
            objective: "causally delegated work".into(),
            role: AgentKind::built_in(AgentKindName::Explorer),
            requested_route: RequestedModelRoute::exact(
                super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
                ReasoningEffort::none(),
            ),
            parent_agent_id: Some(parent.agent_id.clone()),
            causing_turn_id: Some(causing_turn_id),
            caused_by_event,
            workspace: WorkspaceAssignment {
                workspace_id: workspace.clone(),
                strategy: WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            },
        };
        assert_eq!(
            scheduler
                .dispatch(child_request(turn.clone(), None))
                .unwrap_err(),
            DispatchError::ChildMissingCausingEvent
        );

        let cause = record_parent_turn(&sink, &parent, turn.clone());
        assert!(matches!(
            scheduler
                .dispatch(child_request(
                    TurnId::parse("trn_wrong_cause").unwrap(),
                    Some(cause.event_id.clone()),
                ))
                .unwrap_err(),
            DispatchError::CausingEventTurnMismatch { event_id, .. }
                if event_id == cause.event_id
        ));

        let other_parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let other_cause = record_parent_turn(&sink, &other_parent, turn.clone());
        assert!(matches!(
            scheduler
                .dispatch(child_request(turn, Some(other_cause.event_id.clone())))
                .unwrap_err(),
            DispatchError::CausingEventAgentMismatch { event_id, .. }
                if event_id == other_cause.event_id
        ));
    }

    #[test]
    fn dispatch_records_created_then_queued_and_rejects_invalid_lifecycle_edges() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_lifecycle").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let events = sink.events(&handle.run_id).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind,
            EventKind::AgentLifecycle(AgentLifecycleEvent {
                state: AgentLifecycleState::Created,
                ..
            })
        ));
        assert!(matches!(
            events[1].kind,
            EventKind::AgentLifecycle(AgentLifecycleEvent {
                state: AgentLifecycleState::Queued,
                ..
            })
        ));
        assert_eq!(events[1].caused_by.as_ref(), Some(&events[0].event_id));

        assert!(matches!(
            scheduler.transition(&handle, DispatchState::Completed, None),
            Err(DispatchError::InvalidTransition {
                from: DispatchState::Queued,
                to: DispatchState::Completed,
                ..
            })
        ));
        assert_eq!(
            scheduler.state(&handle.agent_id),
            Some(&DispatchState::Queued)
        );
    }

    #[test]
    fn completed_lifecycle_requires_a_durable_validated_handoff() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_completion_gate").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        start_running(&mut scheduler, &handle);
        let events_before = sink.events(&handle.run_id).unwrap().len();

        assert_eq!(
            scheduler
                .transition(&handle, DispatchState::Completed, None)
                .unwrap_err(),
            DispatchError::CompletionRequiresValidatedHandoff(handle.agent_id.clone())
        );
        assert_eq!(sink.events(&handle.run_id).unwrap().len(), events_before);

        let record = scheduler
            .complete_with_handoff(&handle, handoff(HandoffStatus::Completed))
            .unwrap();
        assert!(matches!(
            record.handoff_event.kind,
            EventKind::AgentHandoff(_)
        ));
        assert!(matches!(
            record.terminal_event.kind,
            EventKind::AgentLifecycle(AgentLifecycleEvent {
                state: AgentLifecycleState::Completed,
                ..
            })
        ));
        assert_eq!(
            record.terminal_event.caused_by.as_ref(),
            Some(&record.handoff_event.event_id)
        );
        assert_eq!(
            scheduler.state(&handle.agent_id),
            Some(&DispatchState::Completed)
        );

        drop(scheduler);
        let restored = AgentDispatchScheduler::rehydrate(handle.run_id, sink, catalog()).unwrap();
        assert_eq!(
            restored.state(&handle.agent_id),
            Some(&DispatchState::Completed)
        );
    }

    #[test]
    fn rehydrate_retries_queued_work_only_when_exact_route_is_still_available() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_catalog_recovery").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        drop(scheduler);

        let mut changed = AiConfig::default();
        changed.profiles.get_mut("local").unwrap().model = "different-model".into();
        let changed_catalog = Arc::new(SubagentModelCatalog::from_config(&changed).unwrap());
        let recovered =
            AgentDispatchScheduler::rehydrate(handle.run_id.clone(), sink.clone(), changed_catalog)
                .unwrap();

        assert_eq!(
            recovered.state(&handle.agent_id),
            Some(&DispatchState::Interrupted)
        );
        assert!(sink.events(&handle.run_id).unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::AgentHandoff(recorded)
                    if recorded.handoff.as_handoff().summary.contains("current model catalog")
            )
        }));
    }

    #[test]
    fn partial_handoffs_preserve_explicit_non_completion_status() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_partial_handoff").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        start_running(&mut scheduler, &handle);

        scheduler
            .finish_with_handoff(&handle, handoff(HandoffStatus::TimedOut))
            .unwrap();
        assert_eq!(
            scheduler.state(&handle.agent_id),
            Some(&DispatchState::Interrupted)
        );
        assert!(sink.events(&handle.run_id).unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::AgentHandoff(recorded)
                    if recorded.handoff.status() == HandoffStatus::TimedOut
            )
        }));
    }

    #[test]
    fn rehydrate_rejects_completed_history_without_a_handoff() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_invalid_completion_history").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        start_running(&mut scheduler, &handle);
        let agent = scheduler.agents[&handle.agent_id].clone();
        sink.append(NewRunEvent {
            run_id: handle.run_id.clone(),
            caused_by: Some(agent.last_event_id),
            operation_id: None,
            provider_call_id: None,
            actor: EventActor::System("malformed_history_test".into()),
            agent_id: Some(handle.agent_id.clone()),
            turn_id: agent.causing_turn_id,
            workspace_id: Some(handle.workspace.workspace_id.clone()),
            branch_id: None,
            kind: lifecycle_event(
                &handle.agent_id,
                agent.parent_agent_id,
                &agent.role,
                Some(agent.objective),
                None,
                DispatchState::Completed,
                None,
            ),
        })
        .unwrap();
        drop(scheduler);

        assert!(matches!(
            AgentDispatchScheduler::rehydrate(handle.run_id, sink, catalog()),
            Err(DispatchError::InvalidHistory(detail))
                if detail.contains("completed without a validated handoff")
        ));
    }

    #[test]
    fn rehydrate_finishes_a_handoff_that_was_durable_at_process_stop() {
        let (mut scheduler, sink) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_pending_handoff_recovery").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        start_running(&mut scheduler, &handle);
        let agent = scheduler.agents[&handle.agent_id].clone();
        let recorded = sink
            .append(NewRunEvent {
                run_id: handle.run_id.clone(),
                caused_by: Some(agent.last_event_id),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("process_stop_test".into()),
                agent_id: Some(handle.agent_id.clone()),
                turn_id: agent.causing_turn_id,
                workspace_id: Some(handle.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::AgentHandoff(AgentHandoffEvent {
                    handoff: handoff(HandoffStatus::Completed),
                    workspace_warnings: Vec::new(),
                }),
            })
            .unwrap();
        drop(scheduler);

        let restored =
            AgentDispatchScheduler::rehydrate(handle.run_id.clone(), sink.clone(), catalog())
                .unwrap();
        assert_eq!(
            restored.state(&handle.agent_id),
            Some(&DispatchState::Completed)
        );
        let events = sink.events(&handle.run_id).unwrap();
        let terminal = events.last().unwrap();
        assert_eq!(terminal.caused_by.as_ref(), Some(&recorded.event_id));
        assert!(matches!(
            terminal.kind,
            EventKind::AgentLifecycle(AgentLifecycleEvent {
                state: AgentLifecycleState::Completed,
                ..
            })
        ));
    }

    #[test]
    fn resolved_custom_dispatch_spec_round_trips_and_rehydrates() {
        let (mut scheduler, sink) = scheduler();
        let role = AgentKind {
            name: AgentKindName::Custom("migration-auditor".into()),
            instructions: "Audit migrations using the pinned policy revision.".into(),
            capabilities: BTreeSet::from([
                AgentCapability::Read,
                AgentCapability::Navigate,
                AgentCapability::SafeShell,
            ]),
            workspace_policy: WorkspacePolicy::ReadOnlyProjection,
            completion_contract: CompletionContract::Custom("migration-audit-v3".into()),
        };
        let requested_route = RequestedModelRoute::exact(
            super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
            ReasoningEffort::none(),
        );
        let handle = scheduler
            .dispatch(DispatchRequest {
                task_name: "custom_snapshot".into(),
                objective: "bounded delegated objective".into(),
                role,
                requested_route: requested_route.clone(),
                parent_agent_id: None,
                causing_turn_id: None,
                caused_by_event: None,
                workspace: WorkspaceAssignment {
                    workspace_id: WorkspaceId::parse("wsp_custom_snapshot").unwrap(),
                    strategy: WorkspaceStrategy::ReadOnlySnapshot {
                        manifest_id: Some(ManifestId::parse("mft_custom_snapshot").unwrap()),
                    },
                },
            })
            .unwrap();
        let resolved_route = scheduler.resolved_route(&handle.agent_id).unwrap().clone();
        let events = sink.events(&handle.run_id).unwrap();
        let EventKind::AgentLifecycle(created) = &events[0].kind else {
            panic!("expected created lifecycle")
        };
        let snapshot = created.dispatch_spec.as_ref().unwrap();
        assert_eq!(snapshot.version, 2);
        assert!(snapshot.legacy_model.is_none());
        assert_eq!(
            snapshot.requested_route.as_ref().unwrap().catalog_model_id,
            requested_route.catalog_model_id
        );
        assert_eq!(
            snapshot.resolved_route.as_ref().unwrap().profile_name,
            "local"
        );
        assert_eq!(
            snapshot.completion_contract,
            AgentCompletionContractSnapshot::Custom("migration-audit-v3".into())
        );
        assert!(matches!(
            snapshot.assigned_workspace,
            AgentWorkspaceStrategySnapshot::ReadOnlySnapshot { .. }
        ));
        assert!(matches!(
            events[1].kind,
            EventKind::AgentLifecycle(AgentLifecycleEvent {
                dispatch_spec: None,
                ..
            })
        ));

        let wire = serde_json::to_string(&events[0].kind).unwrap();
        let restored: EventKind = serde_json::from_str(&wire).unwrap();
        assert_eq!(restored, events[0].kind);
        let projection = AgentFollowProjection::rehydrate(&events).unwrap();
        assert_eq!(
            projection.agents()[&handle.agent_id].dispatch_spec.as_ref(),
            Some(snapshot)
        );
        drop(scheduler);
        let rehydrated = AgentDispatchScheduler::rehydrate(handle.run_id, sink, catalog()).unwrap();
        assert_eq!(
            rehydrated.requested_route(&handle.agent_id),
            Some(&requested_route)
        );
        assert_eq!(
            rehydrated.resolved_route(&handle.agent_id),
            Some(&resolved_route)
        );
    }

    #[test]
    fn invalid_explicit_effort_creates_no_durable_agent() {
        let (mut scheduler, sink) = scheduler();
        let mut invalid = request(
            AgentKind::built_in(AgentKindName::Explorer),
            WorkspaceId::parse("wsp_invalid_route").unwrap(),
            WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
        );
        invalid.requested_route.reasoning_effort = ReasoningEffort::high();

        assert!(matches!(
            scheduler.dispatch(invalid),
            Err(DispatchError::ModelRoute(ModelRouteError::InvalidEffort {
                requested,
                ..
            })) if requested == ReasoningEffort::high()
        ));
        assert!(sink
            .events(&RunId::parse("run_dispatch_test").unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn version_one_snapshot_rehydrates_with_explicit_historic_route_marker() {
        let (mut scheduler, source) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_v1_history").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let created = source.events(&handle.run_id).unwrap().remove(0);
        let EventKind::AgentLifecycle(mut lifecycle) = created.kind else {
            panic!("expected lifecycle")
        };
        let snapshot = lifecycle.dispatch_spec.as_mut().unwrap();
        snapshot.version = 1;
        snapshot.legacy_model = Some(AgentModelProfileSnapshot {
            model: "legacy-model".into(),
            effort: AgentModelEffortSnapshot::Medium,
            fallback_model: Some("legacy-fallback".into()),
        });
        snapshot.requested_route = None;
        snapshot.resolved_route = None;
        let wire = serde_json::to_value(EventKind::AgentLifecycle(lifecycle)).unwrap();
        assert_eq!(
            wire.pointer("/data/dispatch_spec/model/model"),
            Some(&serde_json::json!("legacy-model"))
        );
        assert!(wire
            .pointer("/data/dispatch_spec/requested_route")
            .is_none());
        let EventKind::AgentLifecycle(lifecycle) = serde_json::from_value(wire).unwrap() else {
            panic!("version-one wire event did not decode as a lifecycle")
        };

        let history = Arc::new(InMemoryRunEventSink::new());
        history
            .append(NewRunEvent {
                run_id: handle.run_id.clone(),
                caused_by: None,
                operation_id: None,
                provider_call_id: None,
                actor: created.actor,
                agent_id: created.agent_id,
                turn_id: created.turn_id,
                workspace_id: created.workspace_id,
                branch_id: None,
                kind: EventKind::AgentLifecycle(lifecycle),
            })
            .unwrap();

        let rehydrated =
            AgentDispatchScheduler::rehydrate(handle.run_id, history, catalog()).unwrap();
        let requested = rehydrated.requested_route(&handle.agent_id).unwrap();
        let resolved = rehydrated.resolved_route(&handle.agent_id).unwrap();
        assert_eq!(requested.catalog_model_id, "legacy-model");
        assert_eq!(requested.reasoning_effort, ReasoningEffort::medium());
        assert_eq!(
            requested.fallback_policy,
            ModelFallbackPolicy::Explicit {
                catalog_model_id: "legacy-fallback".into(),
                reasoning_effort: ReasoningEffort::medium(),
            }
        );
        assert_eq!(resolved.catalog_generation, "historic-v1");
        assert_eq!(resolved.profile_name, "historic-v1");
        assert_eq!(resolved.resolution, ModelRouteResolution::HistoricV1);
    }

    #[test]
    fn shared_workspace_allows_only_one_writer_until_terminal() {
        let (mut scheduler, _) = scheduler();
        let workspace = WorkspaceId::parse("wsp_shared_writer").unwrap();
        let first = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::SharedWorkspace,
            ))
            .unwrap();
        let rejected = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::SharedWorkspace,
            ))
            .unwrap_err();
        assert!(matches!(
            rejected,
            DispatchError::SharedWorkspaceWriterHeld {
                workspace_id,
                holder
            } if workspace_id == workspace && holder == first.agent_id
        ));

        scheduler
            .transition(&first, DispatchState::Starting, None)
            .unwrap();
        scheduler
            .transition(&first, DispatchState::Running, None)
            .unwrap();
        scheduler
            .complete_with_handoff(&first, handoff(HandoffStatus::Completed))
            .unwrap();
        assert!(scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace,
                WorkspaceStrategy::SharedWorkspace,
            ))
            .is_ok());
    }

    #[test]
    fn isolated_worktree_writers_may_coexist() {
        let (mut scheduler, _) = scheduler();
        let base = WorkspaceId::parse("wsp_isolated_base").unwrap();
        let first = scheduler.dispatch(request(
            AgentKind::built_in(AgentKindName::Implementer),
            base.clone(),
            WorkspaceStrategy::IsolatedWorktree {
                base_manifest_id: None,
            },
        ));
        let second = scheduler.dispatch(request(
            AgentKind::built_in(AgentKindName::Implementer),
            base,
            WorkspaceStrategy::IsolatedWorktree {
                base_manifest_id: None,
            },
        ));
        assert!(first.is_ok());
        assert!(second.is_ok());
    }

    #[test]
    fn restart_rehydrates_shared_writer_lease_and_excludes_second_writer() {
        let (mut scheduler, sink) = scheduler();
        let run_id = scheduler.run_id.clone();
        let workspace = WorkspaceId::parse("wsp_restart_shared_writer").unwrap();
        let first = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::SharedWorkspace,
            ))
            .unwrap();
        drop(scheduler);

        let mut restored = AgentDispatchScheduler::rehydrate(run_id, sink, catalog()).unwrap();
        assert_eq!(
            restored.state(&first.agent_id),
            Some(&DispatchState::Queued)
        );
        let rejected = restored
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::SharedWorkspace,
            ))
            .unwrap_err();
        assert!(matches!(
            rejected,
            DispatchError::SharedWorkspaceWriterHeld {
                workspace_id,
                holder,
            } if workspace_id == workspace && holder == first.agent_id
        ));
    }

    #[test]
    fn restart_allows_isolated_writers_to_coexist() {
        let (mut scheduler, sink) = scheduler();
        let run_id = scheduler.run_id.clone();
        let workspace = WorkspaceId::parse("wsp_restart_isolated_writers").unwrap();
        let first = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::IsolatedWorktree {
                    base_manifest_id: None,
                },
            ))
            .unwrap();
        let second = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::IsolatedWorktree {
                    base_manifest_id: None,
                },
            ))
            .unwrap();
        drop(scheduler);

        let mut restored = AgentDispatchScheduler::rehydrate(run_id, sink, catalog()).unwrap();
        assert_eq!(
            restored.state(&first.agent_id),
            Some(&DispatchState::Queued)
        );
        assert_eq!(
            restored.state(&second.agent_id),
            Some(&DispatchState::Queued)
        );
        assert!(restored
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace,
                WorkspaceStrategy::IsolatedWorktree {
                    base_manifest_id: None,
                },
            ))
            .is_ok());
    }

    #[test]
    fn restart_interrupts_in_flight_agent_and_releases_shared_writer() {
        let (mut scheduler, sink) = scheduler();
        let run_id = scheduler.run_id.clone();
        let workspace = WorkspaceId::parse("wsp_restart_running_writer").unwrap();
        let first = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace.clone(),
                WorkspaceStrategy::SharedWorkspace,
            ))
            .unwrap();
        scheduler
            .transition(&first, DispatchState::Starting, None)
            .unwrap();
        scheduler
            .transition(&first, DispatchState::Running, None)
            .unwrap();
        drop(scheduler);

        let mut restored =
            AgentDispatchScheduler::rehydrate(run_id.clone(), sink.clone(), catalog()).unwrap();
        assert_eq!(
            restored.state(&first.agent_id),
            Some(&DispatchState::Interrupted)
        );
        let events = sink.events(&run_id).unwrap();
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(EventKind::AgentLifecycle(AgentLifecycleEvent {
                state: AgentLifecycleState::Interrupted,
                ..
            }))
        ));
        assert!(restored
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Implementer),
                workspace,
                WorkspaceStrategy::SharedWorkspace,
            ))
            .is_ok());
    }

    #[test]
    fn rehydrate_rejects_changed_workspace_identity() {
        let (mut scheduler, sink) = scheduler();
        let run_id = scheduler.run_id.clone();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_original_identity").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let mut events = sink.events(&run_id).unwrap();
        let queued = events.pop().unwrap();
        sink.append(NewRunEvent {
            run_id: run_id.clone(),
            caused_by: queued.caused_by,
            operation_id: None,
            provider_call_id: None,
            actor: queued.actor,
            agent_id: queued.agent_id,
            turn_id: queued.turn_id,
            workspace_id: Some(WorkspaceId::parse("wsp_changed_identity").unwrap()),
            branch_id: None,
            kind: queued.kind,
        })
        .unwrap();
        drop(scheduler);

        assert!(matches!(
            AgentDispatchScheduler::rehydrate(run_id, sink, catalog()),
            Err(DispatchError::InvalidHistory(detail))
                if detail.contains("workspace identity")
        ));
        let _ = handle;
    }

    #[test]
    fn rehydrate_rejects_unsupported_dispatch_spec_version() {
        let (mut scheduler, source) = scheduler();
        let handle = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                WorkspaceId::parse("wsp_invalid_spec_version").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let created = source.events(&handle.run_id).unwrap().remove(0);
        let EventKind::AgentLifecycle(mut lifecycle) = created.kind else {
            panic!("expected lifecycle")
        };
        lifecycle.dispatch_spec.as_mut().unwrap().version = 99;
        let corrupted = Arc::new(InMemoryRunEventSink::new());
        corrupted
            .append(NewRunEvent {
                run_id: handle.run_id.clone(),
                caused_by: None,
                operation_id: None,
                provider_call_id: None,
                actor: created.actor,
                agent_id: created.agent_id,
                turn_id: created.turn_id,
                workspace_id: created.workspace_id,
                branch_id: None,
                kind: EventKind::AgentLifecycle(lifecycle),
            })
            .unwrap();

        assert!(matches!(
            AgentDispatchScheduler::rehydrate(handle.run_id, corrupted, catalog()),
            Err(DispatchError::InvalidHistory(detail))
                if detail.contains("unsupported dispatch spec version 99")
        ));
    }

    #[test]
    fn arbitrary_shell_is_scheduled_as_write_capable() {
        let (mut scheduler, _) = scheduler();
        let rejection = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Verifier),
                WorkspaceId::parse("wsp_read_only_verifier").unwrap(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap_err();
        assert_eq!(rejection, DispatchError::WriteCapabilityInReadOnlyWorkspace);
    }

    #[test]
    fn follow_and_selection_switch_without_changing_scheduler_state() {
        let (mut scheduler, sink) = scheduler();
        let workspace = WorkspaceId::parse("wsp_projection_switch").unwrap();
        let first = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Explorer),
                workspace.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let second = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Reviewer),
                workspace,
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let events = sink.events(&first.run_id).unwrap();
        let mut projection = AgentFollowProjection::rehydrate(&events).unwrap();
        projection.select(second.agent_id.clone()).unwrap();
        projection.follow(first.agent_id.clone()).unwrap();

        assert_eq!(projection.selected(), Some(&second.agent_id));
        assert_eq!(projection.followed(), Some(&first.agent_id));
        assert_eq!(
            scheduler.state(&first.agent_id),
            Some(&DispatchState::Queued)
        );
        assert_eq!(
            scheduler.state(&second.agent_id),
            Some(&DispatchState::Queued)
        );
    }

    #[test]
    fn projection_rehydrates_tree_and_lifecycle_from_events() {
        let (mut scheduler, sink) = scheduler();
        let workspace = WorkspaceId::parse("wsp_projection_rehydrate").unwrap();
        let parent = scheduler
            .dispatch(request(
                AgentKind::built_in(AgentKindName::Planner),
                workspace.clone(),
                WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            ))
            .unwrap();
        let child_turn = TurnId::parse("trn_projection_child").unwrap();
        let child_cause = record_parent_turn(&sink, &parent, child_turn.clone());
        let child = scheduler
            .dispatch(DispatchRequest {
                task_name: "review_plan".into(),
                objective: "review the plan".into(),
                role: AgentKind::built_in(AgentKindName::Reviewer),
                requested_route: RequestedModelRoute::exact(
                    super::super::catalog_model_id("local", "qwen2.5-coder:7b"),
                    ReasoningEffort::none(),
                ),
                parent_agent_id: Some(parent.agent_id.clone()),
                causing_turn_id: Some(child_turn),
                caused_by_event: Some(child_cause.event_id),
                workspace: WorkspaceAssignment {
                    workspace_id: workspace,
                    strategy: WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
                },
            })
            .unwrap();
        scheduler
            .transition(&child, DispatchState::Starting, None)
            .unwrap();
        scheduler
            .transition(&child, DispatchState::Running, None)
            .unwrap();
        scheduler
            .transition(&child, DispatchState::WaitingForAgent, None)
            .unwrap();

        let events = sink.events(&parent.run_id).unwrap();
        let projection = AgentFollowProjection::rehydrate(&events).unwrap();
        assert_eq!(
            projection.children(&parent.agent_id),
            std::slice::from_ref(&child.agent_id)
        );
        assert_eq!(
            projection.agents()[&child.agent_id].state,
            DispatchState::WaitingForAgent
        );
        assert_eq!(
            projection.agents()[&child.agent_id]
                .causing_turn_id
                .as_ref()
                .map(TurnId::as_str),
            Some("trn_projection_child")
        );
    }
}
