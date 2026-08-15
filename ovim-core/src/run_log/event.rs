use super::{
    AgentId, BaseManifestId, BranchId, EventId, ManifestId, OperationId, RepositoryId, RunId,
    TurnId, WorkspaceId,
};
use crate::agent_runtime::{AgentWorkspaceWarning, ParentHandoffProjection, ValidatedHandoff};
use serde::{
    de::{self, DeserializeOwned},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;

pub const EVENT_SCHEMA_VERSION: u32 = 1;
pub const EVENT_PAYLOAD_VERSION: u32 = 1;

/// The principal responsible for an event. Provider sessions are deliberately
/// absent: they are replaceable adapter metadata, not ovim identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum EventActor {
    User,
    Agent(AgentId),
    System(String),
}

pub const AGENT_HANDOFF_VALIDATION_FAILED_EVENT_VERSION: u32 = 1;
pub const AGENT_ATTACHMENT_EVENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAttachmentEvent {
    pub version: u32,
    pub name: String,
    pub artifact: super::ArtifactRecord,
}

/// A provider completed its investigation but returned a result envelope that
/// failed validation. The bounded original payload is retained so validation
/// cannot destroy useful work and a caller can repair or inspect it later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHandoffValidationFailedEvent {
    pub version: u32,
    pub error: String,
    pub raw_payload: Vec<u8>,
    pub repair_attempted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub payload_version: u32,
    pub event_id: EventId,
    pub run_id: RunId,
    pub sequence: u64,
    /// RFC 3339 UTC timestamp. Sequence, rather than wall time, defines order.
    pub recorded_at: String,
    pub caused_by: Option<EventId>,
    pub operation_id: Option<OperationId>,
    /// Opaque provider/tool call identifier, never used as ovim identity.
    pub provider_call_id: Option<String>,
    pub actor: EventActor,
    pub agent_id: Option<AgentId>,
    pub turn_id: Option<TurnId>,
    pub workspace_id: Option<WorkspaceId>,
    /// Durable conversation trajectory. Editor tree/node identifiers remain a
    /// transient projection and must not be persisted here.
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    pub kind: EventKind,
}

/// An event before store-owned ordering and recording metadata is assigned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewRunEvent {
    pub run_id: RunId,
    pub caused_by: Option<EventId>,
    pub operation_id: Option<OperationId>,
    pub provider_call_id: Option<String>,
    pub actor: EventActor,
    pub agent_id: Option<AgentId>,
    pub turn_id: Option<TurnId>,
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    pub kind: EventKind,
}

impl NewRunEvent {
    pub fn new(run_id: RunId, actor: EventActor, kind: EventKind) -> Self {
        Self {
            run_id,
            caused_by: None,
            operation_id: None,
            provider_call_id: None,
            actor,
            agent_id: None,
            turn_id: None,
            workspace_id: None,
            branch_id: None,
            kind,
        }
    }

    pub fn in_turn(mut self, turn_id: TurnId) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    pub fn caused_by(mut self, event_id: EventId) -> Self {
        self.caused_by = Some(event_id);
        self
    }

    pub fn for_provider_call(mut self, provider_call_id: impl Into<String>) -> Self {
        self.provider_call_id = Some(provider_call_id.into());
        self
    }

    pub fn for_operation(mut self, operation_id: OperationId) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    pub fn for_agent(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    pub fn in_workspace(mut self, workspace_id: WorkspaceId) -> Self {
        self.workspace_id = Some(workspace_id);
        self
    }

    pub fn in_branch(mut self, branch_id: BranchId) -> Self {
        self.branch_id = Some(branch_id);
        self
    }
}

/// Version-one normalized event payload. New semantic shapes should normally be
/// added as variants; incompatible changes require a new payload version.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EventKind {
    RunLifecycle(RunLifecycleEvent),
    BranchLifecycle(BranchLifecycleEvent),
    AgentLifecycle(AgentLifecycleEvent),
    AgentProvider(AgentProviderEvent),
    AgentUsage(AgentUsageEvent),
    AgentProgress(AgentProgressEvent),
    AgentHandoff(AgentHandoffEvent),
    AgentHandoffValidationFailed(AgentHandoffValidationFailedEvent),
    AgentAttachment(AgentAttachmentEvent),
    AgentFollowup(AgentFollowupEvent),
    AgentApprovalRequested(AgentApprovalRequestedEvent),
    AgentApprovalResolved(AgentApprovalResolvedEvent),
    AgentMessage(AgentMessageEvent),
    AgentMessageDelivery(AgentMessageDeliveryEvent),
    MailboxNotification(MailboxNotificationEvent),
    MailboxConsumed(MailboxConsumedEvent),
    TurnLifecycle(TurnLifecycleEvent),
    Message(MessageEvent),
    ToolIntent(ToolIntentEvent),
    ToolDecision(ToolDecisionEvent),
    ToolStarted(ToolStartedEvent),
    ToolResult(ToolResultEvent),
    FileMutation(FileMutationEvent),
    Checkpoint(CheckpointEvent),
    Divergence(DivergenceEvent),
    /// A future or extension event retained without semantic interpretation.
    Unknown {
        name: String,
        payload: Value,
    },
}

#[derive(Serialize, Deserialize)]
struct RawEventKind {
    kind: String,
    #[serde(default)]
    data: Value,
}

impl Serialize for EventKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (kind, data) = match self {
            Self::RunLifecycle(value) => ("run_lifecycle", serde_json::to_value(value)),
            Self::BranchLifecycle(value) => ("branch_lifecycle", serde_json::to_value(value)),
            Self::AgentLifecycle(value) => ("agent_lifecycle", serde_json::to_value(value)),
            Self::AgentProvider(value) => ("agent_provider", serde_json::to_value(value)),
            Self::AgentUsage(value) => ("agent_usage", serde_json::to_value(value)),
            Self::AgentProgress(value) => ("agent_progress", serde_json::to_value(value)),
            Self::AgentHandoff(value) => ("agent_handoff", serde_json::to_value(value)),
            Self::AgentHandoffValidationFailed(value) => (
                "agent_handoff_validation_failed",
                serde_json::to_value(value),
            ),
            Self::AgentAttachment(value) => ("agent_attachment", serde_json::to_value(value)),
            Self::AgentFollowup(value) => ("agent_followup", serde_json::to_value(value)),
            Self::AgentApprovalRequested(value) => {
                ("agent_approval_requested", serde_json::to_value(value))
            }
            Self::AgentApprovalResolved(value) => {
                ("agent_approval_resolved", serde_json::to_value(value))
            }
            Self::AgentMessage(value) => ("agent_message", serde_json::to_value(value)),
            Self::AgentMessageDelivery(value) => {
                ("agent_message_delivery", serde_json::to_value(value))
            }
            Self::MailboxNotification(value) => {
                ("mailbox_notification", serde_json::to_value(value))
            }
            Self::MailboxConsumed(value) => ("mailbox_consumed", serde_json::to_value(value)),
            Self::TurnLifecycle(value) => ("turn_lifecycle", serde_json::to_value(value)),
            Self::Message(value) => ("message", serde_json::to_value(value)),
            Self::ToolIntent(value) => ("tool_intent", serde_json::to_value(value)),
            Self::ToolDecision(value) => ("tool_decision", serde_json::to_value(value)),
            Self::ToolStarted(value) => ("tool_started", serde_json::to_value(value)),
            Self::ToolResult(value) => ("tool_result", serde_json::to_value(value)),
            Self::FileMutation(value) => ("file_mutation", serde_json::to_value(value)),
            Self::Checkpoint(value) => ("checkpoint", serde_json::to_value(value)),
            Self::Divergence(value) => ("divergence", serde_json::to_value(value)),
            Self::Unknown {
                name,
                payload: data,
            } => {
                return RawEventKind {
                    kind: name.clone(),
                    data: data.clone(),
                }
                .serialize(serializer)
            }
        };
        RawEventKind {
            kind: kind.into(),
            data: data.map_err(serde::ser::Error::custom)?,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawEventKind::deserialize(deserializer)?;
        match raw.kind.as_str() {
            "run_lifecycle" => decode(raw.data).map(Self::RunLifecycle),
            "branch_lifecycle" => decode(raw.data).map(Self::BranchLifecycle),
            "agent_lifecycle" => decode(raw.data).map(Self::AgentLifecycle),
            "agent_provider" => decode(raw.data).map(Self::AgentProvider),
            "agent_usage" => decode(raw.data).map(Self::AgentUsage),
            "agent_progress" => decode(raw.data).map(Self::AgentProgress),
            "agent_handoff" => decode(raw.data).map(Self::AgentHandoff),
            "agent_handoff_validation_failed" => {
                decode(raw.data).map(Self::AgentHandoffValidationFailed)
            }
            "agent_attachment" => decode(raw.data).map(Self::AgentAttachment),
            "agent_followup" => decode(raw.data).map(Self::AgentFollowup),
            "agent_approval_requested" => decode(raw.data).map(Self::AgentApprovalRequested),
            "agent_approval_resolved" => decode(raw.data).map(Self::AgentApprovalResolved),
            "agent_message" => decode(raw.data).map(Self::AgentMessage),
            "agent_message_delivery" => decode(raw.data).map(Self::AgentMessageDelivery),
            "mailbox_notification" => decode(raw.data).map(Self::MailboxNotification),
            "mailbox_consumed" => decode(raw.data).map(Self::MailboxConsumed),
            "turn_lifecycle" => decode(raw.data).map(Self::TurnLifecycle),
            "message" => decode(raw.data).map(Self::Message),
            "tool_intent" => decode(raw.data).map(Self::ToolIntent),
            "tool_decision" => decode(raw.data).map(Self::ToolDecision),
            "tool_started" => decode(raw.data).map(Self::ToolStarted),
            "tool_result" => decode(raw.data).map(Self::ToolResult),
            "file_mutation" => decode(raw.data).map(Self::FileMutation),
            "checkpoint" => decode(raw.data).map(Self::Checkpoint),
            "divergence" => decode(raw.data).map(Self::Divergence),
            _ => Ok(Self::Unknown {
                name: raw.kind,
                payload: raw.data,
            }),
        }
    }
}

fn decode<T, E>(value: Value) -> Result<T, E>
where
    T: DeserializeOwned,
    E: de::Error,
{
    serde_json::from_value(value).map_err(E::custom)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLifecycleEvent {
    pub state: RunLifecycleState,
    pub objective: Option<String>,
    pub detail: Option<String>,
    /// Present on `Created`. Optional/defaulted so historical envelopes remain
    /// readable and non-creation lifecycle records stay compact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation: Option<RunCreationMetadata>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCreationMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_manifest_id: Option<BaseManifestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_branch_id: Option<BranchId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchLifecycleEvent {
    pub state: BranchLifecycleState,
    pub branch_id: BranchId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_branch_id: Option<BranchId>,
    /// Exact event in the parent trajectory from which this branch diverged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_at: Option<EventId>,
    /// Human-facing locator at the time of the event; never used as identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchLifecycleState {
    Created,
    Forked,
    Selected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLifecycleState {
    Created,
    ObjectiveUpdated,
    Completed,
    Interrupted,
    Failed,
    Abandoned,
    Recovered,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLifecycleEvent {
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub state: AgentLifecycleState,
    pub kind: String,
    pub objective: Option<String>,
    pub detail: Option<String>,
    /// Resolved provider-independent configuration, recorded once on Created.
    /// Optional/defaulted for histories written before dispatch snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_spec: Option<Box<AgentDispatchSpecSnapshot>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDispatchSpecSnapshot {
    pub version: u32,
    /// Parent-assigned stable task name. Optional only for histories written
    /// before delegated tasks became addressable by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// Version-one role-owned route. New dispatches must leave this absent and
    /// persist both requested and resolved version-two routes instead.
    #[serde(default, rename = "model", skip_serializing_if = "Option::is_none")]
    pub legacy_model: Option<AgentModelProfileSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_route: Option<AgentRequestedModelRouteSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_route: Option<AgentResolvedModelRouteSnapshot>,
    pub instructions: String,
    pub capabilities: Vec<AgentCapabilitySnapshot>,
    pub kind_workspace_policy: AgentWorkspacePolicySnapshot,
    pub assigned_workspace: AgentWorkspaceStrategySnapshot,
    pub completion_contract: AgentCompletionContractSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelProfileSnapshot {
    pub model: String,
    pub effort: AgentModelEffortSnapshot,
    pub fallback_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequestedModelRouteSnapshot {
    pub catalog_model_id: String,
    pub reasoning_effort: String,
    pub fallback_policy: AgentModelFallbackPolicySnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentModelFallbackPolicySnapshot {
    FailClosed,
    Explicit {
        catalog_model_id: String,
        reasoning_effort: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResolvedModelRouteSnapshot {
    pub catalog_generation: String,
    pub catalog_model_id: String,
    pub profile_name: String,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: String,
    pub resolution: AgentModelRouteResolutionSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelRouteResolutionSnapshot {
    Exact,
    ConfiguredFallback,
    HistoricV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelEffortSnapshot {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapabilitySnapshot {
    Read,
    Navigate,
    SafeShell,
    Shell,
    WorkspaceWrite,
    ExternalEffects,
    DispatchAgents,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkspacePolicySnapshot {
    SharedWorkspace,
    IsolatedWorktree,
    ReadOnlyProjection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentWorkspaceStrategySnapshot {
    SharedWorkspace,
    IsolatedWorktree {
        base_manifest_id: Option<ManifestId>,
    },
    ReadOnlySnapshot {
        manifest_id: Option<ManifestId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AgentCompletionContractSnapshot {
    StructuredHandoff,
    ReviewReport,
    SafetyVerdict,
    Plan,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleState {
    Created,
    /// Legacy dispatch state retained for existing run histories.
    Dispatched,
    /// Legacy running state retained for existing run histories.
    Started,
    Queued,
    Starting,
    Running,
    WaitingForAgent,
    WaitingForTool,
    WaitingForUser,
    /// Legacy undifferentiated waiting state.
    Waiting,
    Blocked,
    Completed,
    Interrupted,
    Failed,
}

pub const AGENT_PROVIDER_EVENT_VERSION: u32 = 1;

/// Provider-owned session metadata attached to an ovim agent identity.
///
/// Session and call identifiers remain opaque adapter data. They are useful
/// for audit and conservative recovery, but never replace `AgentId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProviderEvent {
    pub version: u32,
    pub state: AgentProviderState,
    pub provider: String,
    pub profile: String,
    pub model: String,
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderState {
    Bound,
    CallStarted,
    Checkpoint,
}

pub const AGENT_USAGE_EVENT_VERSION: u32 = 1;

/// Whether a provider supplied a metric. `NotReported` is deliberately
/// different from a reported zero: Ovim never estimates missing token or cost
/// data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum AgentReported<T> {
    Reported(T),
    #[default]
    NotReported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUsageCost {
    /// ISO 4217 currency code. Providers currently report USD when they
    /// report cost at all.
    pub currency: String,
    /// Integer millionths of the currency unit, avoiding floating-point
    /// ambiguity in the durable log.
    pub amount_micros: u64,
}

/// Cumulative normalized usage for one stable agent generation/turn.
///
/// Provider and tool call counts are harness-observed. Token and cost fields
/// are reported only when the provider transport supplies them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUsageEvent {
    pub version: u32,
    pub turn_generation: u32,
    pub provider_calls: u64,
    pub tool_calls: u64,
    pub input_tokens: AgentReported<u64>,
    pub output_tokens: AgentReported<u64>,
    pub cached_input_tokens: AgentReported<u64>,
    pub cost: AgentReported<AgentUsageCost>,
}

pub const AGENT_PROGRESS_EVENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProgressActivity {
    StartingProvider,
    ProviderBound,
    ProviderCall,
    Reasoning,
    Responding,
    WaitingForTool,
    WaitingForApproval,
    FinalizingHandoff,
    Other,
}

/// Latest known activity for one stable agent generation/turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgressEvent {
    pub version: u32,
    pub turn_generation: u32,
    pub activity: AgentProgressActivity,
    pub elapsed_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A complete validated handoff retained inline in the durable event stream.
/// Its strict bounds keep the log self-contained without making mailbox
/// projections carry the full provider result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHandoffEvent {
    pub handoff: ValidatedHandoff,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_warnings: Vec<AgentWorkspaceWarning>,
}

pub const AGENT_FOLLOWUP_EVENT_VERSION: u32 = 1;

/// Durable authorization for reopening one terminal child as a fresh turn.
/// The child `AgentId`, workspace, and route remain stable; the turn,
/// generation, parent cause, budget, and eventual handoff are new identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFollowupEvent {
    pub version: u32,
    pub agent_id: AgentId,
    pub turn_generation: u32,
    pub followup_turn_id: TurnId,
    pub parent_agent_id: AgentId,
    pub parent_turn_id: TurnId,
    pub parent_event_id: EventId,
    pub prior_terminal_event_id: EventId,
    pub prior_handoff_event_id: EventId,
    pub objective: String,
    pub effective_capabilities: Vec<AgentCapabilitySnapshot>,
    pub budget: AgentFollowupBudgetSnapshot,
    pub retained_session_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFollowupBudgetSnapshot {
    pub timeout_millis: u64,
    pub max_provider_events: usize,
    pub max_tool_calls: usize,
}

pub const AGENT_APPROVAL_EVENT_VERSION: u32 = 1;

/// Complete, attributed user-attention request for one child tool operation.
///
/// Tool arguments are deliberately absent: the normalized effect and durable
/// tool intent are sufficient for approval routing without copying possibly
/// sensitive payloads into a second event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentApprovalRequestedEvent {
    pub version: u32,
    pub task_name: String,
    /// Root-to-parent chain. The requesting child remains in the envelope's
    /// `agent_id`, so it cannot be confused with one of its ancestors.
    pub ancestry: Vec<AgentId>,
    pub role: String,
    pub model: String,
    pub reasoning_effort: String,
    pub tool_name: String,
    pub normalized_effect: ToolSideEffect,
    pub required_capability: AgentCapabilitySnapshot,
    pub effective_capabilities: Vec<AgentCapabilitySnapshot>,
    pub workspace: AgentApprovalWorkspaceSnapshot,
    pub reason: String,
    /// RFC 3339 UTC timestamp. Event sequence remains the ordering authority.
    pub created_at: String,
    /// RFC 3339 UTC timestamp after which approval must fail closed.
    pub deadline_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentApprovalWorkspaceSnapshot {
    pub workspace_id: WorkspaceId,
    pub strategy: AgentWorkspaceStrategySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentApprovalResolvedEvent {
    pub version: u32,
    pub request_event_id: EventId,
    pub decision: AgentApprovalDecisionSnapshot,
    pub source: AgentApprovalResolutionSourceSnapshot,
    pub decided_by: String,
    pub reason: String,
    /// RFC 3339 UTC timestamp. Event sequence remains the ordering authority.
    pub resolved_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentApprovalDecisionSnapshot {
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentApprovalResolutionSourceSnapshot {
    User,
    Policy,
    Timeout,
    Cancellation,
    Restart,
}

pub const AGENT_MESSAGE_EVENT_VERSION: u32 = 1;

/// One bounded parent-authored message addressed to a live delegated child.
///
/// The envelope event ID is the stable message ID. Delivery is recorded in
/// separate events so a process stop can distinguish never-attempted work from
/// an ambiguous provider boundary without replaying the message blindly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageEvent {
    pub version: u32,
    pub sender_agent_id: AgentId,
    pub recipient_agent_id: AgentId,
    pub parent_event_id: EventId,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageDeliveryEvent {
    pub version: u32,
    pub message_event_id: EventId,
    pub state: AgentMessageDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDeliveryState {
    Started,
    Delivered,
    Rejected,
}

pub const MAILBOX_EVENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxNotificationEvent {
    pub version: u32,
    pub recipient_agent_id: AgentId,
    pub notification: MailboxNotificationKind,
}

/// Version-one notification vocabulary. Messages and user steering are
/// represented now so the mailbox transport does not need a semantic redesign
/// when their producer APIs arrive in a later phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MailboxNotificationKind {
    Handoff {
        source_agent_id: AgentId,
        terminal_event_id: EventId,
        handoff_event_id: EventId,
        handoff: Box<ParentHandoffProjection>,
    },
    Message {
        sender_agent_id: Option<AgentId>,
        message_event_id: EventId,
    },
    Steering {
        turn_id: TurnId,
        message_event_id: EventId,
    },
    Attention {
        source_agent_id: AgentId,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxConsumedEvent {
    pub version: u32,
    pub recipient_agent_id: AgentId,
    pub notification_event_id: EventId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLifecycleEvent {
    pub state: TurnLifecycleState,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnLifecycleState {
    Started,
    Steered,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEvent {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Agent,
    ReasoningSummary,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolIntentEvent {
    pub tool_name: String,
    pub arguments: Value,
    pub side_effect: ToolSideEffect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffect {
    Read,
    Navigation,
    Mutation,
    External,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDecisionEvent {
    pub decision: ToolDecision,
    pub decided_by: String,
    pub reason_summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDecision {
    Allowed,
    Denied,
    Escalated,
    RequiresUser,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStartedEvent {
    pub tool_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResultEvent {
    pub outcome: ToolOutcome,
    pub summary: Option<String>,
    pub result: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Completed,
    Failed,
    Interrupted,
    /// The process stopped after an effect may have begun but before a durable
    /// terminal result was recorded. Recovery must not guess success/failure.
    UnknownAfterCrash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMutationEvent {
    pub path: String,
    pub previous_path: Option<String>,
    pub surface: WorkspaceSurface,
    pub file_kind: FileKind,
    /// `None` represents a file that did not exist before the transition.
    pub before_artifact: Option<ArtifactRef>,
    /// `None` represents deletion by the transition.
    pub after_artifact: Option<ArtifactRef>,
    /// Artifact metadata carried with the transition so a durable event log
    /// and its content-addressed blob directory are a self-contained replay
    /// bundle. Older events deserialize with an empty collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<super::ArtifactRecord>,
    pub state: FileMutationState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceSurface {
    Buffer { version: Option<u64> },
    Disk,
    GitIndex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Regular,
    Symlink,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub artifact_id: super::ArtifactId,
    pub availability: ArtifactAvailability,
    pub representation: ContentRepresentation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentRepresentation {
    RawBytes,
    EditorText {
        encoding: Option<String>,
        line_endings: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAvailability {
    Available,
    Missing,
    Redacted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMutationState {
    Proposed,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointEvent {
    pub checkpoint_id: String,
    pub label: Option<String>,
    pub workspace_manifest: Option<super::ManifestId>,
    pub git_commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergenceEvent {
    pub scope: String,
    pub expected_artifact: Option<String>,
    pub actual_artifact: Option<String>,
    pub replayability: Replayability,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Replayability {
    Exact,
    Compatible,
    Diverged,
    Partial,
    NotReplayable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{
        AgentWorkspaceWarning, AgentWorkspaceWarningKind, HandoffConfidence, HandoffEvidence,
        HandoffStatus, HandoffValidator, StructuredHandoffV1,
    };
    use serde_json::json;

    #[test]
    fn preserves_unknown_event_payloads_across_json_round_trip() {
        let raw = json!({
            "kind": "workspace.future_transition",
            "data": { "revision": 7, "paths": ["src/lib.rs"] }
        });

        let event: EventKind = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            event,
            EventKind::Unknown {
                name: "workspace.future_transition".into(),
                payload: json!({ "revision": 7, "paths": ["src/lib.rs"] }),
            }
        );
        assert_eq!(serde_json::to_value(event).unwrap(), raw);
    }

    #[test]
    fn known_event_payloads_round_trip_through_the_same_wire_shape() {
        let event = EventKind::Message(MessageEvent {
            role: MessageRole::ReasoningSummary,
            content: "Inspect the workspace first".into(),
        });

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(serde_json::from_value::<EventKind>(json).unwrap(), event);
    }

    #[test]
    fn handoff_events_round_trip_only_when_the_embedded_payload_validates() {
        let handoff = HandoffValidator::default()
            .validate(
                StructuredHandoffV1 {
                    version: 1,
                    status: HandoffStatus::Completed,
                    summary: "Validated before it became durable.".into(),
                    evidence: vec![HandoffEvidence {
                        path: "ovim-core/src/run_log/event.rs".into(),
                        line: Some(1),
                        claim: "The event contains a validated wrapper.".into(),
                    }],
                    changed_files: vec![],
                    verification: vec![],
                    blockers: vec![],
                    followups: vec![],
                    attachments: vec![],
                    confidence: HandoffConfidence::High,
                },
                Some(HandoffStatus::Completed),
            )
            .unwrap();
        let event = EventKind::AgentHandoff(AgentHandoffEvent {
            handoff,
            workspace_warnings: vec![AgentWorkspaceWarning {
                kind: AgentWorkspaceWarningKind::MissingArtifact,
                path: Some("src/missing.rs".into()),
                artifact_id: None,
                detail: "captured content was unavailable".into(),
            }],
        });
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(serde_json::from_value::<EventKind>(wire).unwrap(), event);

        let invalid = json!({
            "kind": "agent_handoff",
            "data": {
                "handoff": {
                    "version": 1,
                    "status": "completed",
                    "summary": "No evidence",
                    "confidence": "high"
                }
            }
        });
        assert!(serde_json::from_value::<EventKind>(invalid).is_err());
    }
}
