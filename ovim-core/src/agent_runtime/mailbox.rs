//! Durable parent mailboxes with process-local async wakeups.
//!
//! Notifications remain pending in the run log until a consumption event is
//! appended. The watch channel is only a wakeup optimization: callers always
//! rebuild pending state from durable events, including when completion
//! happened before subscription.

use crate::run_log::{
    AgentId, AgentLifecycleState, AgentMessageDeliveryEvent, AgentMessageDeliveryState,
    AgentMessageEvent, EventActor, EventEnvelope, EventId, EventKind, MailboxConsumedEvent,
    MailboxNotificationEvent, MailboxNotificationKind, NewRunEvent, RunEventSink, RunId,
    RunLogError, TurnId, AGENT_MESSAGE_EVENT_VERSION, MAILBOX_EVENT_VERSION,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

const MAX_ATTENTION_REASON_BYTES: usize = 2 * 1024;
pub const MAX_AGENT_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_CONSUME_RETRIES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMessageState {
    Queued,
    Delivering {
        delivery_event_id: EventId,
        provider_session_id: String,
    },
    Delivered {
        delivery_event_id: EventId,
    },
    Rejected {
        delivery_event_id: EventId,
        detail: String,
    },
}

impl AgentMessageState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered { .. } | Self::Rejected { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMessageRecord {
    pub message_event_id: EventId,
    pub sequence: u64,
    pub recorded_at: String,
    pub sender_agent_id: AgentId,
    pub recipient_agent_id: AgentId,
    pub parent_event_id: EventId,
    pub content: String,
    pub notification_event_id: Option<EventId>,
    pub consumption_event_id: Option<EventId>,
    pub state: AgentMessageState,
}

/// Replayable outbound-message state. The projection deliberately treats a
/// durable `Started` event as ambiguous until a matching terminal delivery
/// event appears; recovery must reject it instead of invoking the provider a
/// second time.
#[derive(Clone, Debug, Default)]
pub struct AgentMessageProjection {
    messages: BTreeMap<EventId, AgentMessageRecord>,
}

impl AgentMessageProjection {
    pub fn rehydrate(events: &[EventEnvelope]) -> Result<Self, AgentMessageProjectionError> {
        let mut ordered: Vec<_> = events.iter().collect();
        ordered.sort_by_key(|event| event.sequence);
        let mut projection = Self::default();
        for event in ordered {
            match &event.kind {
                EventKind::AgentMessage(message) => {
                    if message.version != AGENT_MESSAGE_EVENT_VERSION {
                        return Err(AgentMessageProjectionError::UnsupportedVersion(
                            message.version,
                        ));
                    }
                    if event.agent_id.as_ref() != Some(&message.recipient_agent_id)
                        || event.caused_by.as_ref() != Some(&message.parent_event_id)
                        || event.actor != EventActor::Agent(message.sender_agent_id.clone())
                    {
                        return Err(AgentMessageProjectionError::InvalidEnvelope(
                            event.event_id.clone(),
                        ));
                    }
                    validate_message_content(&message.content).map_err(|detail| {
                        AgentMessageProjectionError::InvalidMessage {
                            event_id: event.event_id.clone(),
                            detail,
                        }
                    })?;
                    if projection.messages.contains_key(&event.event_id) {
                        return Err(AgentMessageProjectionError::DuplicateMessage(
                            event.event_id.clone(),
                        ));
                    }
                    projection.messages.insert(
                        event.event_id.clone(),
                        AgentMessageRecord {
                            message_event_id: event.event_id.clone(),
                            sequence: event.sequence,
                            recorded_at: event.recorded_at.clone(),
                            sender_agent_id: message.sender_agent_id.clone(),
                            recipient_agent_id: message.recipient_agent_id.clone(),
                            parent_event_id: message.parent_event_id.clone(),
                            content: message.content.clone(),
                            notification_event_id: None,
                            consumption_event_id: None,
                            state: AgentMessageState::Queued,
                        },
                    );
                }
                EventKind::AgentMessageDelivery(delivery) => {
                    projection.apply_delivery(event, delivery)?;
                }
                EventKind::MailboxNotification(notification) => {
                    let MailboxNotificationKind::Message {
                        message_event_id, ..
                    } = &notification.notification
                    else {
                        continue;
                    };
                    // Version-one mailboxes also use `Message` for ordinary
                    // chat events. They are outside this outbound projection.
                    let Some(message) = projection.messages.get_mut(message_event_id) else {
                        continue;
                    };
                    if notification.recipient_agent_id != message.recipient_agent_id
                        || message.notification_event_id.is_some()
                    {
                        return Err(AgentMessageProjectionError::InvalidNotification(
                            event.event_id.clone(),
                        ));
                    }
                    message.notification_event_id = Some(event.event_id.clone());
                }
                EventKind::MailboxConsumed(consumed) => {
                    let Some(message) = projection.messages.values_mut().find(|message| {
                        message.notification_event_id.as_ref()
                            == Some(&consumed.notification_event_id)
                    }) else {
                        continue;
                    };
                    message
                        .consumption_event_id
                        .get_or_insert_with(|| event.event_id.clone());
                }
                _ => {}
            }
        }
        Ok(projection)
    }

    pub fn messages(&self) -> impl Iterator<Item = &AgentMessageRecord> {
        self.messages.values()
    }

    pub fn message(&self, event_id: &EventId) -> Option<&AgentMessageRecord> {
        self.messages.get(event_id)
    }

    pub fn queued_for(&self, recipient: &AgentId) -> Vec<AgentMessageRecord> {
        self.messages
            .values()
            .filter(|message| {
                &message.recipient_agent_id == recipient
                    && message.notification_event_id.is_some()
                    && matches!(message.state, AgentMessageState::Queued)
            })
            .cloned()
            .collect()
    }

    fn apply_delivery(
        &mut self,
        envelope: &EventEnvelope,
        delivery: &AgentMessageDeliveryEvent,
    ) -> Result<(), AgentMessageProjectionError> {
        if delivery.version != AGENT_MESSAGE_EVENT_VERSION {
            return Err(AgentMessageProjectionError::UnsupportedVersion(
                delivery.version,
            ));
        }
        let message = self
            .messages
            .get_mut(&delivery.message_event_id)
            .ok_or_else(|| {
                AgentMessageProjectionError::DeliveryBeforeMessage(
                    delivery.message_event_id.clone(),
                )
            })?;
        if envelope.agent_id.as_ref() != Some(&message.recipient_agent_id) {
            return Err(AgentMessageProjectionError::InvalidEnvelope(
                envelope.event_id.clone(),
            ));
        }
        match (&message.state, &delivery.state) {
            (AgentMessageState::Queued, AgentMessageDeliveryState::Started) => {
                let session = delivery
                    .provider_session_id
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        AgentMessageProjectionError::InvalidDelivery(envelope.event_id.clone())
                    })?;
                if envelope.caused_by.as_ref() != Some(&message.message_event_id) {
                    return Err(AgentMessageProjectionError::InvalidDelivery(
                        envelope.event_id.clone(),
                    ));
                }
                message.state = AgentMessageState::Delivering {
                    delivery_event_id: envelope.event_id.clone(),
                    provider_session_id: session,
                };
            }
            (
                AgentMessageState::Delivering {
                    delivery_event_id, ..
                },
                AgentMessageDeliveryState::Delivered,
            ) => {
                if envelope.caused_by.as_ref() != Some(delivery_event_id) {
                    return Err(AgentMessageProjectionError::InvalidDelivery(
                        envelope.event_id.clone(),
                    ));
                }
                message.state = AgentMessageState::Delivered {
                    delivery_event_id: envelope.event_id.clone(),
                };
            }
            (AgentMessageState::Queued, AgentMessageDeliveryState::Rejected) => {
                if envelope.caused_by.as_ref() != Some(&message.message_event_id) {
                    return Err(AgentMessageProjectionError::InvalidDelivery(
                        envelope.event_id.clone(),
                    ));
                }
                message.state = AgentMessageState::Rejected {
                    delivery_event_id: envelope.event_id.clone(),
                    detail: required_delivery_detail(delivery, envelope)?,
                };
            }
            (
                AgentMessageState::Delivering {
                    delivery_event_id, ..
                },
                AgentMessageDeliveryState::Rejected,
            ) => {
                if envelope.caused_by.as_ref() != Some(delivery_event_id) {
                    return Err(AgentMessageProjectionError::InvalidDelivery(
                        envelope.event_id.clone(),
                    ));
                }
                message.state = AgentMessageState::Rejected {
                    delivery_event_id: envelope.event_id.clone(),
                    detail: required_delivery_detail(delivery, envelope)?,
                };
            }
            _ => {
                return Err(AgentMessageProjectionError::InvalidDelivery(
                    envelope.event_id.clone(),
                ))
            }
        }
        Ok(())
    }
}

fn required_delivery_detail(
    delivery: &AgentMessageDeliveryEvent,
    envelope: &EventEnvelope,
) -> Result<String, AgentMessageProjectionError> {
    delivery
        .detail
        .clone()
        .filter(|detail| !detail.trim().is_empty() && detail.len() <= MAX_ATTENTION_REASON_BYTES)
        .ok_or_else(|| AgentMessageProjectionError::InvalidDelivery(envelope.event_id.clone()))
}

fn validate_message_content(content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("message is empty".into());
    }
    if content.len() > MAX_AGENT_MESSAGE_BYTES {
        return Err(format!(
            "message exceeds the {MAX_AGENT_MESSAGE_BYTES}-byte limit"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxEntry {
    pub notification_event_id: EventId,
    pub sequence: u64,
    pub recorded_at: String,
    pub notification: MailboxNotificationKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedMailboxEntry {
    pub notification: MailboxEntry,
    pub consumption_event_id: EventId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxConsumeOutcome {
    Consumed(ConsumedMailboxEntry),
    AlreadyConsumed(ConsumedMailboxEntry),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxWaitOutcome {
    Updates(Vec<MailboxEntry>),
    TimedOut,
}

/// Deterministic replay of notification and consumption events for one agent.
#[derive(Clone, Debug)]
pub struct AgentMailboxProjection {
    recipient_agent_id: AgentId,
    notifications: BTreeMap<EventId, MailboxEntry>,
    consumed: BTreeMap<EventId, EventId>,
}

impl AgentMailboxProjection {
    pub fn rehydrate(
        recipient_agent_id: AgentId,
        events: &[EventEnvelope],
    ) -> Result<Self, MailboxProjectionError> {
        let mut projection = Self {
            recipient_agent_id,
            notifications: BTreeMap::new(),
            consumed: BTreeMap::new(),
        };
        let mut ordered: Vec<_> = events.iter().collect();
        ordered.sort_by_key(|event| event.sequence);
        for event in ordered {
            match &event.kind {
                EventKind::MailboxNotification(notification)
                    if notification.recipient_agent_id == projection.recipient_agent_id =>
                {
                    projection.apply_notification(event, notification)?;
                }
                EventKind::MailboxConsumed(consumed)
                    if consumed.recipient_agent_id == projection.recipient_agent_id =>
                {
                    projection.apply_consumption(event, consumed)?;
                }
                _ => {}
            }
        }
        Ok(projection)
    }

    pub fn recipient_agent_id(&self) -> &AgentId {
        &self.recipient_agent_id
    }

    pub fn pending(&self) -> Vec<MailboxEntry> {
        self.notifications
            .iter()
            .filter(|(event_id, _)| !self.consumed.contains_key(*event_id))
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    pub fn notification(&self, event_id: &EventId) -> Option<&MailboxEntry> {
        self.notifications.get(event_id)
    }

    pub fn consumed_entry(&self, event_id: &EventId) -> Option<ConsumedMailboxEntry> {
        Some(ConsumedMailboxEntry {
            notification: self.notifications.get(event_id)?.clone(),
            consumption_event_id: self.consumed.get(event_id)?.clone(),
        })
    }

    fn apply_notification(
        &mut self,
        envelope: &EventEnvelope,
        event: &MailboxNotificationEvent,
    ) -> Result<(), MailboxProjectionError> {
        if event.version != MAILBOX_EVENT_VERSION {
            return Err(MailboxProjectionError::UnsupportedVersion(event.version));
        }
        if envelope.agent_id.as_ref() != Some(&event.recipient_agent_id) {
            return Err(MailboxProjectionError::EnvelopeRecipientMismatch {
                event_id: envelope.event_id.clone(),
            });
        }
        if self.notifications.contains_key(&envelope.event_id) {
            return Err(MailboxProjectionError::DuplicateNotification(
                envelope.event_id.clone(),
            ));
        }
        self.notifications.insert(
            envelope.event_id.clone(),
            MailboxEntry {
                notification_event_id: envelope.event_id.clone(),
                sequence: envelope.sequence,
                recorded_at: envelope.recorded_at.clone(),
                notification: event.notification.clone(),
            },
        );
        Ok(())
    }

    fn apply_consumption(
        &mut self,
        envelope: &EventEnvelope,
        event: &MailboxConsumedEvent,
    ) -> Result<(), MailboxProjectionError> {
        if event.version != MAILBOX_EVENT_VERSION {
            return Err(MailboxProjectionError::UnsupportedVersion(event.version));
        }
        if envelope.agent_id.as_ref() != Some(&event.recipient_agent_id) {
            return Err(MailboxProjectionError::EnvelopeRecipientMismatch {
                event_id: envelope.event_id.clone(),
            });
        }
        if !self
            .notifications
            .contains_key(&event.notification_event_id)
        {
            return Err(MailboxProjectionError::ConsumptionBeforeNotification(
                event.notification_event_id.clone(),
            ));
        }
        // Replayed duplicate acknowledgements are harmless. The public consume
        // API prevents them, but treating old duplicates idempotently makes
        // projection recovery match delivery semantics.
        self.consumed
            .entry(event.notification_event_id.clone())
            .or_insert_with(|| envelope.event_id.clone());
        Ok(())
    }
}

/// One agent's mailbox. Clones share a wakeup revision and may safely wait on
/// different async tasks without polling the run log.
#[derive(Clone)]
pub struct AgentMailbox {
    run_id: RunId,
    recipient_agent_id: AgentId,
    sink: Arc<dyn RunEventSink>,
    wakeup: watch::Sender<u64>,
}

impl AgentMailbox {
    pub fn new(
        run_id: RunId,
        recipient_agent_id: AgentId,
        sink: Arc<dyn RunEventSink>,
    ) -> Result<Self, MailboxError> {
        let events = sink.events(&run_id)?;
        AgentMailboxProjection::rehydrate(recipient_agent_id.clone(), &events)?;
        let revision = events.last().map_or(0, |event| event.sequence);
        let (wakeup, _) = watch::channel(revision);
        Ok(Self {
            run_id,
            recipient_agent_id,
            sink,
            wakeup,
        })
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn recipient_agent_id(&self) -> &AgentId {
        &self.recipient_agent_id
    }

    pub fn projection(&self) -> Result<AgentMailboxProjection, MailboxError> {
        Ok(AgentMailboxProjection::rehydrate(
            self.recipient_agent_id.clone(),
            &self.sink.events(&self.run_id)?,
        )?)
    }

    pub fn pending(&self) -> Result<Vec<MailboxEntry>, MailboxError> {
        Ok(self.projection()?.pending())
    }

    /// Durably append before waking waiters. A crash after append but before
    /// wakeup is recovered by the subscriber's initial durable projection.
    pub fn notify(
        &self,
        notification: MailboxNotificationKind,
    ) -> Result<MailboxEntry, MailboxError> {
        let caused_by = self.validate_notification(&notification)?;
        let envelope = self.sink.append(NewRunEvent {
            run_id: self.run_id.clone(),
            caused_by: Some(caused_by),
            operation_id: None,
            provider_call_id: None,
            actor: EventActor::System("agent_mailbox".into()),
            agent_id: Some(self.recipient_agent_id.clone()),
            turn_id: None,
            workspace_id: None,
            branch_id: None,
            kind: EventKind::MailboxNotification(MailboxNotificationEvent {
                version: MAILBOX_EVENT_VERSION,
                recipient_agent_id: self.recipient_agent_id.clone(),
                notification,
            }),
        })?;
        self.wakeup.send_replace(envelope.sequence);
        let EventKind::MailboxNotification(recorded) = envelope.kind else {
            unreachable!("the appended event retains its kind")
        };
        Ok(MailboxEntry {
            notification_event_id: envelope.event_id,
            sequence: envelope.sequence,
            recorded_at: envelope.recorded_at,
            notification: recorded.notification,
        })
    }

    /// Consumption is idempotent by durable notification event ID. A guarded
    /// append closes the race between concurrent consumers sharing a sink.
    pub fn consume(
        &self,
        notification_event_id: &EventId,
    ) -> Result<MailboxConsumeOutcome, MailboxError> {
        for _ in 0..MAX_CONSUME_RETRIES {
            let events = self.sink.events(&self.run_id)?;
            let projection =
                AgentMailboxProjection::rehydrate(self.recipient_agent_id.clone(), &events)?;
            let Some(notification) = projection.notification(notification_event_id).cloned() else {
                return Err(MailboxError::UnknownNotification(
                    notification_event_id.clone(),
                ));
            };
            if let Some(consumed) = projection.consumed_entry(notification_event_id) {
                return Ok(MailboxConsumeOutcome::AlreadyConsumed(consumed));
            }
            let expected_last_sequence = events.last().map(|event| event.sequence);
            let append = self.sink.append_if_last(
                NewRunEvent {
                    run_id: self.run_id.clone(),
                    caused_by: Some(notification_event_id.clone()),
                    operation_id: None,
                    provider_call_id: None,
                    actor: EventActor::Agent(self.recipient_agent_id.clone()),
                    agent_id: Some(self.recipient_agent_id.clone()),
                    turn_id: None,
                    workspace_id: None,
                    branch_id: None,
                    kind: EventKind::MailboxConsumed(MailboxConsumedEvent {
                        version: MAILBOX_EVENT_VERSION,
                        recipient_agent_id: self.recipient_agent_id.clone(),
                        notification_event_id: notification_event_id.clone(),
                    }),
                },
                expected_last_sequence,
            );
            match append {
                Ok(consumed) => {
                    self.wakeup.send_replace(consumed.sequence);
                    return Ok(MailboxConsumeOutcome::Consumed(ConsumedMailboxEntry {
                        notification,
                        consumption_event_id: consumed.event_id,
                    }));
                }
                Err(RunLogError::SequenceConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(MailboxError::ConcurrentUpdateLimit)
    }

    /// Waits for an already-pending or newly appended notification until the
    /// bounded deadline. The watch subscription is created before the durable
    /// read, closing the usual completion-before-subscription race.
    pub async fn wait(&self, timeout: Duration) -> Result<MailboxWaitOutcome, MailboxError> {
        let mut receiver = self.wakeup.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            let pending = self.pending()?;
            if !pending.is_empty() {
                return Ok(MailboxWaitOutcome::Updates(pending));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(MailboxWaitOutcome::TimedOut);
            }
            if tokio::time::timeout_at(deadline, receiver.changed())
                .await
                .is_err()
            {
                return Ok(MailboxWaitOutcome::TimedOut);
            }
        }
    }

    /// Wait for notifications attributable to one exact descendant without
    /// consuming or being woken early by another child's pending handoff.
    pub async fn wait_for_agent(
        &self,
        source_agent_id: &AgentId,
        timeout: Duration,
    ) -> Result<MailboxWaitOutcome, MailboxError> {
        let mut receiver = self.wakeup.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            let pending = self
                .pending()?
                .into_iter()
                .filter(|entry| notification_source(&entry.notification) == Some(source_agent_id))
                .collect::<Vec<_>>();
            if !pending.is_empty() {
                return Ok(MailboxWaitOutcome::Updates(pending));
            }
            if Instant::now() >= deadline
                || tokio::time::timeout_at(deadline, receiver.changed())
                    .await
                    .is_err()
            {
                return Ok(MailboxWaitOutcome::TimedOut);
            }
        }
    }

    fn validate_notification(
        &self,
        notification: &MailboxNotificationKind,
    ) -> Result<EventId, MailboxError> {
        match notification {
            MailboxNotificationKind::Handoff {
                source_agent_id,
                terminal_event_id,
                handoff_event_id,
                handoff,
            } => {
                let recorded_handoff = self
                    .sink
                    .event(&self.run_id, handoff_event_id)?
                    .ok_or_else(|| {
                        MailboxError::InvalidNotification("handoff event is missing".into())
                    })?;
                if recorded_handoff.agent_id.as_ref() != Some(source_agent_id) {
                    return Err(MailboxError::InvalidNotification(
                        "handoff source does not match its event envelope".into(),
                    ));
                }
                let EventKind::AgentHandoff(recorded) = recorded_handoff.kind else {
                    return Err(MailboxError::InvalidNotification(
                        "handoff event ID does not name an agent handoff".into(),
                    ));
                };
                if &recorded
                    .handoff
                    .parent_projection_with_workspace_warnings(&recorded.workspace_warnings)
                    != handoff.as_ref()
                {
                    return Err(MailboxError::InvalidNotification(
                        "mailbox handoff projection does not match the validated artifact".into(),
                    ));
                }

                let terminal = self
                    .sink
                    .event(&self.run_id, terminal_event_id)?
                    .ok_or_else(|| {
                        MailboxError::InvalidNotification("terminal event is missing".into())
                    })?;
                if terminal.agent_id.as_ref() != Some(source_agent_id)
                    || terminal.caused_by.as_ref() != Some(handoff_event_id)
                {
                    return Err(MailboxError::InvalidNotification(
                        "terminal event is not causally linked to the handoff".into(),
                    ));
                }
                let EventKind::AgentLifecycle(lifecycle) = terminal.kind else {
                    return Err(MailboxError::InvalidNotification(
                        "terminal event ID does not name an agent lifecycle".into(),
                    ));
                };
                let status_matches = match lifecycle.state {
                    AgentLifecycleState::Completed => handoff.status.is_completed(),
                    AgentLifecycleState::Failed => handoff.status == super::HandoffStatus::Failed,
                    AgentLifecycleState::Interrupted => matches!(
                        handoff.status,
                        super::HandoffStatus::Interrupted | super::HandoffStatus::TimedOut
                    ),
                    _ => false,
                };
                if !status_matches {
                    return Err(MailboxError::InvalidNotification(
                        "terminal lifecycle does not match the handoff status".into(),
                    ));
                }
                Ok(terminal_event_id.clone())
            }
            MailboxNotificationKind::Message {
                sender_agent_id,
                message_event_id,
            } => {
                let message = self
                    .sink
                    .event(&self.run_id, message_event_id)?
                    .ok_or_else(|| {
                        MailboxError::InvalidNotification("message event is missing".into())
                    })?;
                match message.kind {
                    EventKind::AgentMessage(recorded)
                        if sender_agent_id.as_ref() == Some(&recorded.sender_agent_id)
                            && recorded.recipient_agent_id == self.recipient_agent_id
                            && message.agent_id.as_ref() == Some(&recorded.recipient_agent_id) => {}
                    EventKind::Message(_) if &message.agent_id == sender_agent_id => {}
                    _ => {
                        return Err(MailboxError::InvalidNotification(
                            "message notification does not match its event".into(),
                        ))
                    }
                }
                Ok(message_event_id.clone())
            }
            MailboxNotificationKind::Steering {
                turn_id,
                message_event_id,
            } => {
                let message = self
                    .sink
                    .event(&self.run_id, message_event_id)?
                    .ok_or_else(|| {
                        MailboxError::InvalidNotification("steering event is missing".into())
                    })?;
                if !matches!(message.kind, EventKind::Message(_))
                    || message.turn_id.as_ref() != Some(turn_id)
                {
                    return Err(MailboxError::InvalidNotification(
                        "steering notification does not match its message and turn".into(),
                    ));
                }
                Ok(message_event_id.clone())
            }
            MailboxNotificationKind::Attention {
                source_agent_id,
                reason,
            } => {
                if reason.trim().is_empty() || reason.len() > MAX_ATTENTION_REASON_BYTES {
                    return Err(MailboxError::InvalidNotification(
                        "attention reason is empty or oversized".into(),
                    ));
                }
                self.sink
                    .events(&self.run_id)?
                    .into_iter()
                    .rev()
                    .find(|event| event.agent_id.as_ref() == Some(source_agent_id))
                    .map(|event| event.event_id)
                    .ok_or_else(|| {
                        MailboxError::InvalidNotification(
                            "attention source has no event in this run".into(),
                        )
                    })
            }
        }
    }
}

fn notification_source(notification: &MailboxNotificationKind) -> Option<&AgentId> {
    match notification {
        MailboxNotificationKind::Handoff {
            source_agent_id, ..
        }
        | MailboxNotificationKind::Attention {
            source_agent_id, ..
        } => Some(source_agent_id),
        MailboxNotificationKind::Message {
            sender_agent_id, ..
        } => sender_agent_id.as_ref(),
        MailboxNotificationKind::Steering { .. } => None,
    }
}

/// Durable outbound queue for one child. Provider delivery is a two-event
/// claim/finish protocol so recovery never repeats a message whose effect may
/// already have crossed the provider boundary.
#[derive(Clone)]
pub struct AgentMessageQueue {
    run_id: RunId,
    recipient_agent_id: AgentId,
    sink: Arc<dyn RunEventSink>,
    mailbox: AgentMailbox,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMessageClaimOutcome {
    Claimed {
        message: AgentMessageRecord,
        delivery_event_id: EventId,
    },
    AlreadyClaimed(AgentMessageRecord),
    Terminal(AgentMessageRecord),
}

impl AgentMessageQueue {
    pub fn new(
        run_id: RunId,
        recipient_agent_id: AgentId,
        sink: Arc<dyn RunEventSink>,
        mailbox: AgentMailbox,
    ) -> Result<Self, AgentMessageError> {
        if mailbox.run_id() != &run_id || mailbox.recipient_agent_id() != &recipient_agent_id {
            return Err(AgentMessageError::InvalidQueueIdentity);
        }
        let queue = Self {
            run_id,
            recipient_agent_id,
            sink,
            mailbox,
        };
        queue.projection()?;
        Ok(queue)
    }

    pub fn projection(&self) -> Result<AgentMessageProjection, AgentMessageError> {
        Ok(AgentMessageProjection::rehydrate(
            &self.sink.events(&self.run_id)?,
        )?)
    }

    pub fn queue(
        &self,
        sender_agent_id: AgentId,
        turn_id: TurnId,
        parent_event_id: EventId,
        content: impl Into<String>,
    ) -> Result<AgentMessageRecord, AgentMessageError> {
        let content = content.into();
        validate_message_content(&content).map_err(AgentMessageError::InvalidMessage)?;
        let parent = self
            .sink
            .event(&self.run_id, &parent_event_id)?
            .ok_or_else(|| AgentMessageError::ParentEventMissing(parent_event_id.clone()))?;
        if parent.agent_id.as_ref() != Some(&sender_agent_id)
            || parent.turn_id.as_ref() != Some(&turn_id)
        {
            return Err(AgentMessageError::ParentEventMismatch(parent_event_id));
        }
        let message = self.sink.append(NewRunEvent {
            run_id: self.run_id.clone(),
            caused_by: Some(parent.event_id.clone()),
            operation_id: None,
            provider_call_id: None,
            actor: EventActor::Agent(sender_agent_id.clone()),
            agent_id: Some(self.recipient_agent_id.clone()),
            turn_id: Some(turn_id),
            workspace_id: None,
            branch_id: None,
            kind: EventKind::AgentMessage(AgentMessageEvent {
                version: AGENT_MESSAGE_EVENT_VERSION,
                sender_agent_id: sender_agent_id.clone(),
                recipient_agent_id: self.recipient_agent_id.clone(),
                parent_event_id: parent.event_id,
                content,
            }),
        })?;
        self.mailbox.notify(MailboxNotificationKind::Message {
            sender_agent_id: Some(sender_agent_id),
            message_event_id: message.event_id.clone(),
        })?;
        self.projection()?
            .message(&message.event_id)
            .cloned()
            .ok_or(AgentMessageError::ProjectionLostMessage(message.event_id))
    }

    pub fn queued(&self) -> Result<Vec<AgentMessageRecord>, AgentMessageError> {
        Ok(self.projection()?.queued_for(&self.recipient_agent_id))
    }

    pub fn unsettled(&self) -> Result<Vec<AgentMessageRecord>, AgentMessageError> {
        Ok(self
            .projection()?
            .messages()
            .filter(|message| {
                message.recipient_agent_id == self.recipient_agent_id
                    && !message.state.is_terminal()
            })
            .cloned()
            .collect())
    }

    /// Repair the only non-atomic enqueue edge (message event durable before
    /// notification), then conservatively reject any delivery that was not
    /// proven terminal. Delivered/rejected notifications are consumed again
    /// idempotently when a crash landed between status and acknowledgement.
    pub fn reconcile_after_restart(&self) -> Result<Vec<AgentMessageRecord>, AgentMessageError> {
        let projection = self.projection()?;
        for message in projection.messages().filter(|message| {
            message.recipient_agent_id == self.recipient_agent_id
                && message.notification_event_id.is_none()
        }) {
            self.mailbox.notify(MailboxNotificationKind::Message {
                sender_agent_id: Some(message.sender_agent_id.clone()),
                message_event_id: message.message_event_id.clone(),
            })?;
        }
        let messages = self
            .projection()?
            .messages()
            .filter(|message| message.recipient_agent_id == self.recipient_agent_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut reconciled = Vec::with_capacity(messages.len());
        for message in messages {
            reconciled.push(self.reject_queued(
                &message.message_event_id,
                "message was not safely deliverable after process restart",
            )?);
        }
        Ok(reconciled)
    }

    pub fn claim(
        &self,
        message_event_id: &EventId,
        provider_session_id: &str,
    ) -> Result<AgentMessageClaimOutcome, AgentMessageError> {
        if provider_session_id.trim().is_empty() {
            return Err(AgentMessageError::InvalidProviderSession);
        }
        for _ in 0..MAX_CONSUME_RETRIES {
            let events = self.sink.events(&self.run_id)?;
            let projection = AgentMessageProjection::rehydrate(&events)?;
            let message = projection
                .message(message_event_id)
                .cloned()
                .ok_or_else(|| AgentMessageError::UnknownMessage(message_event_id.clone()))?;
            match message.state {
                AgentMessageState::Queued => {}
                AgentMessageState::Delivering { .. } => {
                    return Ok(AgentMessageClaimOutcome::AlreadyClaimed(message))
                }
                AgentMessageState::Delivered { .. } | AgentMessageState::Rejected { .. } => {
                    return Ok(AgentMessageClaimOutcome::Terminal(message))
                }
            }
            let expected_last_sequence = events.last().map(|event| event.sequence);
            match self.sink.append_if_last(
                NewRunEvent {
                    run_id: self.run_id.clone(),
                    caused_by: Some(message_event_id.clone()),
                    operation_id: None,
                    provider_call_id: None,
                    actor: EventActor::System("agent_message_delivery".into()),
                    agent_id: Some(self.recipient_agent_id.clone()),
                    turn_id: None,
                    workspace_id: None,
                    branch_id: None,
                    kind: EventKind::AgentMessageDelivery(AgentMessageDeliveryEvent {
                        version: AGENT_MESSAGE_EVENT_VERSION,
                        message_event_id: message_event_id.clone(),
                        state: AgentMessageDeliveryState::Started,
                        provider_session_id: Some(provider_session_id.into()),
                        detail: None,
                    }),
                },
                expected_last_sequence,
            ) {
                Ok(delivery) => {
                    return Ok(AgentMessageClaimOutcome::Claimed {
                        message,
                        delivery_event_id: delivery.event_id,
                    })
                }
                Err(RunLogError::SequenceConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(AgentMessageError::ConcurrentUpdateLimit)
    }

    pub fn finish_delivery(
        &self,
        message_event_id: &EventId,
        delivery_event_id: &EventId,
        result: Result<(), String>,
    ) -> Result<AgentMessageRecord, AgentMessageError> {
        for _ in 0..MAX_CONSUME_RETRIES {
            let events = self.sink.events(&self.run_id)?;
            let projection = AgentMessageProjection::rehydrate(&events)?;
            let message = projection
                .message(message_event_id)
                .cloned()
                .ok_or_else(|| AgentMessageError::UnknownMessage(message_event_id.clone()))?;
            match &message.state {
                AgentMessageState::Delivering {
                    delivery_event_id: current,
                    ..
                } if current == delivery_event_id => {}
                AgentMessageState::Delivered { .. } | AgentMessageState::Rejected { .. } => {
                    self.consume_notification(&message)?;
                    return Ok(message);
                }
                _ => return Err(AgentMessageError::DeliveryClaimMismatch),
            }
            let (state, detail) = match &result {
                Ok(()) => (AgentMessageDeliveryState::Delivered, None),
                Err(detail) => {
                    let detail = bounded_delivery_detail(detail);
                    (AgentMessageDeliveryState::Rejected, Some(detail))
                }
            };
            let expected_last_sequence = events.last().map(|event| event.sequence);
            match self.sink.append_if_last(
                NewRunEvent {
                    run_id: self.run_id.clone(),
                    caused_by: Some(delivery_event_id.clone()),
                    operation_id: None,
                    provider_call_id: None,
                    actor: EventActor::System("agent_message_delivery".into()),
                    agent_id: Some(self.recipient_agent_id.clone()),
                    turn_id: None,
                    workspace_id: None,
                    branch_id: None,
                    kind: EventKind::AgentMessageDelivery(AgentMessageDeliveryEvent {
                        version: AGENT_MESSAGE_EVENT_VERSION,
                        message_event_id: message_event_id.clone(),
                        state,
                        provider_session_id: None,
                        detail,
                    }),
                },
                expected_last_sequence,
            ) {
                Ok(_) => {
                    let recorded = self
                        .projection()?
                        .message(message_event_id)
                        .cloned()
                        .ok_or_else(|| {
                            AgentMessageError::ProjectionLostMessage(message_event_id.clone())
                        })?;
                    self.consume_notification(&recorded)?;
                    return Ok(recorded);
                }
                Err(RunLogError::SequenceConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(AgentMessageError::ConcurrentUpdateLimit)
    }

    pub fn reject_queued(
        &self,
        message_event_id: &EventId,
        detail: impl AsRef<str>,
    ) -> Result<AgentMessageRecord, AgentMessageError> {
        let detail = bounded_delivery_detail(detail.as_ref());
        for _ in 0..MAX_CONSUME_RETRIES {
            let events = self.sink.events(&self.run_id)?;
            let projection = AgentMessageProjection::rehydrate(&events)?;
            let message = projection
                .message(message_event_id)
                .cloned()
                .ok_or_else(|| AgentMessageError::UnknownMessage(message_event_id.clone()))?;
            if message.state.is_terminal() {
                self.consume_notification(&message)?;
                return Ok(message);
            }
            let cause = match &message.state {
                AgentMessageState::Queued => message.message_event_id.clone(),
                AgentMessageState::Delivering {
                    delivery_event_id, ..
                } => delivery_event_id.clone(),
                _ => unreachable!("terminal state handled above"),
            };
            let expected_last_sequence = events.last().map(|event| event.sequence);
            match self.sink.append_if_last(
                NewRunEvent {
                    run_id: self.run_id.clone(),
                    caused_by: Some(cause),
                    operation_id: None,
                    provider_call_id: None,
                    actor: EventActor::System("agent_message_recovery".into()),
                    agent_id: Some(self.recipient_agent_id.clone()),
                    turn_id: None,
                    workspace_id: None,
                    branch_id: None,
                    kind: EventKind::AgentMessageDelivery(AgentMessageDeliveryEvent {
                        version: AGENT_MESSAGE_EVENT_VERSION,
                        message_event_id: message_event_id.clone(),
                        state: AgentMessageDeliveryState::Rejected,
                        provider_session_id: None,
                        detail: Some(detail.clone()),
                    }),
                },
                expected_last_sequence,
            ) {
                Ok(_) => {
                    let recorded = self
                        .projection()?
                        .message(message_event_id)
                        .cloned()
                        .ok_or_else(|| {
                            AgentMessageError::ProjectionLostMessage(message_event_id.clone())
                        })?;
                    self.consume_notification(&recorded)?;
                    return Ok(recorded);
                }
                Err(RunLogError::SequenceConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(AgentMessageError::ConcurrentUpdateLimit)
    }

    fn consume_notification(&self, message: &AgentMessageRecord) -> Result<(), AgentMessageError> {
        if message.consumption_event_id.is_some() {
            return Ok(());
        }
        if let Some(notification) = &message.notification_event_id {
            self.mailbox.consume(notification)?;
        }
        Ok(())
    }
}

fn bounded_delivery_detail(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        return "message delivery was rejected".into();
    }
    if detail.len() <= MAX_ATTENTION_REASON_BYTES {
        return detail.into();
    }
    let mut end = MAX_ATTENTION_REASON_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMessageProjectionError {
    UnsupportedVersion(u32),
    InvalidEnvelope(EventId),
    InvalidMessage { event_id: EventId, detail: String },
    DuplicateMessage(EventId),
    DeliveryBeforeMessage(EventId),
    NotificationBeforeMessage(EventId),
    InvalidDelivery(EventId),
    InvalidNotification(EventId),
}

impl fmt::Display for AgentMessageProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AgentMessageProjectionError {}

#[derive(Debug)]
pub enum AgentMessageError {
    Store(RunLogError),
    Mailbox(MailboxError),
    Projection(AgentMessageProjectionError),
    InvalidQueueIdentity,
    InvalidMessage(String),
    ParentEventMissing(EventId),
    ParentEventMismatch(EventId),
    UnknownMessage(EventId),
    ProjectionLostMessage(EventId),
    InvalidProviderSession,
    DeliveryClaimMismatch,
    ConcurrentUpdateLimit,
}

impl fmt::Display for AgentMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Mailbox(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
            Self::InvalidQueueIdentity => formatter.write_str("message queue identity mismatch"),
            Self::InvalidMessage(detail) => write!(formatter, "invalid agent message: {detail}"),
            Self::ParentEventMissing(event) => write!(formatter, "parent event {event} is missing"),
            Self::ParentEventMismatch(event) => {
                write!(
                    formatter,
                    "parent event {event} has the wrong agent or turn"
                )
            }
            Self::UnknownMessage(event) => write!(formatter, "agent message {event} is unknown"),
            Self::ProjectionLostMessage(event) => {
                write!(formatter, "agent message {event} disappeared during replay")
            }
            Self::InvalidProviderSession => formatter.write_str("provider session ID is empty"),
            Self::DeliveryClaimMismatch => formatter.write_str("message delivery claim mismatch"),
            Self::ConcurrentUpdateLimit => {
                formatter.write_str("message queue changed too often to update safely")
            }
        }
    }
}

impl std::error::Error for AgentMessageError {}

impl From<RunLogError> for AgentMessageError {
    fn from(error: RunLogError) -> Self {
        Self::Store(error)
    }
}

impl From<MailboxError> for AgentMessageError {
    fn from(error: MailboxError) -> Self {
        Self::Mailbox(error)
    }
}

impl From<AgentMessageProjectionError> for AgentMessageError {
    fn from(error: AgentMessageProjectionError) -> Self {
        Self::Projection(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxProjectionError {
    UnsupportedVersion(u32),
    EnvelopeRecipientMismatch { event_id: EventId },
    DuplicateNotification(EventId),
    ConsumptionBeforeNotification(EventId),
}

impl fmt::Display for MailboxProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported mailbox event version {version}")
            }
            Self::EnvelopeRecipientMismatch { event_id } => write!(
                formatter,
                "mailbox event {event_id} recipient disagrees with its envelope"
            ),
            Self::DuplicateNotification(event_id) => {
                write!(formatter, "mailbox notification {event_id} is duplicated")
            }
            Self::ConsumptionBeforeNotification(event_id) => write!(
                formatter,
                "mailbox notification {event_id} was consumed before it was recorded"
            ),
        }
    }
}

impl std::error::Error for MailboxProjectionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxError {
    Store(RunLogError),
    Projection(MailboxProjectionError),
    UnknownNotification(EventId),
    InvalidNotification(String),
    ConcurrentUpdateLimit,
}

impl fmt::Display for MailboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
            Self::UnknownNotification(event_id) => {
                write!(formatter, "mailbox notification {event_id} does not exist")
            }
            Self::InvalidNotification(detail) => {
                write!(formatter, "invalid mailbox notification: {detail}")
            }
            Self::ConcurrentUpdateLimit => {
                formatter.write_str("mailbox changed too often to record consumption")
            }
        }
    }
}

impl std::error::Error for MailboxError {}

impl From<RunLogError> for MailboxError {
    fn from(error: RunLogError) -> Self {
        Self::Store(error)
    }
}

impl From<MailboxProjectionError> for MailboxError {
    fn from(error: MailboxProjectionError) -> Self {
        Self::Projection(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{
        HandoffConfidence, HandoffEvidence, HandoffStatus, HandoffValidator, StructuredHandoffV1,
    };
    use crate::run_log::{
        AgentHandoffEvent, AgentLifecycleEvent, InMemoryRunEventSink, MessageEvent, MessageRole,
        SqliteRunEventSink, TurnId,
    };

    fn message(
        sink: &dyn RunEventSink,
        run_id: &RunId,
        sender: Option<AgentId>,
        turn_id: Option<TurnId>,
    ) -> EventEnvelope {
        sink.append(NewRunEvent {
            run_id: run_id.clone(),
            caused_by: None,
            operation_id: None,
            provider_call_id: None,
            actor: sender
                .clone()
                .map(EventActor::Agent)
                .unwrap_or(EventActor::User),
            agent_id: sender,
            turn_id,
            workspace_id: None,
            branch_id: None,
            kind: EventKind::Message(MessageEvent {
                role: MessageRole::Agent,
                content: "mailbox source".into(),
            }),
        })
        .unwrap()
    }

    fn mailbox() -> (AgentMailbox, Arc<InMemoryRunEventSink>, RunId, AgentId) {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let run_id = RunId::parse("run_mailbox_test").unwrap();
        let recipient = AgentId::parse("agt_mailbox_parent").unwrap();
        let mailbox = AgentMailbox::new(run_id.clone(), recipient.clone(), sink.clone()).unwrap();
        (mailbox, sink, run_id, recipient)
    }

    fn outbound_queue() -> (
        AgentMessageQueue,
        Arc<InMemoryRunEventSink>,
        RunId,
        AgentId,
        AgentId,
        TurnId,
        EventId,
    ) {
        let sink = Arc::new(InMemoryRunEventSink::new());
        let run_id = RunId::parse("run_message_queue_test").unwrap();
        let sender = AgentId::parse("agt_message_parent").unwrap();
        let recipient = AgentId::parse("agt_message_child").unwrap();
        let turn_id = TurnId::parse("trn_message_parent").unwrap();
        let parent = message(&*sink, &run_id, Some(sender.clone()), Some(turn_id.clone()));
        let mailbox = AgentMailbox::new(run_id.clone(), recipient.clone(), sink.clone()).unwrap();
        let queue =
            AgentMessageQueue::new(run_id.clone(), recipient.clone(), sink.clone(), mailbox)
                .unwrap();
        (
            queue,
            sink,
            run_id,
            sender,
            recipient,
            turn_id,
            parent.event_id,
        )
    }

    #[test]
    fn outbound_message_delivery_is_claimed_and_consumed_idempotently() {
        let (queue, sink, run_id, sender, _, turn, parent_event) = outbound_queue();
        let queued = queue
            .queue(sender, turn, parent_event, "Check the recovery edge too.")
            .unwrap();
        assert!(matches!(queued.state, AgentMessageState::Queued));
        assert_eq!(queue.queued().unwrap(), std::slice::from_ref(&queued));

        let claimed = queue
            .claim(&queued.message_event_id, "session-one")
            .unwrap();
        let AgentMessageClaimOutcome::Claimed {
            delivery_event_id, ..
        } = claimed
        else {
            panic!("first delivery should claim the durable message")
        };
        assert!(matches!(
            queue
                .claim(&queued.message_event_id, "session-one")
                .unwrap(),
            AgentMessageClaimOutcome::AlreadyClaimed(_)
        ));
        let delivered = queue
            .finish_delivery(&queued.message_event_id, &delivery_event_id, Ok(()))
            .unwrap();
        assert!(matches!(
            delivered.state,
            AgentMessageState::Delivered { .. }
        ));
        assert!(delivered.consumption_event_id.is_none());
        let event_count = sink.events(&run_id).unwrap().len();
        let delivered_again = queue
            .finish_delivery(&queued.message_event_id, &delivery_event_id, Ok(()))
            .unwrap();
        assert!(delivered_again.consumption_event_id.is_some());
        assert_eq!(sink.events(&run_id).unwrap().len(), event_count);
        assert!(queue.queued().unwrap().is_empty());
    }

    #[test]
    fn ambiguous_delivery_is_rejected_after_restart_instead_of_replayed() {
        let (queue, sink, run_id, sender, recipient, turn, parent_event) = outbound_queue();
        let queued = queue
            .queue(sender, turn, parent_event, "Do not duplicate me.")
            .unwrap();
        let AgentMessageClaimOutcome::Claimed { .. } = queue
            .claim(&queued.message_event_id, "session-before-crash")
            .unwrap()
        else {
            panic!("message was not claimed")
        };
        drop(queue);

        let mailbox = AgentMailbox::new(run_id.clone(), recipient.clone(), sink.clone()).unwrap();
        let reopened = AgentMessageQueue::new(run_id, recipient, sink, mailbox).unwrap();
        let rejected = reopened
            .reject_queued(
                &queued.message_event_id,
                "provider delivery outcome is ambiguous after restart",
            )
            .unwrap();
        assert!(matches!(
            rejected.state,
            AgentMessageState::Rejected { ref detail, .. }
                if detail.contains("ambiguous after restart")
        ));
        assert!(reopened.queued().unwrap().is_empty());
    }

    #[test]
    fn outbound_messages_enforce_parent_causality_and_bounds() {
        let (queue, _, _, sender, _, turn, parent_event) = outbound_queue();
        assert!(matches!(
            queue.queue(
                sender.clone(),
                TurnId::new(),
                parent_event.clone(),
                "wrong turn"
            ),
            Err(AgentMessageError::ParentEventMismatch(_))
        ));
        assert!(matches!(
            queue.queue(sender.clone(), turn.clone(), parent_event.clone(), "   "),
            Err(AgentMessageError::InvalidMessage(_))
        ));
        assert!(matches!(
            queue.queue(
                sender,
                turn,
                parent_event,
                "x".repeat(MAX_AGENT_MESSAGE_BYTES + 1)
            ),
            Err(AgentMessageError::InvalidMessage(_))
        ));
    }

    #[tokio::test]
    async fn completion_before_subscription_is_returned_immediately_and_at_least_once() {
        let (mailbox, sink, run_id, _) = mailbox();
        let source = AgentId::parse("agt_mailbox_child").unwrap();
        let cause = message(&*sink, &run_id, Some(source.clone()), None);
        let handoff = HandoffValidator::default()
            .validate(
                StructuredHandoffV1 {
                    version: 1,
                    status: HandoffStatus::Completed,
                    summary: "Finished before the parent subscribed.".into(),
                    evidence: vec![HandoffEvidence {
                        path: "ovim-core/src/agent_runtime/mailbox.rs".into(),
                        line: Some(1),
                        claim: "Durable state is checked before waiting.".into(),
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
        let handoff_event = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: Some(cause.event_id),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("completion_test".into()),
                agent_id: Some(source.clone()),
                turn_id: None,
                workspace_id: None,
                branch_id: None,
                kind: EventKind::AgentHandoff(AgentHandoffEvent {
                    handoff: handoff.clone(),
                    workspace_warnings: Vec::new(),
                }),
            })
            .unwrap();
        let terminal_event = sink
            .append(NewRunEvent {
                run_id: run_id.clone(),
                caused_by: Some(handoff_event.event_id.clone()),
                operation_id: None,
                provider_call_id: None,
                actor: EventActor::System("completion_test".into()),
                agent_id: Some(source.clone()),
                turn_id: None,
                workspace_id: None,
                branch_id: None,
                kind: EventKind::AgentLifecycle(AgentLifecycleEvent {
                    agent_id: source.clone(),
                    parent_agent_id: Some(mailbox.recipient_agent_id().clone()),
                    state: AgentLifecycleState::Completed,
                    kind: "explorer".into(),
                    objective: Some("finish before subscription".into()),
                    detail: None,
                    dispatch_spec: None,
                }),
            })
            .unwrap();
        let notification = mailbox
            .notify(MailboxNotificationKind::Handoff {
                source_agent_id: source,
                terminal_event_id: terminal_event.event_id,
                handoff_event_id: handoff_event.event_id,
                handoff: Box::new(handoff.parent_projection()),
            })
            .unwrap();

        let first = mailbox.wait(Duration::ZERO).await.unwrap();
        let second = mailbox.wait(Duration::ZERO).await.unwrap();
        assert_eq!(first, second);
        assert!(matches!(
            first,
            MailboxWaitOutcome::Updates(entries)
                if entries[0].notification_event_id == notification.notification_event_id
                    && matches!(entries[0].notification, MailboxNotificationKind::Handoff { .. })
        ));
    }

    #[test]
    fn consumption_is_idempotent_by_notification_event_id() {
        let (mailbox, sink, run_id, _) = mailbox();
        let source = AgentId::parse("agt_mailbox_consume_child").unwrap();
        let event = message(&*sink, &run_id, Some(source.clone()), None);
        let notification = mailbox
            .notify(MailboxNotificationKind::Message {
                sender_agent_id: Some(source),
                message_event_id: event.event_id,
            })
            .unwrap();
        let first = mailbox
            .consume(&notification.notification_event_id)
            .unwrap();
        let count_after_first = sink.events(&run_id).unwrap().len();
        let second = mailbox
            .consume(&notification.notification_event_id)
            .unwrap();

        let MailboxConsumeOutcome::Consumed(first) = first else {
            panic!("first consume did not append")
        };
        let MailboxConsumeOutcome::AlreadyConsumed(second) = second else {
            panic!("second consume was not idempotent")
        };
        assert_eq!(first, second);
        assert_eq!(sink.events(&run_id).unwrap().len(), count_after_first);
        assert!(mailbox.pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn steering_wakes_a_subscribed_waiter_without_polling() {
        let (mailbox, sink, run_id, _) = mailbox();
        let waiting = {
            let mailbox = mailbox.clone();
            tokio::spawn(async move { mailbox.wait(Duration::from_secs(1)).await.unwrap() })
        };
        tokio::task::yield_now().await;

        let turn_id = TurnId::parse("trn_mailbox_steering").unwrap();
        let event = message(&*sink, &run_id, None, Some(turn_id.clone()));
        mailbox
            .notify(MailboxNotificationKind::Steering {
                turn_id,
                message_event_id: event.event_id,
            })
            .unwrap();

        assert!(matches!(
            waiting.await.unwrap(),
            MailboxWaitOutcome::Updates(entries)
                if matches!(entries[0].notification, MailboxNotificationKind::Steering { .. })
        ));
    }

    #[tokio::test]
    async fn bounded_wait_times_out_without_a_notification() {
        let (mailbox, _, _, _) = mailbox();
        assert_eq!(
            mailbox.wait(Duration::from_millis(10)).await.unwrap(),
            MailboxWaitOutcome::TimedOut
        );
    }

    #[test]
    fn sqlite_round_trip_replays_pending_and_consumed_state() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("events.sqlite3");
        let run_id = RunId::parse("run_mailbox_sqlite").unwrap();
        let recipient = AgentId::parse("agt_mailbox_sqlite_parent").unwrap();
        let source = AgentId::parse("agt_mailbox_sqlite_child").unwrap();
        let sink: Arc<dyn RunEventSink> = Arc::new(SqliteRunEventSink::open(&database).unwrap());
        let source_event = message(&*sink, &run_id, Some(source.clone()), None);
        let mailbox = AgentMailbox::new(run_id.clone(), recipient.clone(), sink).unwrap();
        let notification = mailbox
            .notify(MailboxNotificationKind::Message {
                sender_agent_id: Some(source),
                message_event_id: source_event.event_id,
            })
            .unwrap();
        mailbox
            .consume(&notification.notification_event_id)
            .unwrap();
        drop(mailbox);

        let reopened: Arc<dyn RunEventSink> =
            Arc::new(SqliteRunEventSink::open(&database).unwrap());
        let projection =
            AgentMailboxProjection::rehydrate(recipient, &reopened.events(&run_id).unwrap())
                .unwrap();
        assert!(projection.pending().is_empty());
        assert!(projection
            .consumed_entry(&notification.notification_event_id)
            .is_some());
    }
}
