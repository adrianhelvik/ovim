//! Shared test doubles for agent-runtime failure-injection tests.

use crate::run_log::{
    EventEnvelope, EventId, InMemoryRunEventSink, NewRunEvent, RunEventSink, RunId, RunLogError,
};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// A sink whose appends can be made to fail on demand — simulates a full
/// disk (ENOSPC) at exact points in a multi-append sequence. Reads always
/// succeed against the wrapped in-memory sink.
pub(crate) struct FlakySink {
    inner: Arc<InMemoryRunEventSink>,
    /// Appends remaining before failure. Negative means "never fail".
    appends_before_failure: AtomicI64,
}

impl FlakySink {
    pub(crate) fn new(inner: Arc<InMemoryRunEventSink>) -> Self {
        Self {
            inner,
            appends_before_failure: AtomicI64::new(-1),
        }
    }

    /// Allow `count` more appends, then fail every append until healed.
    pub(crate) fn fail_after(&self, count: i64) {
        self.appends_before_failure.store(count, Ordering::SeqCst);
    }

    /// Fail every append from now on.
    pub(crate) fn fail_all(&self) {
        self.fail_after(0);
    }

    /// Let appends succeed again.
    pub(crate) fn heal(&self) {
        self.appends_before_failure.store(-1, Ordering::SeqCst);
    }
}

impl RunEventSink for FlakySink {
    fn append(&self, event: NewRunEvent) -> Result<EventEnvelope, RunLogError> {
        let remaining = self.appends_before_failure.load(Ordering::SeqCst);
        if remaining >= 0 {
            if remaining == 0 {
                return Err(RunLogError::Storage {
                    operation: "append".into(),
                    detail: "no space left on device (injected)".into(),
                });
            }
            self.appends_before_failure.fetch_sub(1, Ordering::SeqCst);
        }
        self.inner.append(event)
    }

    fn event(
        &self,
        run_id: &RunId,
        event_id: &EventId,
    ) -> Result<Option<EventEnvelope>, RunLogError> {
        self.inner.event(run_id, event_id)
    }

    fn events(&self, run_id: &RunId) -> Result<Vec<EventEnvelope>, RunLogError> {
        self.inner.events(run_id)
    }

    fn last_sequence(&self, run_id: &RunId) -> Result<Option<u64>, RunLogError> {
        self.inner.last_sequence(run_id)
    }

    fn runs(&self) -> Result<Vec<RunId>, RunLogError> {
        self.inner.runs()
    }
}
