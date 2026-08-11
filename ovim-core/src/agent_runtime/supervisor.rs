//! Concurrent control plane for durable delegated-agent dispatches.

use super::{
    validated_terminal_handoff, AgentApprovalClient, AgentCancellationToken, AgentDispatchRecord,
    AgentDispatchScheduler, AgentLoopBudget, AgentLoopError, AgentLoopEventRecord,
    AgentLoopEventSink, AgentLoopInput, AgentLoopResult, AgentLoopRunner, AgentLoopRuntimeHooks,
    AgentMailbox, AgentMessageClaimOutcome, AgentMessageError, AgentMessageQueue,
    AgentMessageRecord, AgentProviderAdapter, AgentProviderError, AgentProviderEvent,
    AgentProviderFollowup, AgentProviderSession, AgentProviderStart, AgentToolExecutor,
    AgentToolResult, AgentWorkspaceDescriptor, DispatchError, DispatchHandle, DispatchRequest,
    DispatchState, FollowupAgentHandle, FollowupAgentRequest, HandoffStatus, HandoffValidator,
    MailboxError, MailboxWaitOutcome, RootAgentBudget, ScopedToolView, SubagentModelCatalog,
};
use crate::run_log::{
    AgentId, EventEnvelope, EventId, EventKind, MailboxNotificationKind, RunEventSink, RunId,
    TurnId,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::AbortHandle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSupervisorConfig {
    pub max_concurrent: usize,
    pub max_queued: usize,
    pub max_children_per_parent: usize,
    pub max_total_per_run: usize,
    pub max_depth: usize,
    pub child_budget: AgentLoopBudget,
    pub root_max_provider_events: usize,
    pub root_max_tool_calls: usize,
}

impl Default for AgentSupervisorConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            max_queued: 8,
            max_children_per_parent: 4,
            max_total_per_run: 8,
            max_depth: 1,
            child_budget: AgentLoopBudget::default(),
            root_max_provider_events: 1024,
            root_max_tool_calls: 160,
        }
    }
}

pub struct AgentLoopDependencies {
    pub provider: Arc<dyn AgentProviderAdapter>,
    pub tool_view: ScopedToolView,
    pub tool_executor: Arc<dyn AgentToolExecutor>,
    pub approval_client: Arc<dyn AgentApprovalClient>,
    pub workspace: AgentWorkspaceDescriptor,
    pub envelope: super::DelegationEnvelope,
    pub budget: Option<AgentLoopBudget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendAgentMessageRequest {
    pub sender_agent_id: AgentId,
    pub recipient_agent_id: AgentId,
    pub causing_turn_id: TurnId,
    pub caused_by_event: EventId,
    pub content: String,
}

pub trait AgentLoopInputFactory: Send + Sync {
    fn build(&self, dispatch: &AgentDispatchRecord) -> Result<AgentLoopDependencies, String>;
}

#[derive(Clone, Debug)]
pub struct LiveAgentHandle {
    pub dispatch: DispatchHandle,
    pub cancellation: AgentCancellationToken,
    pub abort_handle: AbortHandle,
}

/// Outcome of the steering sweep performed at a terminal boundary. The dispatch
/// is terminal in both cases — the sweep runs after the terminal transition has
/// already been committed, so it can never undo it. `Incomplete` means steering
/// admitted before the boundary is still Queued against a now-terminal child,
/// where it is inert (nothing will ever deliver it) but stays visible in the
/// mailbox projection until a later sweep clears it.
#[derive(Debug)]
#[must_use = "an incomplete sweep leaves steering queued against a terminal child"]
pub enum SteeringSweep {
    Complete,
    Incomplete(AgentMessageError),
}

impl SteeringSweep {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Clone)]
pub struct AgentSupervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    run_id: RunId,
    root_agent_id: AgentId,
    sink: Arc<dyn RunEventSink>,
    scheduler: Arc<Mutex<AgentDispatchScheduler>>,
    factory: Arc<dyn AgentLoopInputFactory>,
    config: AgentSupervisorConfig,
    semaphore: Arc<Semaphore>,
    root_budget: Arc<RootAgentBudget>,
    live: AsyncMutex<BTreeMap<AgentId, LiveAgentHandle>>,
    mailboxes: Mutex<HashMap<AgentId, AgentMailbox>>,
    idle_sessions: Mutex<HashMap<AgentId, Box<dyn AgentProviderSession>>>,
    drain_lock: AsyncMutex<()>,
    changed: Notify,
    /// Terminalizations that failed durably (e.g. ENOSPC appending the
    /// terminal event). Retried from `settle_pending_terminalizations` so a
    /// transient write failure cannot leave a dispatch non-terminal for the
    /// process lifetime — permanently holding a concurrency slot and blocking
    /// `wait_for_idle`. (OV-00318)
    pending_terminalizations: Mutex<HashMap<AgentId, PendingTerminalization>>,
}

/// A child whose infrastructure failure could not be recorded durably yet.
struct PendingTerminalization {
    record: AgentDispatchRecord,
    status: HandoffStatus,
    detail: String,
}

impl AgentSupervisor {
    pub fn new(
        run_id: RunId,
        root_agent_id: AgentId,
        sink: Arc<dyn RunEventSink>,
        model_catalog: Arc<SubagentModelCatalog>,
        factory: Arc<dyn AgentLoopInputFactory>,
        config: AgentSupervisorConfig,
    ) -> Result<Self, AgentSupervisorError> {
        validate_config(&config)?;
        let mut scheduler =
            AgentDispatchScheduler::new(run_id.clone(), sink.clone(), model_catalog);
        scheduler.set_external_parent(root_agent_id.clone());
        Ok(Self::from_scheduler(
            run_id,
            root_agent_id,
            sink,
            scheduler,
            factory,
            config,
        ))
    }

    /// Rehydrate durable ownership. Queued children are eligible to run;
    /// anything that may have crossed a provider/effect boundary is already
    /// terminated with a validated interrupted handoff by the scheduler.
    pub fn rehydrate(
        run_id: RunId,
        root_agent_id: AgentId,
        sink: Arc<dyn RunEventSink>,
        model_catalog: Arc<SubagentModelCatalog>,
        factory: Arc<dyn AgentLoopInputFactory>,
        config: AgentSupervisorConfig,
    ) -> Result<Self, AgentSupervisorError> {
        validate_config(&config)?;
        let mut scheduler =
            AgentDispatchScheduler::rehydrate(run_id.clone(), sink.clone(), model_catalog)?;
        scheduler.set_external_parent(root_agent_id.clone());
        let supervisor =
            Self::from_scheduler(run_id, root_agent_id, sink, scheduler, factory, config);
        supervisor.recover_agent_messages()?;
        supervisor.recover_mailbox_notifications()?;
        Ok(supervisor)
    }

    fn from_scheduler(
        run_id: RunId,
        root_agent_id: AgentId,
        sink: Arc<dyn RunEventSink>,
        scheduler: AgentDispatchScheduler,
        factory: Arc<dyn AgentLoopInputFactory>,
        config: AgentSupervisorConfig,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                run_id,
                root_agent_id,
                sink,
                scheduler: Arc::new(Mutex::new(scheduler)),
                factory,
                semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
                root_budget: Arc::new(RootAgentBudget::new(
                    config.root_max_provider_events,
                    config.root_max_tool_calls,
                )),
                config,
                live: AsyncMutex::new(BTreeMap::new()),
                mailboxes: Mutex::new(HashMap::new()),
                idle_sessions: Mutex::new(HashMap::new()),
                drain_lock: AsyncMutex::new(()),
                changed: Notify::new(),
                pending_terminalizations: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn dispatch(
        &self,
        request: DispatchRequest,
    ) -> Result<DispatchHandle, AgentSupervisorError> {
        let handle = self.allocate(request)?;
        self.drain_ready().await?;
        Ok(handle)
    }

    /// Durably allocate and queue a child, then drain in an independent task.
    /// This is the parent-tool boundary: the caller receives the stable child
    /// identity without waiting for provider startup or completion.
    pub fn dispatch_nonblocking(
        &self,
        request: DispatchRequest,
    ) -> Result<DispatchHandle, AgentSupervisorError> {
        let handle = self.allocate(request)?;
        let supervisor = self.clone();
        tokio::spawn(async move {
            if let Err(error) = supervisor.drain_ready().await {
                crate::log_warn!("agent_runtime", "could not drain child queue: {error}");
            }
        });
        Ok(handle)
    }

    fn allocate(&self, request: DispatchRequest) -> Result<DispatchHandle, AgentSupervisorError> {
        // Settle first: a dispatch wedged non-terminal by a failed append
        // would otherwise count against the per-run limit forever.
        self.settle_pending_terminalizations();
        let mut scheduler = self.inner.scheduler.lock().map_err(|_| poisoned())?;
        let records = scheduler.dispatch_records();
        self.validate_dispatch_limits(&records, &request)?;
        scheduler.dispatch(request).map_err(Into::into)
    }

    pub fn state(&self, agent_id: &AgentId) -> Result<Option<DispatchState>, AgentSupervisorError> {
        Ok(self
            .inner
            .scheduler
            .lock()
            .map_err(|_| poisoned())?
            .state(agent_id)
            .cloned())
    }

    pub fn dispatches(&self) -> Result<Vec<AgentDispatchRecord>, AgentSupervisorError> {
        Ok(self
            .inner
            .scheduler
            .lock()
            .map_err(|_| poisoned())?
            .dispatch_records())
    }

    pub async fn live_handles(&self) -> BTreeMap<AgentId, LiveAgentHandle> {
        self.inner.live.lock().await.clone()
    }

    pub fn mailbox(&self, recipient: AgentId) -> Result<AgentMailbox, AgentSupervisorError> {
        let mut mailboxes = self.inner.mailboxes.lock().map_err(|_| poisoned())?;
        if let Some(mailbox) = mailboxes.get(&recipient) {
            return Ok(mailbox.clone());
        }
        let mailbox = AgentMailbox::new(
            self.inner.run_id.clone(),
            recipient.clone(),
            self.inner.sink.clone(),
        )?;
        mailboxes.insert(recipient, mailbox.clone());
        Ok(mailbox)
    }

    pub fn message_queue(
        &self,
        recipient: AgentId,
    ) -> Result<AgentMessageQueue, AgentSupervisorError> {
        Ok(AgentMessageQueue::new(
            self.inner.run_id.clone(),
            recipient.clone(),
            self.inner.sink.clone(),
            self.mailbox(recipient)?,
        )?)
    }

    pub fn messages(&self) -> Result<Vec<AgentMessageRecord>, AgentSupervisorError> {
        let projection =
            super::AgentMessageProjection::rehydrate(&self.inner.sink.events(&self.inner.run_id)?)
                .map_err(AgentMessageError::Projection)?;
        Ok(projection.messages().cloned().collect())
    }

    /// Reject any unsettled steering for `agent_id` and run `finish` as one
    /// scheduler-locked step. The scheduler lock is the admission/termination
    /// exclusion: a concurrent `send_message` either enqueues before this sweep
    /// (and is rejected here) or observes the resulting terminal state and is
    /// refused, so a parent-authored message can never be durably orphaned
    /// across the child's terminal boundary. Every terminal path funnels through
    /// here so the sweep cannot be forgotten (the infrastructure-failure path
    /// previously skipped it). The message-queue view is built before the lock
    /// so the lock order stays scheduler-only.
    fn terminalize_locked<T>(
        &self,
        agent_id: &AgentId,
        finish: impl FnOnce(&mut AgentDispatchScheduler) -> Result<T, AgentSupervisorError>,
    ) -> Result<(T, SteeringSweep), AgentSupervisorError> {
        let queue = self.message_queue(agent_id.clone())?;
        let mut scheduler = self.inner.scheduler.lock().map_err(|_| poisoned())?;
        // Close admission FIRST, then sweep. send_message checks the dispatch
        // state under this same lock, so once `finish` lands no further steering
        // can be enqueued and the sweep below only has to drain what was
        // admitted before this point — the exclusion is preserved either way.
        //
        // The order matters because reject_queued is a CAS against the run-wide
        // event sequence (any agent's append invalidates a claim) and can fail
        // with ConcurrentUpdateLimit for reasons that have nothing to do with
        // this child. Sweeping first made the terminal transition hostage to
        // that CAS: one exhausted retry loop left the dispatch non-terminal
        // forever, so wait_for_idle could never return true again and the parent
        // never got a handoff. Reaching a terminal state is the load-bearing
        // invariant; the sweep is cleanup and is reported, not fatal.
        let finished = finish(&mut scheduler)?;
        let sweep = Self::sweep_unsettled(&queue);
        Ok((finished, sweep))
    }

    /// Best-effort rejection of steering that was admitted before the terminal
    /// boundary. Returns rather than propagates, so a failure cannot undo the
    /// terminal transition that has already been committed by the caller.
    fn sweep_unsettled(queue: &AgentMessageQueue) -> SteeringSweep {
        let unsettled = match queue.unsettled() {
            Ok(unsettled) => unsettled,
            Err(error) => return SteeringSweep::Incomplete(error),
        };
        for message in unsettled {
            if let Err(error) = queue.reject_queued(
                &message.message_event_id,
                "child reached a terminal boundary before message delivery",
            ) {
                return SteeringSweep::Incomplete(error);
            }
        }
        SteeringSweep::Complete
    }

    /// Queue one bounded parent-authored message for a currently active child.
    /// The provider wrapper accepts it only between provider/tool operations;
    /// terminal and merely queued children reject steering explicitly.
    pub fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> Result<AgentMessageRecord, AgentSupervisorError> {
        // Build the queue view before locking the scheduler, then hold that lock
        // across both the liveness check and the durable enqueue. This makes the
        // scheduler lock the admission/termination exclusion (see
        // terminalize_locked): a terminal transition cannot slip between the
        // check and the append, so the message is either delivered/rejected or
        // the target is refused as not-live — never durably orphaned.
        let queue = self.message_queue(request.recipient_agent_id.clone())?;
        let scheduler = self.inner.scheduler.lock().map_err(|_| poisoned())?;
        let record = scheduler
            .dispatch_record(&request.recipient_agent_id)
            .ok_or_else(|| {
                AgentSupervisorError::UnknownAgent(request.recipient_agent_id.clone())
            })?;
        let expected_parent = record
            .parent_agent_id
            .as_ref()
            .unwrap_or(&self.inner.root_agent_id);
        // The direct parent owns model-to-model steering. The durable root is
        // also the human/operator authority and may steer any visible
        // descendant from the agent inspector without impersonating the
        // intermediate parent.
        if expected_parent != &request.sender_agent_id
            && self.inner.root_agent_id != request.sender_agent_id
        {
            return Err(AgentSupervisorError::MessageSenderMismatch {
                recipient: request.recipient_agent_id,
                sender: request.sender_agent_id,
            });
        }
        if !matches!(
            record.state,
            DispatchState::Starting
                | DispatchState::Running
                | DispatchState::WaitingForAgent
                | DispatchState::WaitingForTool
                | DispatchState::WaitingForUser
        ) {
            return Err(AgentSupervisorError::MessageTargetNotLive {
                agent_id: request.recipient_agent_id,
                state: record.state,
            });
        }
        let queued = queue.queue(
            request.sender_agent_id,
            request.causing_turn_id,
            request.caused_by_event,
            request.content,
        )?;
        drop(scheduler);
        self.inner.changed.notify_waiters();
        Ok(queued)
    }

    pub async fn followup_agent(
        &self,
        mut request: FollowupAgentRequest,
    ) -> Result<FollowupAgentHandle, AgentSupervisorError> {
        if self.inner.live.lock().await.contains_key(&request.agent_id) {
            return Err(AgentSupervisorError::FollowupTargetBusy(request.agent_id));
        }
        let record = self
            .inner
            .scheduler
            .lock()
            .map_err(|_| poisoned())?
            .dispatch_record(&request.agent_id)
            .ok_or_else(|| AgentSupervisorError::UnknownAgent(request.agent_id.clone()))?;
        let dependencies = self
            .inner
            .factory
            .build(&record)
            .map_err(AgentSupervisorError::InputFactory)?;
        let inherited_budget = dependencies
            .budget
            .unwrap_or_else(|| self.inner.config.child_budget.clone());
        if request.budget.timeout > inherited_budget.timeout
            || request.budget.max_provider_events > inherited_budget.max_provider_events
            || request.budget.max_tool_calls > inherited_budget.max_tool_calls
        {
            return Err(AgentSupervisorError::FollowupBudgetWidening);
        }
        let retained = self
            .inner
            .idle_sessions
            .lock()
            .map_err(|_| poisoned())?
            .get(&request.agent_id)
            .is_some_and(|session| session.can_followup());
        request.retained_session_requested = retained;
        let followup = self
            .inner
            .scheduler
            .lock()
            .map_err(|_| poisoned())?
            .begin_followup(request)?;
        self.drain_ready().await?;
        Ok(followup)
    }

    pub async fn interrupt(
        &self,
        agent_id: &AgentId,
        reason: impl Into<String>,
    ) -> Result<Vec<AgentId>, AgentSupervisorError> {
        let reason = reason.into();
        let records = self.dispatches()?;
        if !records
            .iter()
            .any(|record| &record.handle.agent_id == agent_id)
        {
            return Err(AgentSupervisorError::UnknownAgent(agent_id.clone()));
        }
        let mut descendants = descendants_including(&records, agent_id);
        descendants.sort();
        descendants.reverse();
        // Classify and terminate under `live` held for the whole pass, matching
        // drain_ready's live -> scheduler order. Because promotion holds `live`
        // across Queued -> Starting, any descendant absent from the live map
        // here is provably not mid-start: if it is still non-terminal it is safe
        // to terminate before it ever reaches the provider. Terminal records are
        // collected and notified after the locks are released.
        let mut terminals = Vec::new();
        {
            let live = self.inner.live.lock().await;
            for current in &descendants {
                if let Some(handle) = live.get(current) {
                    handle.cancellation.cancel(reason.clone());
                    continue;
                }
                // Not live. Because promotion holds `live` (see drain_ready),
                // this child is not mid-start; terminalize it through the shared
                // helper so any queued steering is rejected too, unless it has
                // already reached a terminal state.
                let reason = reason.clone();
                let outcome = self.terminalize_locked(current, |scheduler| {
                    let non_terminal = scheduler
                        .state(current)
                        .is_some_and(|state| !state.is_terminal());
                    if !non_terminal {
                        return Ok(None);
                    }
                    let record = scheduler
                        .dispatch_record(current)
                        .ok_or_else(|| AgentSupervisorError::UnknownAgent(current.clone()))?;
                    let terminal = scheduler.finish_with_handoff(
                        &record.handle,
                        validated_terminal_handoff(
                            HandoffStatus::Interrupted,
                            format!("cancelled before provider start: {reason}"),
                        )?,
                    )?;
                    Ok(Some((record, terminal)))
                })?;
                let (outcome, _sweep) = outcome;
                if let Some(pair) = outcome {
                    terminals.push(pair);
                }
            }
        }
        for (record, terminal) in terminals {
            self.notify_terminal(&record, &terminal)?;
        }
        self.inner.changed.notify_waiters();
        Ok(descendants)
    }

    pub async fn wait_for_idle(&self, timeout: Duration) -> Result<bool, AgentSupervisorError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Arm the change waiter BEFORE reading state. Notify stores no
            // permit, so a terminal notify_waiters() that fires between the read
            // and the await would otherwise be lost, blocking to the deadline
            // even though the run is already idle. enable() registers interest
            // now; a notify after this point makes the await return immediately.
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            self.settle_pending_terminalizations();
            let all_terminal = self
                .dispatches()?
                .iter()
                .all(|record| record.state.is_terminal());
            if all_terminal && self.inner.live.lock().await.is_empty() {
                return Ok(true);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(false);
            }
        }
    }

    pub async fn start_recovered(&self) -> Result<(), AgentSupervisorError> {
        self.drain_ready().await
    }

    /// Wait for a delegated child's mailbox without consuming one of the
    /// provider-concurrency slots needed by that child. The parent remains a
    /// live task and keeps its original permit; a temporary permit is added
    /// while it is semantically waiting, then retired before the parent can
    /// resume its provider loop.
    pub async fn wait_for_agent_updates(
        &self,
        handle: &DispatchHandle,
        timeout: Duration,
    ) -> Result<MailboxWaitOutcome, AgentSupervisorError> {
        self.inner
            .scheduler
            .lock()
            .map_err(|_| poisoned())?
            .transition(
                handle,
                DispatchState::WaitingForAgent,
                Some("waiting for delegated-agent updates".into()),
            )?;
        let yielded = YieldedConcurrencySlot::new(self.inner.semaphore.clone());
        if let Err(error) = self.drain_ready().await {
            yielded.restore().await;
            return Err(error);
        }
        let outcome = self.mailbox(handle.agent_id.clone())?.wait(timeout).await;
        yielded.restore().await;
        outcome.map_err(AgentSupervisorError::from)
    }

    async fn drain_ready(&self) -> Result<(), AgentSupervisorError> {
        let _guard = self.inner.drain_lock.lock().await;
        self.settle_pending_terminalizations();
        loop {
            let permit = match self.inner.semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => break,
            };
            // Hold `live` across the entire Queued -> Starting promotion and the
            // live-map insertion. A child is therefore present in `live` for the
            // whole time it is not Queued, closing the window where interrupt()
            // could observe a Starting child that is absent from the map (and so
            // neither cancel nor terminate it while still reporting it stopped).
            // Lock order is live -> scheduler everywhere; the gate below still
            // prevents provider execution until after publication.
            let mut live = self.inner.live.lock().await;
            let record = {
                let mut scheduler = self.inner.scheduler.lock().map_err(|_| poisoned())?;
                let Some(record) = scheduler.queued_dispatches().into_iter().next() else {
                    drop(permit);
                    break;
                };
                scheduler.transition(
                    &record.handle,
                    DispatchState::Starting,
                    Some("supervisor allocated provider concurrency".into()),
                )?;
                record
            };
            let cancellation = AgentCancellationToken::new();
            let supervisor = self.clone();
            let task_record = record.clone();
            let task_cancellation = cancellation.clone();
            let (launch, launched) = tokio::sync::oneshot::channel();
            // The permit is handed to run_one rather than held by this block, so
            // run_one can release it before spawning the re-drain. Dropping it
            // here (i.e. after run_one returns) would let the re-drain observe
            // the slot as still occupied and give up with a child left Queued.
            let task = tokio::spawn(async move {
                let _ = launched.await;
                supervisor
                    .run_one(task_record, task_cancellation, permit)
                    .await;
            });
            live.insert(
                record.handle.agent_id.clone(),
                LiveAgentHandle {
                    dispatch: record.handle,
                    cancellation,
                    abort_handle: task.abort_handle(),
                },
            );
            // Publish before releasing the gate so the spawned task cannot reach
            // run_one (and its own `live` access) until the handle is in place.
            drop(live);
            let _ = launch.send(());
        }
        Ok(())
    }

    async fn run_one(
        &self,
        record: AgentDispatchRecord,
        cancellation: AgentCancellationToken,
        permit: OwnedSemaphorePermit,
    ) {
        let result = self.run_one_inner(&record, cancellation).await;
        if let Err(error) = result {
            if let Ok(mut sessions) = self.inner.idle_sessions.lock() {
                sessions.remove(&record.handle.agent_id);
            }
            let status = match &error {
                AgentSupervisorError::Loop(AgentLoopError::CancelledBeforeBinding(_)) => {
                    HandoffStatus::Interrupted
                }
                AgentSupervisorError::Loop(AgentLoopError::TimedOutBeforeBinding) => {
                    HandoffStatus::TimedOut
                }
                _ => HandoffStatus::Failed,
            };
            // Route through terminalize_locked so an infrastructure failure also
            // rejects any unsettled steering (this path previously skipped it).
            let detail = format!("child loop infrastructure failed: {error}");
            match self.terminalize_failed_child(&record, status, &detail) {
                Ok(Some(terminal)) => {
                    if let Err(notify_error) = self.notify_terminal(&record, &terminal) {
                        crate::log_warn!(
                            "agent_runtime",
                            "terminal handoff for {} could not be delivered to its parent: {notify_error}",
                            record.handle.agent_id
                        );
                    }
                }
                Ok(None) => {}
                Err(terminalize_error) => {
                    // Recording the failure durably failed too (e.g. a full
                    // disk). Park it for retry instead of leaving the dispatch
                    // non-terminal for the process lifetime. (OV-00318)
                    crate::log_warn!(
                        "agent_runtime",
                        "could not terminalize failed child {}: {terminalize_error}; will retry",
                        record.handle.agent_id
                    );
                    if let Ok(mut pending) = self.inner.pending_terminalizations.lock() {
                        pending.insert(
                            record.handle.agent_id.clone(),
                            PendingTerminalization {
                                record: record.clone(),
                                status,
                                detail,
                            },
                        );
                    }
                }
            }
        }
        self.inner.live.lock().await.remove(&record.handle.agent_id);
        // Release the concurrency slot BEFORE spawning the re-drain. The re-drain
        // gives up on the first try_acquire_owned miss, so if the permit were
        // still held here (as it was when the spawned task owned it and dropped
        // it only after this function returned) the drain could observe a full
        // semaphore and return with a child left Queued and nothing scheduled to
        // retry it — a permanent stall, since the only other drain triggers are
        // new dispatches. This is only reachable on a multi-threaded runtime;
        // under the current-thread runtime the spawned drain cannot start until
        // this task yields, which is why the existing tests never caught it.
        drop(permit);
        self.inner.changed.notify_waiters();
        tokio::spawn(self.clone().drain_ready_task());
    }

    /// Terminalize a child whose loop failed: resume a durable-but-unfinished
    /// handoff if one exists, otherwise record a conservative terminal
    /// handoff. `Ok(None)` means the dispatch is already terminal.
    fn terminalize_failed_child(
        &self,
        record: &AgentDispatchRecord,
        status: HandoffStatus,
        detail: &str,
    ) -> Result<Option<super::DispatchTerminalRecord>, AgentSupervisorError> {
        self.terminalize_locked(&record.handle.agent_id, |scheduler| {
            let non_terminal = scheduler
                .state(&record.handle.agent_id)
                .is_some_and(|state| !state.is_terminal());
            if !non_terminal {
                return Ok(None);
            }
            Ok(Some(scheduler.terminate_conservatively(
                &record.handle,
                status,
                detail,
            )?))
        })
        .map(|(terminal, _sweep)| terminal)
    }

    /// Retry terminalizations whose durable append previously failed. Called
    /// from the dispatch/drain/wait paths so the first operation after the
    /// write failure clears (e.g. disk space freed) settles the wedged
    /// dispatches, releases their slots, and notifies their parents.
    fn settle_pending_terminalizations(&self) {
        let drained: Vec<(AgentId, PendingTerminalization)> = {
            let Ok(mut pending) = self.inner.pending_terminalizations.lock() else {
                return;
            };
            pending.drain().collect()
        };
        if drained.is_empty() {
            return;
        }
        let mut settled_any = false;
        for (agent_id, item) in drained {
            match self.terminalize_failed_child(&item.record, item.status, &item.detail) {
                Ok(Some(terminal)) => {
                    settled_any = true;
                    if let Err(notify_error) = self.notify_terminal(&item.record, &terminal) {
                        crate::log_warn!(
                            "agent_runtime",
                            "terminal handoff for {agent_id} could not be delivered to its parent: {notify_error}"
                        );
                    }
                }
                Ok(None) => settled_any = true,
                Err(error) => {
                    crate::log_warn!(
                        "agent_runtime",
                        "retried terminalization for {agent_id} still failing: {error}"
                    );
                    if let Ok(mut pending) = self.inner.pending_terminalizations.lock() {
                        pending.insert(agent_id, item);
                    }
                }
            }
        }
        if settled_any {
            self.inner.changed.notify_waiters();
        }
    }

    fn drain_ready_task(self) -> super::AgentFuture<'static, ()> {
        Box::pin(async move {
            let _ = self.drain_ready().await;
        })
    }

    async fn run_one_inner(
        &self,
        record: &AgentDispatchRecord,
        cancellation: AgentCancellationToken,
    ) -> Result<(), AgentSupervisorError> {
        let mut dependencies = self
            .inner
            .factory
            .build(record)
            .map_err(AgentSupervisorError::InputFactory)?;
        if dependencies.workspace.assignment != record.handle.workspace {
            return Err(AgentSupervisorError::InputFactory(
                "factory workspace differs from durable dispatch".into(),
            ));
        }
        let bridge = Arc::new(SchedulerLoopBridge {
            scheduler: self.inner.scheduler.clone(),
        });
        let message_queue = self.message_queue(record.handle.agent_id.clone())?;
        let provider: Arc<dyn AgentProviderAdapter> = Arc::new(MessageDeliveringProvider {
            inner: dependencies.provider,
            queue: message_queue.clone(),
        });
        let retained_session = if record.followup.is_some() {
            self.inner
                .idle_sessions
                .lock()
                .map_err(|_| poisoned())?
                .remove(&record.handle.agent_id)
        } else {
            None
        };
        if let Some(followup) = &record.followup {
            dependencies.envelope.objective = followup.objective.clone();
            dependencies.envelope.parent_brief = Some(format!(
                "Previous validated handoff: {}",
                followup.prior_handoff_summary
            ));
            if let Some(identity) = dependencies.envelope.identity.as_mut() {
                identity.causing_turn_id = followup.parent_turn_id.clone();
                identity.causing_event_id = followup.parent_event_id.clone();
            }
        }
        let budget = record
            .followup
            .as_ref()
            .map(|followup| followup.budget.clone())
            .or(dependencies.budget)
            .unwrap_or_else(|| self.inner.config.child_budget.clone());
        let input = AgentLoopInput {
            handle: record.handle.clone(),
            envelope: dependencies.envelope,
            route: record.resolved_route.clone(),
            provider,
            tool_view: dependencies.tool_view,
            tool_executor: dependencies.tool_executor,
            event_sink: bridge.clone(),
            runtime_hooks: bridge,
            approval_client: dependencies.approval_client,
            workspace: dependencies.workspace,
            cancellation,
            budget,
            root_budget: self.inner.root_budget.clone(),
            handoff_validator: HandoffValidator::default(),
            turn_generation: record.turn_generation,
        };
        let AgentLoopResult {
            handoff,
            workspace_warnings,
            retained_session: next_idle_session,
            ..
        } = if let (Some(session), Some(followup)) = (retained_session, record.followup.as_ref()) {
            AgentLoopRunner::run_followup(
                input,
                session,
                AgentProviderFollowup {
                    handle: record.handle.clone(),
                    followup_turn_id: followup.followup_turn_id.clone(),
                    turn_generation: followup.turn_generation,
                    objective: followup.objective.clone(),
                },
            )
            .await?
        } else {
            AgentLoopRunner::run(input).await?
        };
        let (terminal, _sweep) = self.terminalize_locked(&record.handle.agent_id, |scheduler| {
            Ok(scheduler.finish_with_handoff_and_warnings(
                &record.handle,
                handoff,
                workspace_warnings,
            )?)
        })?;
        if let Some(session) = next_idle_session {
            self.inner
                .idle_sessions
                .lock()
                .map_err(|_| poisoned())?
                .insert(record.handle.agent_id.clone(), session);
        }
        self.notify_terminal(record, &terminal)?;
        Ok(())
    }

    fn notify_terminal(
        &self,
        record: &AgentDispatchRecord,
        terminal: &super::DispatchTerminalRecord,
    ) -> Result<(), AgentSupervisorError> {
        let EventKind::AgentHandoff(handoff) = &terminal.handoff_event.kind else {
            return Err(AgentSupervisorError::InvalidHistory(
                "terminal record does not contain a handoff".into(),
            ));
        };
        let recipient = record
            .parent_agent_id
            .clone()
            .unwrap_or_else(|| self.inner.root_agent_id.clone());
        self.mailbox(recipient)?
            .notify(MailboxNotificationKind::Handoff {
                source_agent_id: record.handle.agent_id.clone(),
                terminal_event_id: terminal.terminal_event.event_id.clone(),
                handoff_event_id: terminal.handoff_event.event_id.clone(),
                handoff: Box::new(
                    handoff
                        .handoff
                        .parent_projection_with_workspace_warnings(&handoff.workspace_warnings),
                ),
            })?;
        self.inner.changed.notify_waiters();
        Ok(())
    }

    fn recover_mailbox_notifications(&self) -> Result<(), AgentSupervisorError> {
        let events = self.inner.sink.events(&self.inner.run_id)?;
        let notified = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::MailboxNotification(notification) => match &notification.notification {
                    MailboxNotificationKind::Handoff {
                        source_agent_id,
                        terminal_event_id,
                        handoff_event_id,
                        ..
                    } => Some((
                        source_agent_id.clone(),
                        terminal_event_id.clone(),
                        handoff_event_id.clone(),
                    )),
                    _ => None,
                },
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let records = self
            .dispatches()?
            .into_iter()
            .map(|record| (record.handle.agent_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        // One stable AgentId may have several terminal generations. Recover
        // every missing handoff notification, not only the latest terminal
        // event, while the mailbox projection prevents duplicate consumption.
        for terminal in events.iter().filter(|event| {
            matches!(
                event.kind,
                EventKind::AgentLifecycle(ref lifecycle)
                    if matches!(
                        lifecycle.state,
                        crate::run_log::AgentLifecycleState::Completed
                            | crate::run_log::AgentLifecycleState::Failed
                            | crate::run_log::AgentLifecycleState::Interrupted
                    )
            )
        }) {
            let Some(record) = terminal
                .agent_id
                .as_ref()
                .and_then(|agent_id| records.get(agent_id))
            else {
                continue;
            };
            let Some(handoff_id) = terminal.caused_by.as_ref() else {
                continue;
            };
            if notified.contains(&(
                record.handle.agent_id.clone(),
                terminal.event_id.clone(),
                handoff_id.clone(),
            )) {
                continue;
            }
            let Some(handoff_event) = events
                .iter()
                .find(|event| &event.event_id == handoff_id)
                .cloned()
            else {
                continue;
            };
            self.notify_terminal(
                record,
                &super::DispatchTerminalRecord {
                    handoff_event,
                    terminal_event: terminal.clone(),
                },
            )?;
        }
        Ok(())
    }

    fn recover_agent_messages(&self) -> Result<(), AgentSupervisorError> {
        for record in self.dispatches()? {
            self.message_queue(record.handle.agent_id)?
                .reconcile_after_restart()?;
        }
        Ok(())
    }

    fn validate_dispatch_limits(
        &self,
        records: &[AgentDispatchRecord],
        request: &DispatchRequest,
    ) -> Result<(), AgentSupervisorError> {
        if records.len() >= self.inner.config.max_total_per_run {
            return Err(AgentSupervisorError::RunAgentLimit);
        }
        let queued = records
            .iter()
            .filter(|record| record.state == DispatchState::Queued)
            .count();
        if queued >= self.inner.config.max_queued {
            return Err(AgentSupervisorError::QueueLimit);
        }
        if let Some(parent) = &request.parent_agent_id {
            let children = records
                .iter()
                .filter(|record| record.parent_agent_id.as_ref() == Some(parent))
                .count();
            if children >= self.inner.config.max_children_per_parent {
                return Err(AgentSupervisorError::ParentChildLimit(parent.clone()));
            }
            let depth = if parent == &self.inner.root_agent_id {
                1
            } else {
                agent_depth(records, parent, &self.inner.root_agent_id)? + 1
            };
            if depth > self.inner.config.max_depth {
                return Err(AgentSupervisorError::DepthLimit { depth });
            }
        }
        Ok(())
    }
}

struct SchedulerLoopBridge {
    scheduler: Arc<Mutex<AgentDispatchScheduler>>,
}

impl AgentLoopEventSink for SchedulerLoopBridge {
    fn record(
        &self,
        event: AgentLoopEventRecord,
    ) -> super::AgentFuture<'_, Result<EventEnvelope, AgentLoopError>> {
        Box::pin(async move {
            self.scheduler
                .lock()
                .map_err(|_| AgentLoopError::EventSink("scheduler lock poisoned".into()))?
                .record_runtime_event(
                    &event.handle,
                    event.kind,
                    event.operation_id,
                    event.provider_call_id,
                )
                .map_err(|error| AgentLoopError::EventSink(error.to_string()))
        })
    }
}

impl AgentLoopRuntimeHooks for SchedulerLoopBridge {
    fn transition(
        &self,
        handle: &DispatchHandle,
        state: DispatchState,
        detail: Option<String>,
    ) -> super::AgentFuture<'_, Result<EventEnvelope, AgentLoopError>> {
        let handle = handle.clone();
        Box::pin(async move {
            self.scheduler
                .lock()
                .map_err(|_| AgentLoopError::RuntimeHook("scheduler lock poisoned".into()))?
                .transition(&handle, state, detail)
                .map_err(|error| AgentLoopError::RuntimeHook(error.to_string()))
        })
    }
}

#[derive(Clone)]
struct MessageDeliveringProvider {
    inner: Arc<dyn AgentProviderAdapter>,
    queue: AgentMessageQueue,
}

impl AgentProviderAdapter for MessageDeliveringProvider {
    fn start(
        &self,
        request: AgentProviderStart,
    ) -> super::AgentFuture<'_, Result<Box<dyn AgentProviderSession>, AgentProviderError>> {
        Box::pin(async move {
            let session = self.inner.start(request).await?;
            Ok(Box::new(MessageDeliveringSession {
                inner: session,
                queue: self.queue.clone(),
            }) as Box<dyn AgentProviderSession>)
        })
    }
}

struct MessageDeliveringSession {
    inner: Box<dyn AgentProviderSession>,
    queue: AgentMessageQueue,
}

impl MessageDeliveringSession {
    async fn deliver_pending(&mut self) -> Result<(), AgentProviderError> {
        loop {
            let Some(message) = self
                .queue
                .queued()
                .map_err(|error| AgentProviderError::new(error.to_string()))?
                .into_iter()
                .next()
            else {
                return Ok(());
            };
            let session_id = self.inner.binding().session_id.clone();
            match self
                .queue
                .claim(&message.message_event_id, &session_id)
                .map_err(|error| AgentProviderError::new(error.to_string()))?
            {
                AgentMessageClaimOutcome::Claimed {
                    delivery_event_id, ..
                } => {
                    let result = self
                        .inner
                        .deliver_message(&message.message_event_id, &message.content)
                        .await
                        .map_err(|error| error.to_string());
                    self.queue
                        .finish_delivery(&message.message_event_id, &delivery_event_id, result)
                        .map_err(|error| AgentProviderError::new(error.to_string()))?;
                }
                AgentMessageClaimOutcome::AlreadyClaimed(_) => {
                    self.queue
                        .reject_queued(
                            &message.message_event_id,
                            "message delivery was already attempted at an ambiguous boundary",
                        )
                        .map_err(|error| AgentProviderError::new(error.to_string()))?;
                }
                AgentMessageClaimOutcome::Terminal(_) => {
                    self.queue
                        .reject_queued(
                            &message.message_event_id,
                            "message delivery was already terminal",
                        )
                        .map_err(|error| AgentProviderError::new(error.to_string()))?;
                }
            }
        }
    }
}

impl AgentProviderSession for MessageDeliveringSession {
    fn binding(&self) -> &super::ProviderBinding {
        self.inner.binding()
    }

    fn next_event(
        &mut self,
    ) -> super::AgentFuture<'_, Result<AgentProviderEvent, AgentProviderError>> {
        Box::pin(async move {
            self.deliver_pending().await?;
            self.inner.next_event().await
        })
    }

    fn submit_tool_result(
        &mut self,
        tool_call_id: &str,
        result: &AgentToolResult,
    ) -> super::AgentFuture<'_, Result<(), AgentProviderError>> {
        let tool_call_id = tool_call_id.to_string();
        let result = result.clone();
        Box::pin(async move {
            self.inner
                .submit_tool_result(&tool_call_id, &result)
                .await?;
            self.deliver_pending().await
        })
    }

    fn deliver_message(
        &mut self,
        message_event_id: &EventId,
        content: &str,
    ) -> super::AgentFuture<'_, Result<(), AgentProviderError>> {
        self.inner.deliver_message(message_event_id, content)
    }

    fn repair_handoff(
        &mut self,
        payload: &[u8],
        validation_error: &str,
    ) -> super::AgentFuture<'_, Result<(), AgentProviderError>> {
        self.inner.repair_handoff(payload, validation_error)
    }

    fn can_followup(&self) -> bool {
        self.inner.can_followup()
    }

    fn start_followup(
        &mut self,
        followup: &super::AgentProviderFollowup,
    ) -> super::AgentFuture<'_, Result<(), AgentProviderError>> {
        self.inner.start_followup(followup)
    }
}

struct YieldedConcurrencySlot {
    semaphore: Arc<Semaphore>,
    restored: bool,
}

impl YieldedConcurrencySlot {
    fn new(semaphore: Arc<Semaphore>) -> Self {
        semaphore.add_permits(1);
        Self {
            semaphore,
            restored: false,
        }
    }

    async fn restore(mut self) {
        if let Ok(permit) = self.semaphore.clone().acquire_owned().await {
            permit.forget();
        }
        self.restored = true;
    }
}

impl Drop for YieldedConcurrencySlot {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        let semaphore = self.semaphore.clone();
        tokio::spawn(async move {
            if let Ok(permit) = semaphore.acquire_owned().await {
                permit.forget();
            }
        });
    }
}

fn validate_config(config: &AgentSupervisorConfig) -> Result<(), AgentSupervisorError> {
    if config.max_concurrent == 0
        || config.max_queued == 0
        || config.max_children_per_parent == 0
        || config.max_total_per_run == 0
        || config.root_max_provider_events == 0
        || config.root_max_tool_calls == 0
    {
        return Err(AgentSupervisorError::InvalidConfig(
            "supervisor limits must be positive".into(),
        ));
    }
    Ok(())
}

fn agent_depth(
    records: &[AgentDispatchRecord],
    agent_id: &AgentId,
    root_agent_id: &AgentId,
) -> Result<usize, AgentSupervisorError> {
    let by_id = records
        .iter()
        .map(|record| (&record.handle.agent_id, record))
        .collect::<BTreeMap<_, _>>();
    let mut current = agent_id;
    let mut seen = BTreeSet::new();
    let mut depth = 0;
    while current != root_agent_id {
        if !seen.insert(current.clone()) {
            return Err(AgentSupervisorError::InvalidHistory(
                "agent ancestry contains a cycle".into(),
            ));
        }
        let record = by_id
            .get(current)
            .ok_or_else(|| AgentSupervisorError::UnknownAgent(current.clone()))?;
        let Some(parent) = record.parent_agent_id.as_ref() else {
            // Transport-neutral scheduler users may allocate a top-level
            // agent without recording the external root as its parent. Editor
            // runs do record that root explicitly; both shapes are durable and
            // the next descendant is one level below either parent.
            return Ok(depth);
        };
        current = parent;
        depth += 1;
    }
    Ok(depth)
}

fn descendants_including(records: &[AgentDispatchRecord], root: &AgentId) -> Vec<AgentId> {
    let mut result = vec![root.clone()];
    let mut cursor = 0;
    while cursor < result.len() {
        let parent = result[cursor].clone();
        result.extend(
            records
                .iter()
                .filter(|record| record.parent_agent_id.as_ref() == Some(&parent))
                .map(|record| record.handle.agent_id.clone()),
        );
        cursor += 1;
    }
    result
}

fn poisoned() -> AgentSupervisorError {
    AgentSupervisorError::Poisoned
}

#[derive(Debug)]
pub enum AgentSupervisorError {
    InvalidConfig(String),
    InvalidHistory(String),
    QueueLimit,
    RunAgentLimit,
    ParentChildLimit(AgentId),
    DepthLimit {
        depth: usize,
    },
    UnknownAgent(AgentId),
    MessageSenderMismatch {
        recipient: AgentId,
        sender: AgentId,
    },
    MessageTargetNotLive {
        agent_id: AgentId,
        state: DispatchState,
    },
    FollowupTargetBusy(AgentId),
    FollowupBudgetWidening,
    InputFactory(String),
    Dispatch(DispatchError),
    Loop(AgentLoopError),
    Mailbox(MailboxError),
    Message(AgentMessageError),
    RunLog(crate::run_log::RunLogError),
    Poisoned,
}

impl From<DispatchError> for AgentSupervisorError {
    fn from(value: DispatchError) -> Self {
        Self::Dispatch(value)
    }
}

impl From<AgentLoopError> for AgentSupervisorError {
    fn from(value: AgentLoopError) -> Self {
        Self::Loop(value)
    }
}

impl From<MailboxError> for AgentSupervisorError {
    fn from(value: MailboxError) -> Self {
        Self::Mailbox(value)
    }
}

impl From<AgentMessageError> for AgentSupervisorError {
    fn from(value: AgentMessageError) -> Self {
        Self::Message(value)
    }
}

impl From<crate::run_log::RunLogError> for AgentSupervisorError {
    fn from(value: crate::run_log::RunLogError) -> Self {
        Self::RunLog(value)
    }
}

impl fmt::Display for AgentSupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(detail) => write!(formatter, "invalid supervisor config: {detail}"),
            Self::InvalidHistory(detail) => {
                write!(formatter, "invalid supervisor history: {detail}")
            }
            Self::QueueLimit => formatter.write_str("agent ready queue is full"),
            Self::RunAgentLimit => formatter.write_str("root run agent budget is exhausted"),
            Self::ParentChildLimit(parent) => {
                write!(formatter, "agent {parent} child budget is exhausted")
            }
            Self::DepthLimit { depth } => write!(
                formatter,
                "agent depth {depth} exceeds the configured limit"
            ),
            Self::UnknownAgent(agent) => write!(formatter, "agent {agent} is unknown"),
            Self::MessageSenderMismatch { recipient, sender } => write!(
                formatter,
                "agent {sender} is not the parent authorized to message child {recipient}"
            ),
            Self::MessageTargetNotLive { agent_id, state } => write!(
                formatter,
                "agent {agent_id} cannot receive a message while in state {state:?}"
            ),
            Self::FollowupTargetBusy(agent_id) => {
                write!(
                    formatter,
                    "agent {agent_id} has not reached an idle boundary"
                )
            }
            Self::FollowupBudgetWidening => formatter
                .write_str("follow-up budget cannot exceed the child turn's inherited budget"),
            Self::InputFactory(detail) => {
                write!(formatter, "could not build child loop input: {detail}")
            }
            Self::Dispatch(error) => write!(formatter, "agent scheduler: {error}"),
            Self::Loop(error) => write!(formatter, "agent loop: {error}"),
            Self::Mailbox(error) => write!(formatter, "agent mailbox: {error}"),
            Self::Message(error) => write!(formatter, "agent message: {error}"),
            Self::RunLog(error) => write!(formatter, "run log: {error}"),
            Self::Poisoned => formatter.write_str("agent supervisor lock was poisoned"),
        }
    }
}

impl std::error::Error for AgentSupervisorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{
        catalog_model_id, fake_provider::FakeProviderAdapter, AgentKind, AgentKindName,
        DelegationEnvelope, DenyAllAgentApprovals, ReasoningEffort, RequestedModelRoute,
        WorkspaceAssignment, WorkspaceStrategy,
    };
    use crate::ai::AiConfig;
    use crate::run_log::{
        EventActor, EventKind, InMemoryRunEventSink, MessageEvent, MessageRole, NewRunEvent,
        RunEventSink, TurnId, WorkspaceId,
    };
    use std::sync::atomic::Ordering;

    struct UnusedExecutor;

    impl AgentToolExecutor for UnusedExecutor {
        fn execute(
            &self,
            _call: super::super::AgentToolCall,
        ) -> super::super::AgentFuture<
            '_,
            Result<super::super::AgentToolResult, super::super::AgentToolError>,
        > {
            Box::pin(async { Err(super::super::AgentToolError::new("unexpected tool request")) })
        }
    }

    struct FakeInputFactory {
        out_of_order: FakeProviderAdapter,
        delayed: FakeProviderAdapter,
        failure: FakeProviderAdapter,
        timeout: FakeProviderAdapter,
    }

    impl FakeInputFactory {
        fn new(tick: Duration) -> Self {
            Self {
                out_of_order: FakeProviderAdapter::new("out_of_order_completion")
                    .with_tick_duration(tick),
                delayed: FakeProviderAdapter::new("delayed_completion").with_tick_duration(tick),
                failure: FakeProviderAdapter::new("tool_failure").with_tick_duration(tick),
                timeout: FakeProviderAdapter::new("timeout").with_tick_duration(tick),
            }
        }
    }

    impl AgentLoopInputFactory for FakeInputFactory {
        fn build(&self, dispatch: &AgentDispatchRecord) -> Result<AgentLoopDependencies, String> {
            let provider: Arc<dyn AgentProviderAdapter> = if dispatch.objective.contains("fast") {
                Arc::new(self.out_of_order.clone().for_call("fast-second"))
            } else if dispatch.objective.contains("slow") {
                Arc::new(self.out_of_order.clone().for_call("slow-first"))
            } else if dispatch.objective.contains("fail") {
                Arc::new(self.failure.clone())
            } else if dispatch.objective.contains("timeout") {
                Arc::new(self.timeout.clone())
            } else {
                Arc::new(self.delayed.clone())
            };
            Ok(AgentLoopDependencies {
                provider,
                tool_view: ScopedToolView::default(),
                tool_executor: Arc::new(UnusedExecutor),
                approval_client: Arc::new(DenyAllAgentApprovals),
                workspace: AgentWorkspaceDescriptor {
                    assignment: dispatch.handle.workspace.clone(),
                    root: None,
                    read_only: true,
                    warnings: Vec::new(),
                },
                envelope: DelegationEnvelope::objective(dispatch.objective.clone()),
                budget: None,
            })
        }
    }

    fn catalog() -> Arc<SubagentModelCatalog> {
        Arc::new(SubagentModelCatalog::from_config(&AiConfig::default()).unwrap())
    }

    fn request(objective: &str) -> DispatchRequest {
        DispatchRequest {
            task_name: format!("task_{}", objective.replace(' ', "_")),
            objective: objective.into(),
            role: AgentKind::built_in(AgentKindName::Explorer),
            requested_route: RequestedModelRoute::exact(
                catalog_model_id("local", "qwen2.5-coder:7b"),
                ReasoningEffort::none(),
            ),
            parent_agent_id: None,
            causing_turn_id: None,
            caused_by_event: None,
            workspace: WorkspaceAssignment {
                workspace_id: WorkspaceId::new(),
                strategy: WorkspaceStrategy::ReadOnlySnapshot { manifest_id: None },
            },
        }
    }

    fn supervisor_with(
        sink: Arc<InMemoryRunEventSink>,
        factory: Arc<FakeInputFactory>,
        config: AgentSupervisorConfig,
    ) -> (AgentSupervisor, RunId, AgentId) {
        let run_id = RunId::new();
        let root = AgentId::new();
        let supervisor = AgentSupervisor::new(
            run_id.clone(),
            root.clone(),
            sink,
            catalog(),
            factory,
            config,
        )
        .unwrap();
        (supervisor, run_id, root)
    }

    fn parent_cause(sink: &dyn RunEventSink, run_id: &RunId, root: &AgentId) -> (TurnId, EventId) {
        let turn_id = TurnId::new();
        let event = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: None,
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::Agent(root.clone()),
                agent_id: Some(root.clone()),
                turn_id: Some(turn_id.clone()),
                workspace_id: None,
                branch_id: None,
                kind: EventKind::Message(MessageEvent {
                    role: MessageRole::Agent,
                    content: "message child".into(),
                }),
            })
            .unwrap();
        (turn_id, event.event_id)
    }

    /// OV-00318: when the run log rejects appends (e.g. a full disk), a failed
    /// child's terminalization used to be silently discarded, leaving the
    /// dispatch non-terminal for the process lifetime. It must instead be
    /// parked and retried, so the first wait after the sink heals settles the
    /// dispatch and delivers the handoff to the parent.
    #[tokio::test(flavor = "multi_thread")]
    async fn failed_terminalization_is_retried_after_sink_heals() {
        use crate::agent_runtime::test_support::FlakySink;

        let inner = Arc::new(InMemoryRunEventSink::new());
        let flaky = Arc::new(FlakySink::new(inner));
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(150)));
        let run_id = RunId::new();
        let root = AgentId::new();
        let supervisor = AgentSupervisor::new(
            run_id,
            root.clone(),
            flaky.clone(),
            catalog(),
            factory,
            AgentSupervisorConfig::default(),
        )
        .unwrap();

        let child = supervisor
            .dispatch_nonblocking(request("slow child"))
            .unwrap();
        while supervisor.state(&child.agent_id).unwrap() != Some(DispatchState::Running) {
            tokio::task::yield_now().await;
        }

        // Every append now fails: the child loop errors, and recording the
        // failure durably fails too — the terminalization must be parked.
        flaky.fail_all();
        assert!(
            !supervisor
                .wait_for_idle(Duration::from_millis(600))
                .await
                .unwrap(),
            "run must not report idle while the failure is unrecorded"
        );

        // Space is freed: the next wait settles the parked terminalization,
        // the dispatch reaches a terminal state, and the parent is notified.
        flaky.heal();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(5))
            .await
            .unwrap());
        let state = supervisor.state(&child.agent_id).unwrap().unwrap();
        assert!(
            state.is_terminal(),
            "dispatch must be terminal after the retry; got {state:?}"
        );
        assert_eq!(
            supervisor.mailbox(root).unwrap().pending().unwrap().len(),
            1,
            "the parent must receive the terminal handoff notification"
        );
    }

    #[tokio::test]
    async fn runs_three_children_concurrently_and_finishes_out_of_order() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(15)));
        let maximum_active = factory.out_of_order.maximum_active_counter();
        let (supervisor, run_id, root) =
            supervisor_with(sink.clone(), factory, AgentSupervisorConfig::default());
        let slow = supervisor.dispatch(request("slow one")).await.unwrap();
        let fast = supervisor.dispatch(request("fast two")).await.unwrap();
        let slow_three = supervisor.dispatch(request("slow three")).await.unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(3))
            .await
            .unwrap());

        assert!(maximum_active.load(Ordering::Acquire) >= 3);
        for handle in [&slow, &fast, &slow_three] {
            assert_eq!(
                supervisor.state(&handle.agent_id).unwrap(),
                Some(DispatchState::Completed)
            );
        }
        let terminal_sequence = |agent_id: &AgentId| {
            sink.events(&run_id)
                .unwrap()
                .into_iter()
                .find(|event| {
                    event.agent_id.as_ref() == Some(agent_id)
                        && matches!(
                            event.kind,
                            EventKind::AgentLifecycle(ref lifecycle)
                                if lifecycle.state == crate::run_log::AgentLifecycleState::Completed
                        )
                })
                .unwrap()
                .sequence
        };
        assert!(terminal_sequence(&fast.agent_id) < terminal_sequence(&slow.agent_id));
        assert_eq!(
            supervisor.mailbox(root).unwrap().pending().unwrap().len(),
            3
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn waiting_parent_yields_the_only_concurrency_slot_to_its_child() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(100)));
        let config = AgentSupervisorConfig {
            max_concurrent: 1,
            max_depth: 2,
            ..AgentSupervisorConfig::default()
        };
        let (supervisor, run_id, _root) = supervisor_with(sink.clone(), factory, config);
        let parent = supervisor
            .dispatch_nonblocking(request("slow parent"))
            .unwrap();
        while supervisor.state(&parent.agent_id).unwrap() != Some(DispatchState::Running) {
            tokio::task::yield_now().await;
        }

        let turn_id = TurnId::new();
        let event_id = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: None,
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::Agent(parent.agent_id.clone()),
                agent_id: Some(parent.agent_id.clone()),
                turn_id: Some(turn_id.clone()),
                workspace_id: Some(parent.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::Message(MessageEvent {
                    role: MessageRole::Agent,
                    content: "delegate child".into(),
                }),
            })
            .unwrap()
            .event_id;
        let mut child_request = request("fast child");
        child_request.parent_agent_id = Some(parent.agent_id.clone());
        child_request.causing_turn_id = Some(turn_id);
        child_request.caused_by_event = Some(event_id);
        let child = supervisor.dispatch_nonblocking(child_request).unwrap();
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Queued)
        );

        let outcome = supervisor
            .wait_for_agent_updates(&parent, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(matches!(outcome, MailboxWaitOutcome::Updates(ref items) if !items.is_empty()));
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Completed)
        );
    }

    #[tokio::test]
    async fn parent_message_delivers_once_at_a_provider_boundary_and_completed_target_rejects() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(30)));
        let (supervisor, run_id, root) =
            supervisor_with(sink.clone(), factory, AgentSupervisorConfig::default());
        let (turn_id, caused_by_event) = parent_cause(&*sink, &run_id, &root);
        let child = supervisor
            .dispatch_nonblocking(request("message boundary"))
            .unwrap();
        loop {
            if matches!(
                supervisor.state(&child.agent_id).unwrap(),
                Some(DispatchState::Starting | DispatchState::Running)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let message = supervisor
            .send_message(SendAgentMessageRequest {
                sender_agent_id: root.clone(),
                recipient_agent_id: child.agent_id.clone(),
                causing_turn_id: turn_id.clone(),
                caused_by_event: caused_by_event.clone(),
                content: "Also inspect the restart edge.".into(),
            })
            .unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());
        let projection = supervisor
            .message_queue(child.agent_id.clone())
            .unwrap()
            .projection()
            .unwrap();
        assert!(matches!(
            projection.message(&message.message_event_id).unwrap().state,
            super::super::AgentMessageState::Delivered { .. }
        ));
        assert!(projection
            .message(&message.message_event_id)
            .unwrap()
            .consumption_event_id
            .is_some());
        assert!(matches!(
            supervisor.send_message(SendAgentMessageRequest {
                sender_agent_id: root,
                recipient_agent_id: child.agent_id,
                causing_turn_id: turn_id,
                caused_by_event,
                content: "too late".into(),
            }),
            Err(AgentSupervisorError::MessageTargetNotLive {
                state: DispatchState::Completed,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn steering_to_a_terminating_child_is_never_left_durably_queued() {
        // Queue steering to a live child, then interrupt it. However the child
        // terminates — whether it consumed the message at a boundary first or
        // was cancelled while parked — the terminal path must settle the message
        // (delivered or rejected). It must never be left durably queued, which
        // is the orphaning A3 addresses.
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_secs(10)));
        let (supervisor, run_id, root) =
            supervisor_with(sink.clone(), factory, AgentSupervisorConfig::default());
        let (turn_id, caused_by_event) = parent_cause(&*sink, &run_id, &root);
        let child = supervisor
            .dispatch_nonblocking(request("message boundary"))
            .unwrap();
        loop {
            if matches!(
                supervisor.state(&child.agent_id).unwrap(),
                Some(
                    DispatchState::Starting
                        | DispatchState::Running
                        | DispatchState::WaitingForAgent
                        | DispatchState::WaitingForTool
                        | DispatchState::WaitingForUser
                )
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let message = supervisor
            .send_message(SendAgentMessageRequest {
                sender_agent_id: root.clone(),
                recipient_agent_id: child.agent_id.clone(),
                causing_turn_id: turn_id,
                caused_by_event,
                content: "steer before the child reaches a boundary".into(),
            })
            .unwrap();
        // send_message durably enqueues the steering.
        assert!(matches!(
            message.state,
            super::super::AgentMessageState::Queued
        ));
        supervisor.interrupt(&child.agent_id, "stop").await.unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());
        // The steering is settled, never left durably queued.
        assert!(supervisor
            .message_queue(child.agent_id.clone())
            .unwrap()
            .projection()
            .unwrap()
            .message(&message.message_event_id)
            .unwrap()
            .state
            .is_terminal());
    }

    #[tokio::test]
    async fn followup_reuses_proven_idle_session_on_same_agent_with_fresh_budget() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(2)));
        let (supervisor, run_id, root) =
            supervisor_with(sink.clone(), factory, AgentSupervisorConfig::default());
        let child = supervisor
            .dispatch(request("follow up once"))
            .await
            .unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Completed)
        );
        let (turn_id, caused_by_event) = parent_cause(&*sink, &run_id, &root);
        let followup = supervisor
            .followup_agent(FollowupAgentRequest {
                agent_id: child.agent_id.clone(),
                parent_agent_id: root.clone(),
                causing_turn_id: turn_id,
                caused_by_event,
                objective: "Now inspect the restart edge.".into(),
                capabilities: Some(BTreeSet::from([super::super::AgentCapability::Read])),
                budget: AgentLoopBudget {
                    timeout: Duration::from_secs(30),
                    max_provider_events: 32,
                    max_tool_calls: 8,
                },
                retained_session_requested: false,
            })
            .await
            .unwrap();
        assert_eq!(followup.handle.agent_id, child.agent_id);
        assert_eq!(followup.turn_generation, 1);
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        let record = supervisor
            .dispatches()
            .unwrap()
            .into_iter()
            .find(|record| record.handle.agent_id == child.agent_id)
            .unwrap();
        assert_eq!(record.state, DispatchState::Completed);
        assert_eq!(record.turn_generation, 1);
        assert_eq!(record.handle.workspace, child.workspace);
        assert_eq!(record.followup.unwrap().budget.max_tool_calls, 8);
        let followup_event = sink
            .events(&run_id)
            .unwrap()
            .into_iter()
            .find_map(|event| match event.kind {
                EventKind::AgentFollowup(followup) => Some(followup),
                _ => None,
            })
            .unwrap();
        assert!(followup_event.retained_session_requested);
        assert_eq!(
            supervisor.mailbox(root).unwrap().pending().unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn failure_and_timeout_are_independent_and_durably_mailed() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(2)));
        let (supervisor, _, root) =
            supervisor_with(sink, factory, AgentSupervisorConfig::default());
        let failed = supervisor
            .dispatch(request("fail independently"))
            .await
            .unwrap();
        let timed_out = supervisor
            .dispatch(request("timeout independently"))
            .await
            .unwrap();
        let completed = supervisor
            .dispatch(request("complete independently"))
            .await
            .unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());

        assert_eq!(
            supervisor.state(&failed.agent_id).unwrap(),
            Some(DispatchState::Failed)
        );
        assert_eq!(
            supervisor.state(&timed_out.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        assert_eq!(
            supervisor.state(&completed.agent_id).unwrap(),
            Some(DispatchState::Completed)
        );
        let entries = supervisor.mailbox(root).unwrap().pending().unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|entry| matches!(
            &entry.notification,
            MailboxNotificationKind::Handoff { handoff, .. }
                if handoff.status == HandoffStatus::Failed
        )));
        assert!(entries.iter().any(|entry| matches!(
            &entry.notification,
            MailboxNotificationKind::Handoff { handoff, .. }
                if handoff.status == HandoffStatus::TimedOut
        )));
    }

    #[tokio::test]
    async fn malformed_handoff_never_creates_completed_lifecycle() {
        struct MalformedFactory(FakeProviderAdapter);
        impl AgentLoopInputFactory for MalformedFactory {
            fn build(
                &self,
                dispatch: &AgentDispatchRecord,
            ) -> Result<AgentLoopDependencies, String> {
                Ok(AgentLoopDependencies {
                    provider: Arc::new(self.0.clone()),
                    tool_view: ScopedToolView::default(),
                    tool_executor: Arc::new(UnusedExecutor),
                    approval_client: Arc::new(DenyAllAgentApprovals),
                    workspace: AgentWorkspaceDescriptor {
                        assignment: dispatch.handle.workspace.clone(),
                        root: None,
                        read_only: true,
                        warnings: Vec::new(),
                    },
                    envelope: DelegationEnvelope::objective(dispatch.objective.clone()),
                    budget: None,
                })
            }
        }
        let sink = Arc::new(InMemoryRunEventSink::new());
        let run_id = RunId::new();
        let root = AgentId::new();
        let supervisor = AgentSupervisor::new(
            run_id.clone(),
            root,
            sink.clone(),
            catalog(),
            Arc::new(MalformedFactory(FakeProviderAdapter::new(
                "malformed_handoff",
            ))),
            AgentSupervisorConfig::default(),
        )
        .unwrap();
        let child = supervisor.dispatch(request("malformed")).await.unwrap();
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Failed)
        );
        assert!(!sink.events(&run_id).unwrap().iter().any(|event| matches!(
            event.kind,
            EventKind::AgentLifecycle(ref lifecycle)
                if lifecycle.agent_id == child.agent_id
                    && lifecycle.state == crate::run_log::AgentLifecycleState::Completed
        )));
    }

    #[tokio::test]
    async fn queue_and_concurrency_limits_fail_before_allocating_extra_agent() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(30)));
        let maximum_active = factory.delayed.maximum_active_counter();
        let config = AgentSupervisorConfig {
            max_concurrent: 1,
            max_queued: 1,
            ..AgentSupervisorConfig::default()
        };
        let (supervisor, _, _) = supervisor_with(sink, factory, config);
        supervisor.dispatch(request("first")).await.unwrap();
        supervisor.dispatch(request("second")).await.unwrap();
        assert!(matches!(
            supervisor.dispatch(request("third")).await,
            Err(AgentSupervisorError::QueueLimit)
        ));
        assert_eq!(supervisor.dispatches().unwrap().len(), 2);
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());
        assert_eq!(maximum_active.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn root_and_child_budgets_terminalize_with_validated_partial_handoffs() {
        let root_sink = Arc::new(InMemoryRunEventSink::new());
        let root_factory = Arc::new(FakeInputFactory::new(Duration::from_millis(2)));
        let root_config = AgentSupervisorConfig {
            max_concurrent: 1,
            root_max_provider_events: 2,
            ..AgentSupervisorConfig::default()
        };
        let (root_limited, _, root_recipient) =
            supervisor_with(root_sink, root_factory, root_config);
        let first = root_limited
            .dispatch(request("first budget"))
            .await
            .unwrap();
        let second = root_limited
            .dispatch(request("second budget"))
            .await
            .unwrap();
        assert!(root_limited
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        assert_eq!(
            root_limited.state(&first.agent_id).unwrap(),
            Some(DispatchState::Completed)
        );
        assert_eq!(
            root_limited.state(&second.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        assert_eq!(
            root_limited
                .mailbox(root_recipient)
                .unwrap()
                .pending()
                .unwrap()
                .len(),
            2
        );

        let child_sink = Arc::new(InMemoryRunEventSink::new());
        let child_factory = Arc::new(FakeInputFactory::new(Duration::from_millis(20)));
        let child_config = AgentSupervisorConfig {
            child_budget: AgentLoopBudget {
                timeout: Duration::from_millis(5),
                max_provider_events: 1,
                max_tool_calls: 1,
            },
            ..AgentSupervisorConfig::default()
        };
        let (child_limited, _, child_recipient) =
            supervisor_with(child_sink, child_factory, child_config);
        let child = child_limited
            .dispatch(request("child budget"))
            .await
            .unwrap();
        assert!(child_limited
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        assert_eq!(
            child_limited.state(&child.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        let entries = child_limited
            .mailbox(child_recipient)
            .unwrap()
            .pending()
            .unwrap();
        assert!(matches!(
            &entries[0].notification,
            MailboxNotificationKind::Handoff { handoff, .. }
                if matches!(handoff.status, HandoffStatus::Interrupted | HandoffStatus::TimedOut)
        ));
    }

    #[tokio::test]
    async fn parent_interrupt_cascades_to_live_descendant() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(40)));
        let config = AgentSupervisorConfig {
            max_depth: 1,
            ..AgentSupervisorConfig::default()
        };
        let (supervisor, run_id, _) = supervisor_with(sink.clone(), factory, config);
        let mut parent_request = request("parent delayed");
        parent_request.role = AgentKind::built_in(AgentKindName::Planner);
        let parent = supervisor.dispatch(parent_request).await.unwrap();
        let turn_id = TurnId::new();
        let last_parent = sink
            .events(&run_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| event.agent_id.as_ref() == Some(&parent.agent_id))
            .unwrap();
        let cause = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: Some(last_parent.event_id),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::Agent(parent.agent_id.clone()),
                agent_id: Some(parent.agent_id.clone()),
                turn_id: Some(turn_id.clone()),
                workspace_id: Some(parent.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::Message(MessageEvent {
                    role: MessageRole::Agent,
                    content: "dispatch child".into(),
                }),
            })
            .unwrap();
        let mut child_request = request("child delayed");
        child_request.parent_agent_id = Some(parent.agent_id.clone());
        child_request.causing_turn_id = Some(turn_id);
        child_request.caused_by_event = Some(cause.event_id);
        let child = supervisor.dispatch(child_request).await.unwrap();
        while supervisor.live_handles().await.len() < 2 {
            tokio::task::yield_now().await;
        }
        let interrupted = supervisor
            .interrupt(&parent.agent_id, "stop subtree")
            .await
            .unwrap();
        assert_eq!(interrupted.len(), 2);
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());
        assert_eq!(
            supervisor.state(&parent.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
    }

    #[tokio::test]
    async fn parent_interrupt_terminalizes_a_not_yet_live_descendant() {
        // Saturate the single provider slot with the parent so its child stays
        // queued (not live), then interrupt the subtree. interrupt() must
        // terminalize the not-yet-live child as interrupted under the same
        // live->scheduler lock ordering as promotion, so that when the parent's
        // permit frees moments later, drain_ready does not promote and run it.
        // This guards the interrupt classification rewrite; the exact
        // Starting-but-not-live window needs a drain-boundary hook to hit
        // deterministically and is covered by reasoning, not this test.
        let sink = Arc::new(InMemoryRunEventSink::new());
        // A long per-tick duration pins the parent live for the whole test; the
        // loop runner cancels it via select!, so interrupt still lands promptly.
        let factory = Arc::new(FakeInputFactory::new(Duration::from_secs(10)));
        let config = AgentSupervisorConfig {
            max_depth: 1,
            max_concurrent: 1,
            ..AgentSupervisorConfig::default()
        };
        let (supervisor, run_id, _) = supervisor_with(sink.clone(), factory, config);
        let mut parent_request = request("parent delayed");
        parent_request.role = AgentKind::built_in(AgentKindName::Planner);
        let parent = supervisor.dispatch(parent_request).await.unwrap();
        while supervisor.live_handles().await.is_empty() {
            tokio::task::yield_now().await;
        }
        let turn_id = TurnId::new();
        let last_parent = sink
            .events(&run_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| event.agent_id.as_ref() == Some(&parent.agent_id))
            .unwrap();
        let cause = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: Some(last_parent.event_id),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::Agent(parent.agent_id.clone()),
                agent_id: Some(parent.agent_id.clone()),
                turn_id: Some(turn_id.clone()),
                workspace_id: Some(parent.workspace.workspace_id.clone()),
                branch_id: None,
                kind: EventKind::Message(MessageEvent {
                    role: MessageRole::Agent,
                    content: "dispatch child".into(),
                }),
            })
            .unwrap();
        let mut child_request = request("child delayed");
        child_request.parent_agent_id = Some(parent.agent_id.clone());
        child_request.causing_turn_id = Some(turn_id);
        child_request.caused_by_event = Some(cause.event_id);
        let child = supervisor.dispatch(child_request).await.unwrap();

        // The single provider slot is held by the parent, so the child is queued.
        assert_eq!(supervisor.live_handles().await.len(), 1);
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Queued)
        );

        let interrupted = supervisor
            .interrupt(&parent.agent_id, "stop subtree")
            .await
            .unwrap();
        assert_eq!(interrupted.len(), 2);
        assert!(supervisor
            .wait_for_idle(Duration::from_secs(2))
            .await
            .unwrap());
        assert_eq!(
            supervisor.state(&parent.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        // The child ends interrupted rather than being promoted-and-run once the
        // parent's permit freed, proving interrupt terminalized it in place.
        assert_eq!(
            supervisor.state(&child.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
    }

    #[tokio::test]
    async fn recovery_interrupts_in_flight_and_does_not_duplicate_existing_mail() {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let run_id = RunId::new();
        let root = AgentId::new();
        let mut scheduler = AgentDispatchScheduler::new(run_id.clone(), sink.clone(), catalog());
        let queued = scheduler.dispatch(request("queued recovery")).unwrap();
        let running = scheduler.dispatch(request("running recovery")).unwrap();
        scheduler
            .transition(&running, DispatchState::Starting, None)
            .unwrap();
        scheduler
            .transition(&running, DispatchState::Running, None)
            .unwrap();
        drop(scheduler);

        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(2)));
        let recovered = AgentSupervisor::rehydrate(
            run_id.clone(),
            root.clone(),
            sink.clone(),
            catalog(),
            factory.clone(),
            AgentSupervisorConfig::default(),
        )
        .unwrap();
        assert_eq!(
            recovered.state(&running.agent_id).unwrap(),
            Some(DispatchState::Interrupted)
        );
        assert_eq!(
            recovered
                .mailbox(root.clone())
                .unwrap()
                .pending()
                .unwrap()
                .len(),
            1
        );
        recovered.start_recovered().await.unwrap();
        assert!(recovered
            .wait_for_idle(Duration::from_secs(1))
            .await
            .unwrap());
        assert_eq!(
            recovered.state(&queued.agent_id).unwrap(),
            Some(DispatchState::Completed)
        );
        assert_eq!(
            recovered
                .mailbox(root.clone())
                .unwrap()
                .pending()
                .unwrap()
                .len(),
            2
        );

        drop(recovered);
        let reopened = AgentSupervisor::rehydrate(
            run_id,
            root.clone(),
            sink,
            catalog(),
            factory,
            AgentSupervisorConfig::default(),
        )
        .unwrap();
        assert_eq!(reopened.mailbox(root).unwrap().pending().unwrap().len(), 2);
    }

    // Regression: run_one must release its concurrency permit BEFORE spawning the
    // re-drain. When the permit was owned by the spawned task instead, it dropped
    // only after run_one returned, so on a multi-threaded runtime the re-drain
    // could observe a saturated semaphore, give up on its first try_acquire miss,
    // and leave the remaining children Queued with nothing scheduled to retry.
    //
    // This must run on the multi_thread flavor: under the current-thread runtime
    // the spawned drain cannot start until the owning task completes (which drops
    // the permit first), so the ordering bug is invisible there — which is why
    // every other test in this file passed while the defect was live.
    //
    // Honest scope: this test does NOT reproduce the defect on its own. Measured
    // against the unfixed code it passed 7/7, because the natural window between
    // `tokio::spawn(drain_ready_task)` and the permit drop is only a function
    // epilogue wide — the re-drain almost never gets scheduled inside it.
    //
    // The mechanism was confirmed separately by widening that window with a
    // single `yield_now().await` between the spawn and the permit drop:
    //   - unfixed + widened window: 3/3 stalled here with "never reached idle"
    //   - fixed   + widened window: 3/3 passed
    // So the ordering defect is real and this fix closes it, but in production it
    // needs an unlucky preemption in a very narrow window to bite.
    //
    // What this test therefore guards is the single-slot promotion chain itself
    // (every completion must promote the next queued child); the ordering is
    // covered by reasoning plus that experiment, matching the precedent set by
    // parent_interrupt_terminalizes_a_not_yet_live_descendant above.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn single_slot_drains_every_queued_child_after_each_completion() {
        const CHILDREN: usize = 12;

        let sink = Arc::new(InMemoryRunEventSink::new());
        let factory = Arc::new(FakeInputFactory::new(Duration::from_millis(1)));
        let config = AgentSupervisorConfig {
            // One slot is the point: every child after the first must be
            // promoted by the re-drain that follows a completion.
            max_concurrent: 1,
            max_queued: CHILDREN,
            max_children_per_parent: CHILDREN,
            max_total_per_run: CHILDREN,
            ..AgentSupervisorConfig::default()
        };
        let (supervisor, _run_id, _root) = supervisor_with(sink, factory, config);

        // The first dispatch takes the only slot; every later one is left Queued,
        // so each must be promoted by the re-drain that follows a completion.
        let mut dispatched = Vec::new();
        for index in 0..CHILDREN {
            dispatched.push(
                supervisor
                    .dispatch(request(&format!("queued child {index}")))
                    .await
                    .unwrap(),
            );
        }

        assert!(
            supervisor
                .wait_for_idle(Duration::from_secs(10))
                .await
                .unwrap(),
            "supervisor never reached idle: a queued child was not promoted after \
             a completion freed the single provider slot"
        );

        for handle in dispatched {
            let state = supervisor.state(&handle.agent_id).unwrap();
            assert!(
                state.as_ref().is_some_and(|state| state.is_terminal()),
                "child {:?} ended in {state:?}, expected a terminal state",
                handle.agent_id
            );
        }
    }
}
