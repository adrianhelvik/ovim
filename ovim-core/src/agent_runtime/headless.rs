//! Transport-neutral delegated-agent snapshots for editor and headless clients.

use super::{
    AgentApprovalProjection, AgentDispatchRecord, AgentMessageRecord, AgentMessageState,
    AgentRuntimeProjection, DispatchState, PendingAgentApproval, ResolvedAgentApproval,
    WorkspaceStrategy,
};
use crate::run_log::{
    AgentApprovalDecisionSnapshot, AgentApprovalResolutionSourceSnapshot, AgentId, AgentReported,
    AgentUsageEvent, ArtifactId, ArtifactRecord, EventEnvelope, EventId, EventKind, OperationId,
    RunId, TurnId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const AGENT_CONTROL_SNAPSHOT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentControlPlaneSnapshot {
    pub schema_version: u32,
    pub run_id: RunId,
    pub root_agent_id: AgentId,
    pub last_sequence: u64,
    pub agents: Vec<AgentSnapshot>,
    /// Human decisions that block progress, currently pending approvals.
    pub pending_attention: usize,
    /// Unconsumed model-to-model mailbox traffic, shown separately from
    /// decisions so routine handoffs never masquerade as operator blockers.
    #[serde(default)]
    pub pending_updates: usize,
}

const MAX_AGENT_TRACE_EVENTS: usize = 64;
const MAX_AGENT_TRACE_SUMMARY_CHARS: usize = 240;

fn agent_trace(events: &[EventEnvelope], agent_id: &AgentId) -> Vec<AgentTraceEventSnapshot> {
    let mut trace = events
        .iter()
        .filter(|event| event.agent_id.as_ref() == Some(agent_id))
        .filter_map(trace_event)
        .collect::<Vec<_>>();
    if trace.len() > MAX_AGENT_TRACE_EVENTS {
        trace.drain(..trace.len() - MAX_AGENT_TRACE_EVENTS);
    }

    trace
}

fn trace_event(event: &EventEnvelope) -> Option<AgentTraceEventSnapshot> {
    let (kind, summary) = match &event.kind {
        EventKind::AgentLifecycle(value) => (
            "lifecycle",
            value
                .detail
                .clone()
                .unwrap_or_else(|| format!("state {}", wire_name(&value.state))),
        ),
        EventKind::AgentProvider(value) => (
            "provider",
            format!(
                "{} · {}/{}",
                wire_name(&value.state),
                value.model,
                value.reasoning_effort
            ),
        ),
        EventKind::AgentProgress(value) => (
            "progress",
            value
                .detail
                .clone()
                .or_else(|| {
                    value
                        .current_tool
                        .as_ref()
                        .map(|tool| format!("tool {tool}"))
                })
                .unwrap_or_else(|| wire_name(&value.activity)),
        ),
        EventKind::ToolStarted(value) => ("tool_started", value.tool_name.clone()),
        EventKind::ToolResult(value) => (
            "tool_result",
            value
                .summary
                .clone()
                .map(|summary| format!("{} · {summary}", wire_name(&value.outcome)))
                .unwrap_or_else(|| wire_name(&value.outcome)),
        ),
        EventKind::AgentHandoff(value) => (
            "handoff",
            format!(
                "{} · {}",
                wire_name(&value.handoff.status()),
                value.handoff.as_handoff().summary
            ),
        ),
        EventKind::AgentApprovalRequested(value) => (
            "approval_requested",
            format!("{} · {}", value.tool_name, value.reason),
        ),
        EventKind::AgentApprovalResolved(value) => (
            "approval_resolved",
            format!("{} · {}", wire_name(&value.decision), value.reason),
        ),
        EventKind::AgentFollowup(_) => ("followup", "follow-up turn started".into()),
        EventKind::AgentMessage(_) => ("message", "steering message queued".into()),
        EventKind::AgentMessageDelivery(_) => {
            ("message_delivery", "steering delivery updated".into())
        }
        _ => return None,
    };
    Some(AgentTraceEventSnapshot {
        sequence: event.sequence,
        recorded_at: event.recorded_at.clone(),
        kind: kind.into(),
        summary: bounded_trace_summary(&summary),
        turn_id: event.turn_id.clone(),
        provider_call_id: event.provider_call_id.clone(),
    })
}

fn wire_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn bounded_trace_summary(value: &str) -> String {
    let mut chars = value.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_AGENT_TRACE_SUMMARY_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

impl AgentControlPlaneSnapshot {
    /// Return agents in stable parent-before-child order.
    ///
    /// The durable dispatch list is normally already chronological, but API
    /// clients and renderers must not rely on completion or transport order to
    /// reconstruct hierarchy. Unknown/orphaned parents remain visible after
    /// the rooted tree rather than disappearing from the control plane.
    pub fn hierarchy(&self) -> Vec<&AgentSnapshot> {
        let by_id = self
            .agents
            .iter()
            .map(|agent| (agent.agent_id.clone(), agent))
            .collect::<BTreeMap<_, _>>();
        let mut children = BTreeMap::<AgentId, Vec<AgentId>>::new();
        for agent in &self.agents {
            let parent = agent
                .parent_agent_id
                .clone()
                .unwrap_or_else(|| self.root_agent_id.clone());
            children
                .entry(parent)
                .or_default()
                .push(agent.agent_id.clone());
        }

        let mut ordered = Vec::with_capacity(self.agents.len());
        let mut visited = BTreeSet::new();
        visit_hierarchy(
            &self.root_agent_id,
            &by_id,
            &children,
            &mut visited,
            &mut ordered,
        );
        for agent in &self.agents {
            if visited.insert(agent.agent_id.clone()) {
                ordered.push(agent);
                visit_hierarchy(
                    &agent.agent_id,
                    &by_id,
                    &children,
                    &mut visited,
                    &mut ordered,
                );
            }
        }
        ordered
    }

    /// Oldest pending decision in durable projection order. Presentation and
    /// input use this same attribution rule so simultaneous approvals cannot
    /// silently retarget between the prompt and the response.
    pub fn oldest_pending_approval(&self) -> Option<(&AgentSnapshot, &AgentApprovalSnapshot)> {
        self.hierarchy()
            .into_iter()
            .flat_map(|agent| {
                agent
                    .approvals
                    .iter()
                    .filter(|approval| approval.state == "pending")
                    .map(move |approval| (agent, approval))
            })
            .min_by(|(left_agent, left), (right_agent, right)| {
                (
                    &left.created_at,
                    &left.request_event_id,
                    &left_agent.agent_id,
                )
                    .cmp(&(
                        &right.created_at,
                        &right.request_event_id,
                        &right_agent.agent_id,
                    ))
            })
    }

    /// Oldest pending decision for one specific agent, using the same durable
    /// ordering as [`Self::oldest_pending_approval`]. The agent tree resolves
    /// the child the user has highlighted rather than the globally-oldest one,
    /// so a decision cannot silently retarget to a different child.
    pub fn oldest_pending_approval_for(
        &self,
        agent_id: &str,
    ) -> Option<(&AgentSnapshot, &AgentApprovalSnapshot)> {
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == agent_id)?;
        agent
            .approvals
            .iter()
            .filter(|approval| approval.state == "pending")
            .min_by(|left, right| {
                (&left.created_at, &left.request_event_id)
                    .cmp(&(&right.created_at, &right.request_event_id))
            })
            .map(|approval| (agent, approval))
    }
}

fn visit_hierarchy<'a>(
    parent: &AgentId,
    by_id: &BTreeMap<AgentId, &'a AgentSnapshot>,
    children: &BTreeMap<AgentId, Vec<AgentId>>,
    visited: &mut BTreeSet<AgentId>,
    ordered: &mut Vec<&'a AgentSnapshot>,
) {
    let Some(child_ids) = children.get(parent) else {
        return;
    };
    for child_id in child_ids {
        if !visited.insert(child_id.clone()) {
            continue;
        }
        if let Some(child) = by_id.get(child_id) {
            ordered.push(*child);
            visit_hierarchy(child_id, by_id, children, visited, ordered);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub ancestry: Vec<AgentId>,
    pub children: Vec<AgentId>,
    pub task_name: String,
    pub role: String,
    pub objective: String,
    pub requested_route: AgentRequestedRouteSnapshot,
    pub resolved_route: AgentResolvedRouteSnapshot,
    pub lifecycle: String,
    pub turn_generation: u32,
    pub turn_id: Option<TurnId>,
    pub elapsed_millis: AgentReported<u64>,
    pub progress: AgentReported<crate::run_log::AgentProgressEvent>,
    pub usage: AgentReported<AgentUsageEvent>,
    pub workspace: AgentWorkspaceSnapshot,
    pub messages: Vec<AgentMessageSnapshot>,
    pub approvals: Vec<AgentApprovalSnapshot>,
    pub handoff: Option<AgentHandoffSnapshot>,
    pub artifact_handles: Vec<AgentArtifactHandle>,
    pub attention: AgentAttentionSnapshot,
    pub recovery_status: String,
    /// Bounded, ordered operational history. This intentionally contains
    /// auditable state/tool summaries rather than provider chain-of-thought or
    /// raw tool payloads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<AgentTraceEventSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTraceEventSnapshot {
    pub sequence: u64,
    pub recorded_at: String,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_call_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequestedRouteSnapshot {
    pub catalog_model_id: String,
    pub reasoning_effort: String,
    pub fallback_policy: String,
    pub fallback_catalog_model_id: Option<String>,
    pub fallback_reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResolvedRouteSnapshot {
    pub catalog_generation: String,
    pub catalog_model_id: String,
    pub profile_name: String,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: String,
    pub resolution: String,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceSnapshot {
    pub workspace_id: WorkspaceId,
    pub strategy: String,
    pub manifest_id: Option<String>,
    pub ownership: String,
    pub root: Option<String>,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageSnapshot {
    pub message_event_id: EventId,
    pub sender_agent_id: AgentId,
    pub recipient_agent_id: AgentId,
    pub content: String,
    pub state: String,
    pub detail: Option<String>,
    pub consumed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentApprovalSnapshot {
    pub request_event_id: EventId,
    pub operation_id: OperationId,
    pub state: String,
    pub tool_name: String,
    pub effect: String,
    pub reason: String,
    pub created_at: String,
    pub deadline_at: String,
    pub decision: Option<String>,
    pub resolution_source: Option<String>,
    pub resolution_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHandoffSnapshot {
    pub event_id: EventId,
    pub status: String,
    pub summary: String,
    pub evidence: Vec<super::HandoffEvidence>,
    pub changed_files: Vec<String>,
    pub blockers: Vec<String>,
    pub followups: Vec<String>,
    pub confidence: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttentionSnapshot {
    pub required: bool,
    pub pending_approvals: usize,
    pub pending_messages: usize,
    pub pending_notifications: usize,
}

/// Artifact metadata safe for list/detail responses. Blob bytes are returned
/// only by the separately ID-scoped artifact read operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentArtifactHandle {
    pub artifact_id: ArtifactId,
    pub state: crate::run_log::ArtifactState,
    pub media_type: Option<String>,
    pub retention: crate::run_log::ArtifactRetention,
    pub export_policy: crate::run_log::ArtifactExportPolicy,
}

pub(crate) fn build_agent_snapshot(
    run_id: RunId,
    root_agent_id: AgentId,
    records: Vec<AgentDispatchRecord>,
    events: &[EventEnvelope],
    messages: Vec<AgentMessageRecord>,
    pending_approvals: Vec<PendingAgentApproval>,
    resolved_approvals: Vec<ResolvedAgentApproval>,
    pending_notifications: usize,
) -> Result<AgentControlPlaneSnapshot, String> {
    let runtime = AgentRuntimeProjection::rehydrate(events).map_err(|error| error.to_string())?;
    // Rehydrate independently as an integrity check: the broker is live state,
    // while this proves the durable stream still reconstructs the same domain.
    AgentApprovalProjection::rehydrate(&run_id, events).map_err(|error| error.to_string())?;
    let by_id = records
        .iter()
        .map(|record| (record.handle.agent_id.clone(), record.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut children = BTreeMap::<AgentId, Vec<AgentId>>::new();
    for record in &records {
        if let Some(parent) = &record.parent_agent_id {
            children
                .entry(parent.clone())
                .or_default()
                .push(record.handle.agent_id.clone());
        }
    }
    let artifacts = artifact_handles_by_agent(events);
    let handoffs = latest_handoffs(events);
    let terminal_recovery = recovery_by_agent(events);
    let mut agents = Vec::with_capacity(records.len());
    for record in records {
        let agent_id = record.handle.agent_id.clone();
        let metrics = runtime.latest_for_agent(&agent_id);
        let progress = metrics
            .and_then(|metrics| metrics.progress.clone())
            .map_or(AgentReported::NotReported, AgentReported::Reported);
        let elapsed_millis = metrics
            .and_then(|metrics| metrics.progress.as_ref())
            .map(|progress| AgentReported::Reported(progress.elapsed_millis))
            .unwrap_or(AgentReported::NotReported);
        let usage = metrics
            .and_then(|metrics| metrics.usage.clone())
            .map_or(AgentReported::NotReported, AgentReported::Reported);
        let agent_messages = messages
            .iter()
            .filter(|message| {
                message.sender_agent_id == agent_id || message.recipient_agent_id == agent_id
            })
            .map(message_snapshot)
            .collect::<Vec<_>>();
        let approvals = pending_approvals
            .iter()
            .filter(|approval| approval.key.agent_id == agent_id)
            .map(pending_approval_snapshot)
            .chain(
                resolved_approvals
                    .iter()
                    .filter(|approval| approval.pending.key.agent_id == agent_id)
                    .map(resolved_approval_snapshot),
            )
            .collect::<Vec<_>>();
        let pending_message_count = agent_messages
            .iter()
            .filter(|message| matches!(message.state.as_str(), "queued" | "delivering"))
            .count();
        let pending_approval_count = approvals
            .iter()
            .filter(|approval| approval.state == "pending")
            .count();
        agents.push(AgentSnapshot {
            ancestry: ancestry(&record, &by_id, &root_agent_id)?,
            children: children.remove(&agent_id).unwrap_or_default(),
            parent_agent_id: record.parent_agent_id.clone(),
            task_name: record.task_name.clone(),
            role: record.role.name.to_string(),
            objective: record.objective.clone(),
            requested_route: requested_route(&record),
            resolved_route: resolved_route(&record),
            lifecycle: state_name(&record.state).into(),
            turn_generation: record.turn_generation,
            turn_id: metrics
                .and_then(|metrics| metrics.key.turn_id.clone())
                .or_else(|| {
                    record
                        .followup
                        .as_ref()
                        .map(|followup| followup.followup_turn_id.clone())
                        .or_else(|| record.causing_turn_id.clone())
                }),
            elapsed_millis,
            progress,
            usage,
            workspace: workspace_snapshot(&record),
            messages: agent_messages,
            approvals,
            handoff: handoffs.get(&agent_id).cloned(),
            artifact_handles: artifacts.get(&agent_id).cloned().unwrap_or_default(),
            attention: AgentAttentionSnapshot {
                required: pending_approval_count > 0,
                pending_approvals: pending_approval_count,
                pending_messages: pending_message_count,
                pending_notifications: 0,
            },
            recovery_status: terminal_recovery
                .get(&agent_id)
                .cloned()
                .unwrap_or_else(|| "none".into()),
            trace: agent_trace(events, &agent_id),
            agent_id,
        });
    }
    let pending_attention = agents
        .iter()
        .map(|agent| agent.attention.pending_approvals)
        .sum();
    Ok(AgentControlPlaneSnapshot {
        schema_version: AGENT_CONTROL_SNAPSHOT_VERSION,
        run_id,
        root_agent_id,
        last_sequence: events.last().map_or(0, |event| event.sequence),
        agents,
        pending_attention,
        pending_updates: pending_notifications,
    })
}

fn ancestry(
    record: &AgentDispatchRecord,
    records: &BTreeMap<AgentId, AgentDispatchRecord>,
    root: &AgentId,
) -> Result<Vec<AgentId>, String> {
    let mut result = Vec::new();
    let mut next = record.parent_agent_id.as_ref();
    let mut seen = BTreeSet::new();
    while let Some(parent) = next {
        if !seen.insert(parent.clone()) {
            return Err(format!("agent ancestry contains a cycle at {parent}"));
        }
        result.push(parent.clone());
        if parent == root {
            break;
        }
        next = records
            .get(parent)
            .and_then(|record| record.parent_agent_id.as_ref());
    }
    result.reverse();
    Ok(result)
}

fn requested_route(record: &AgentDispatchRecord) -> AgentRequestedRouteSnapshot {
    let (fallback_policy, fallback_catalog_model_id, fallback_reasoning_effort) =
        match &record.requested_route.fallback_policy {
            super::ModelFallbackPolicy::FailClosed => ("fail_closed".into(), None, None),
            super::ModelFallbackPolicy::Explicit {
                catalog_model_id,
                reasoning_effort,
            } => (
                "explicit".into(),
                Some(catalog_model_id.clone()),
                Some(reasoning_effort.as_str().into()),
            ),
        };
    AgentRequestedRouteSnapshot {
        catalog_model_id: record.requested_route.catalog_model_id.clone(),
        reasoning_effort: record.requested_route.reasoning_effort.as_str().into(),
        fallback_policy,
        fallback_catalog_model_id,
        fallback_reasoning_effort,
    }
}

fn resolved_route(record: &AgentDispatchRecord) -> AgentResolvedRouteSnapshot {
    AgentResolvedRouteSnapshot {
        catalog_generation: record.resolved_route.catalog_generation.clone(),
        catalog_model_id: record.resolved_route.catalog_model_id.clone(),
        profile_name: record.resolved_route.profile_name.clone(),
        provider: record.resolved_route.provider.clone(),
        model: record.resolved_route.model.clone(),
        reasoning_effort: record.resolved_route.reasoning_effort.as_str().into(),
        resolution: match record.resolved_route.resolution {
            super::ModelRouteResolution::Exact => "exact",
            super::ModelRouteResolution::ConfiguredFallback => "configured_fallback",
            super::ModelRouteResolution::HistoricV1 => "historic_v1",
        }
        .into(),
        fallback_reason: record.resolved_route.fallback_reason.clone(),
    }
}

fn workspace_snapshot(record: &AgentDispatchRecord) -> AgentWorkspaceSnapshot {
    let (strategy, manifest_id, read_only) = match &record.handle.workspace.strategy {
        WorkspaceStrategy::SharedWorkspace => ("shared_workspace", None, false),
        WorkspaceStrategy::IsolatedWorktree { base_manifest_id } => (
            "isolated_worktree",
            base_manifest_id.as_ref().map(ToString::to_string),
            false,
        ),
        WorkspaceStrategy::ReadOnlySnapshot { manifest_id } => (
            "read_only_snapshot",
            manifest_id.as_ref().map(ToString::to_string),
            true,
        ),
    };
    AgentWorkspaceSnapshot {
        workspace_id: record.handle.workspace.workspace_id.clone(),
        strategy: strategy.into(),
        manifest_id,
        ownership: "ovim".into(),
        root: None,
        read_only,
    }
}

fn message_snapshot(message: &AgentMessageRecord) -> AgentMessageSnapshot {
    let (state, detail) = match &message.state {
        AgentMessageState::Queued => ("queued", None),
        AgentMessageState::Delivering { .. } => ("delivering", None),
        AgentMessageState::Delivered { .. } => ("delivered", None),
        AgentMessageState::Rejected { detail, .. } => ("rejected", Some(detail.clone())),
    };
    AgentMessageSnapshot {
        message_event_id: message.message_event_id.clone(),
        sender_agent_id: message.sender_agent_id.clone(),
        recipient_agent_id: message.recipient_agent_id.clone(),
        content: message.content.clone(),
        state: state.into(),
        detail,
        consumed: message.consumption_event_id.is_some(),
    }
}

fn pending_approval_snapshot(approval: &PendingAgentApproval) -> AgentApprovalSnapshot {
    AgentApprovalSnapshot {
        request_event_id: approval.request_event_id.clone(),
        operation_id: approval.key.operation_id.clone(),
        state: "pending".into(),
        tool_name: approval.request.tool_name.clone(),
        effect: format!("{:?}", approval.request.normalized_effect).to_lowercase(),
        reason: approval.request.reason.clone(),
        created_at: approval.request.created_at.clone(),
        deadline_at: approval.request.deadline_at.clone(),
        decision: None,
        resolution_source: None,
        resolution_reason: None,
    }
}

fn resolved_approval_snapshot(approval: &ResolvedAgentApproval) -> AgentApprovalSnapshot {
    let mut snapshot = pending_approval_snapshot(&approval.pending);
    snapshot.state = "resolved".into();
    snapshot.decision = Some(
        match approval.resolution.decision {
            AgentApprovalDecisionSnapshot::Allowed => "allowed",
            AgentApprovalDecisionSnapshot::Denied => "denied",
        }
        .into(),
    );
    snapshot.resolution_source = Some(
        match approval.resolution.source {
            AgentApprovalResolutionSourceSnapshot::User => "user",
            AgentApprovalResolutionSourceSnapshot::Policy => "policy",
            AgentApprovalResolutionSourceSnapshot::Timeout => "timeout",
            AgentApprovalResolutionSourceSnapshot::Cancellation => "cancellation",
            AgentApprovalResolutionSourceSnapshot::Restart => "restart",
        }
        .into(),
    );
    snapshot.resolution_reason = Some(approval.resolution.reason.clone());
    snapshot
}

fn latest_handoffs(events: &[EventEnvelope]) -> BTreeMap<AgentId, AgentHandoffSnapshot> {
    events
        .iter()
        .filter_map(|event| {
            let EventKind::AgentHandoff(recorded) = &event.kind else {
                return None;
            };
            let agent_id = event.agent_id.clone()?;
            let handoff = recorded.handoff.as_handoff();
            Some((
                agent_id,
                AgentHandoffSnapshot {
                    event_id: event.event_id.clone(),
                    status: format!("{:?}", handoff.status).to_lowercase(),
                    summary: handoff.summary.clone(),
                    evidence: handoff.evidence.clone(),
                    changed_files: handoff.changed_files.clone(),
                    blockers: handoff.blockers.clone(),
                    followups: handoff.followups.clone(),
                    confidence: format!("{:?}", handoff.confidence).to_lowercase(),
                },
            ))
        })
        .collect()
}

fn artifact_handles_by_agent(
    events: &[EventEnvelope],
) -> BTreeMap<AgentId, Vec<AgentArtifactHandle>> {
    let mut result = BTreeMap::<AgentId, BTreeMap<ArtifactId, AgentArtifactHandle>>::new();
    for event in events {
        let Some(agent_id) = &event.agent_id else {
            continue;
        };
        let EventKind::FileMutation(mutation) = &event.kind else {
            continue;
        };
        for artifact in &mutation.artifacts {
            result
                .entry(agent_id.clone())
                .or_default()
                .insert(artifact.artifact_id.clone(), artifact_handle(artifact));
        }
    }
    result
        .into_iter()
        .map(|(agent, artifacts)| (agent, artifacts.into_values().collect()))
        .collect()
}

fn artifact_handle(artifact: &ArtifactRecord) -> AgentArtifactHandle {
    AgentArtifactHandle {
        artifact_id: artifact.artifact_id.clone(),
        state: artifact.state.clone(),
        media_type: artifact.media_type.clone(),
        retention: artifact.retention.clone(),
        export_policy: artifact.export_policy.clone(),
    }
}

fn recovery_by_agent(events: &[EventEnvelope]) -> BTreeMap<AgentId, String> {
    events
        .iter()
        .filter_map(|event| {
            let EventKind::AgentLifecycle(lifecycle) = &event.kind else {
                return None;
            };
            let detail = lifecycle.detail.as_deref()?.to_ascii_lowercase();
            let status = if detail.contains("restart")
                || detail.contains("recovery")
                || detail.contains("process stopped")
            {
                "interrupted_after_restart"
            } else {
                return None;
            };
            Some((lifecycle.agent_id.clone(), status.into()))
        })
        .collect()
}

fn state_name(state: &DispatchState) -> &'static str {
    match state {
        DispatchState::Created => "created",
        DispatchState::Queued => "queued",
        DispatchState::Starting => "starting",
        DispatchState::Running => "running",
        DispatchState::WaitingForAgent => "waiting_for_agent",
        DispatchState::WaitingForTool => "waiting_for_tool",
        DispatchState::WaitingForUser => "waiting_for_user",
        DispatchState::Completed => "completed",
        DispatchState::Interrupted => "interrupted",
        DispatchState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::{
        EventActor, ToolStartedEvent, EVENT_PAYLOAD_VERSION, EVENT_SCHEMA_VERSION,
    };

    fn tool_event(agent_id: &AgentId, sequence: u64, tool_name: &str) -> EventEnvelope {
        EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            payload_version: EVENT_PAYLOAD_VERSION,
            event_id: EventId::new(),
            run_id: RunId::new(),
            sequence,
            recorded_at: format!("2026-08-08T00:00:{sequence:02}Z"),
            caused_by: None,
            operation_id: None,
            provider_call_id: Some(format!("call_{sequence}")),
            actor: EventActor::Agent(agent_id.clone()),
            agent_id: Some(agent_id.clone()),
            turn_id: None,
            workspace_id: None,
            branch_id: None,
            kind: EventKind::ToolStarted(ToolStartedEvent {
                tool_name: tool_name.into(),
            }),
        }
    }

    fn pending_approval(created_at: &str) -> AgentApprovalSnapshot {
        AgentApprovalSnapshot {
            request_event_id: EventId::new(),
            operation_id: OperationId::new(),
            state: "pending".into(),
            tool_name: "read".into(),
            effect: "read".into(),
            reason: "test".into(),
            created_at: created_at.into(),
            deadline_at: "2026-12-31T00:00:00Z".into(),
            decision: None,
            resolution_source: None,
            resolution_reason: None,
        }
    }

    fn child(parent: &AgentId, approvals: Vec<AgentApprovalSnapshot>) -> AgentSnapshot {
        AgentSnapshot {
            agent_id: AgentId::new(),
            parent_agent_id: Some(parent.clone()),
            ancestry: vec![parent.clone()],
            children: Vec::new(),
            task_name: "task".into(),
            role: "explorer".into(),
            objective: "obj".into(),
            requested_route: AgentRequestedRouteSnapshot {
                catalog_model_id: "m".into(),
                reasoning_effort: "low".into(),
                fallback_policy: "none".into(),
                fallback_catalog_model_id: None,
                fallback_reasoning_effort: None,
            },
            resolved_route: AgentResolvedRouteSnapshot {
                catalog_generation: "g".into(),
                catalog_model_id: "m".into(),
                profile_name: "p".into(),
                provider: "codex".into(),
                model: "m".into(),
                reasoning_effort: "low".into(),
                resolution: "exact".into(),
                fallback_reason: None,
            },
            lifecycle: "running".into(),
            turn_generation: 0,
            turn_id: None,
            elapsed_millis: AgentReported::NotReported,
            progress: AgentReported::NotReported,
            usage: AgentReported::NotReported,
            workspace: AgentWorkspaceSnapshot {
                workspace_id: WorkspaceId::new(),
                strategy: "snapshot".into(),
                manifest_id: None,
                ownership: "owned".into(),
                root: None,
                read_only: true,
            },
            messages: Vec::new(),
            approvals,
            handoff: None,
            artifact_handles: Vec::new(),
            attention: AgentAttentionSnapshot::default(),
            recovery_status: "none".into(),
            trace: Vec::new(),
        }
    }

    fn control_plane(root: AgentId, agents: Vec<AgentSnapshot>) -> AgentControlPlaneSnapshot {
        AgentControlPlaneSnapshot {
            schema_version: AGENT_CONTROL_SNAPSHOT_VERSION,
            run_id: RunId::new(),
            root_agent_id: root,
            last_sequence: 0,
            agents,
            pending_attention: 0,
            pending_updates: 0,
        }
    }

    #[test]
    fn operational_trace_is_ordered_bounded_and_omits_raw_payloads() {
        let agent_id = AgentId::new();
        let events = (0..70)
            .map(|sequence| tool_event(&agent_id, sequence, &format!("tool_{sequence}")))
            .collect::<Vec<_>>();

        let trace = agent_trace(&events, &agent_id);

        assert_eq!(trace.len(), MAX_AGENT_TRACE_EVENTS);
        assert_eq!(trace.first().unwrap().sequence, 6);
        assert_eq!(trace.last().unwrap().sequence, 69);
        assert_eq!(trace.last().unwrap().kind, "tool_started");
        assert_eq!(trace.last().unwrap().summary, "tool_69");
        assert_eq!(
            trace.last().unwrap().provider_call_id.as_deref(),
            Some("call_69")
        );
    }

    #[test]
    fn targeted_child_resolves_its_own_approval_not_the_globally_oldest() {
        let root = AgentId::new();
        let older = child(&root, vec![pending_approval("2026-01-01T00:00:00Z")]);
        let selected = child(&root, vec![pending_approval("2026-06-01T00:00:00Z")]);
        let older_id = older.agent_id.clone();
        let selected_id = selected.agent_id.clone();
        let snapshot = control_plane(root, vec![older, selected]);

        // The globally-oldest approval belongs to the first child...
        assert_eq!(
            snapshot.oldest_pending_approval().unwrap().0.agent_id,
            older_id
        );
        // ...but targeting the highlighted child returns that child's approval,
        // so `a`/`d` in the tree never silently retarget to a different child.
        let (agent, _) = snapshot
            .oldest_pending_approval_for(selected_id.as_str())
            .expect("selected child has a pending approval");
        assert_eq!(agent.agent_id, selected_id);
    }

    #[test]
    fn targeted_child_without_a_pending_approval_returns_none() {
        let root = AgentId::new();
        let quiet = child(&root, Vec::new());
        let quiet_id = quiet.agent_id.clone();
        let busy = child(&root, vec![pending_approval("2026-01-01T00:00:00Z")]);
        let snapshot = control_plane(root, vec![quiet, busy]);

        // No result for the quiet child; the caller then falls back to oldest.
        assert!(snapshot
            .oldest_pending_approval_for(quiet_id.as_str())
            .is_none());
        assert!(snapshot.oldest_pending_approval().is_some());
    }
}
