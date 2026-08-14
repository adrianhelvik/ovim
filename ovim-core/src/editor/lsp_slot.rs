//! Generic `Slot<T>` for tracking in-flight LSP requests.
//!
//! A `Slot<T>` holds at most one in-flight request.  `fire()` cancels any
//! previous request before storing the new one, so same-type cancellation is
//! structural — no sequence counters needed.  Different features use separate
//! slots, so they can coexist without interference.

use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// A single in-flight LSP request with its expected result type.
pub struct Slot<T> {
    inflight: Option<Inflight<T>>,
}

struct Inflight<T> {
    task: JoinHandle<()>,
    rx: oneshot::Receiver<anyhow::Result<T>>,
    started: Instant,
}

impl<T> Slot<T> {
    /// Create an empty slot.
    pub fn new() -> Self {
        Self { inflight: None }
    }

    /// Fire a new request.  If one is already in flight, abort it first.
    pub fn fire(&mut self, task: JoinHandle<()>, rx: oneshot::Receiver<anyhow::Result<T>>) {
        if let Some(old) = self.inflight.take() {
            old.task.abort();
        }
        self.inflight = Some(Inflight {
            task,
            rx,
            started: Instant::now(),
        });
    }

    /// Non-blocking poll.  Returns `Some(result)` when the response has
    /// arrived, `None` while still waiting.  Automatically aborts requests
    /// that have been in flight longer than `timeout`.
    pub fn poll_with_timeout(&mut self, timeout: Duration) -> Option<anyhow::Result<T>> {
        let inflight = self.inflight.as_mut()?;
        match inflight.rx.try_recv() {
            Ok(result) => {
                self.inflight.take();
                Some(result)
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                if inflight.started.elapsed() > timeout {
                    self.inflight.take().unwrap().task.abort();
                }
                None
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.inflight.take();
                None
            }
        }
    }

    /// Returns `true` if a request is currently in flight.
    pub fn is_pending(&self) -> bool {
        self.inflight.is_some()
    }

    /// Abort and clear the in-flight request, if any.
    pub fn cancel(&mut self) {
        if let Some(old) = self.inflight.take() {
            old.task.abort();
        }
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TrackedSlot<T> — Slot with generation-based invalidation
// ---------------------------------------------------------------------------

/// A `Slot<T>` with generation-based invalidation tracking.
///
/// Use this for features driven by external state changes (diagnostics,
/// inlay hints, completion) where "something changed, refresh needed" is
/// a recurring signal that must never be lost.
///
/// `invalidate()` bumps a monotonic generation counter. `is_stale()`
/// compares it against the generation that was current when the last
/// request was fired. Because the generation is a counter (not a flag),
/// it can never be "consumed" — calling `invalidate()` ten times while
/// a request is in flight means `is_stale()` stays true until a new
/// request fires, no matter how many times you check.
///
/// ## Debounce semantics (idle-trigger)
///
/// The debounce window measures idleness since the *last invalidation*,
/// not since the last fire. Every call to `invalidate()` pushes
/// `last_invalidated` forward to `Instant::now()`, so continuous
/// typing keeps deferring the next fire. `needs_refresh()` returns
/// true only once invalidations have gone quiet for the debounce
/// duration — i.e. the user paused. This replaces the older
/// "debounce-from-last-fire" semantics where a fire during continuous
/// typing would queue another fire exactly `debounce` later, even if
/// the user was still actively editing.
pub struct TrackedSlot<T> {
    slot: Slot<T>,
    /// Bumped by `invalidate()`. Monotonically increasing.
    pub(crate) generation: u64,
    /// The generation that was current when `fire()` was last called.
    pub(crate) fired_at: u64,
    /// Optional minimum idle interval since the last invalidation before
    /// `needs_refresh()` returns true.
    debounce: Option<Duration>,
    /// When `fire()` was last called. Retained for diagnostics / tests.
    pub(crate) last_fired: Option<Instant>,
    /// When `invalidate()` was last called. Used by `needs_refresh()` to
    /// implement idle-trigger semantics: fires happen after typing pauses,
    /// not during continuous invalidation.
    pub(crate) last_invalidated: Option<Instant>,
}

impl<T> TrackedSlot<T> {
    pub fn new() -> Self {
        Self {
            slot: Slot::new(),
            generation: 0,
            fired_at: 0,
            debounce: None,
            last_fired: None,
            last_invalidated: None,
        }
    }

    /// Create with a minimum idle interval since the last invalidation
    /// before `needs_refresh()` returns true.
    pub fn with_debounce(debounce: Duration) -> Self {
        Self {
            debounce: Some(debounce),
            ..Self::new()
        }
    }

    /// Mark the current result as stale. Cheap, idempotent-ish, never
    /// loses information — call it as often as you like.
    ///
    /// Also bumps `last_invalidated` to now so the debounce window
    /// restarts. Rapid-fire invalidations therefore stay debounced
    /// until the caller goes idle for at least `debounce`.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.last_invalidated = Some(Instant::now());
    }

    /// Has `invalidate()` been called since the last `fire()`?
    pub fn is_stale(&self) -> bool {
        self.generation > self.fired_at
    }

    /// Is stale AND idle for at least the debounce window since the
    /// last `invalidate()`?
    ///
    /// Under idle-trigger semantics the clock starts at the most
    /// recent `invalidate()`, not at the last `fire()`. Continuous
    /// invalidations (e.g. per-keystroke) keep pushing the deadline
    /// forward so refreshes only fire when the caller actually pauses.
    ///
    /// Cold start exception: if we have never fired, debounce is
    /// bypassed so initial data (e.g. hints on file open) shows up
    /// without a perceptible delay. The debounce only kicks in once
    /// there is a prior fire to protect against thrashing.
    pub fn needs_refresh(&self) -> bool {
        if !self.is_stale() {
            return false;
        }
        // Cold start: never fired → no thrashing risk yet, fire now.
        if self.last_fired.is_none() {
            return true;
        }
        if let (Some(debounce), Some(last)) = (self.debounce, self.last_invalidated) {
            if last.elapsed() < debounce {
                return false;
            }
        }
        true
    }

    /// Fire a new request, marking this generation as covered.
    pub fn fire(&mut self, task: JoinHandle<()>, rx: oneshot::Receiver<anyhow::Result<T>>) {
        self.fired_at = self.generation;
        self.last_fired = Some(Instant::now());
        self.slot.fire(task, rx);
    }

    /// Non-blocking poll with explicit timeout.
    pub fn poll_with_timeout(&mut self, timeout: Duration) -> Option<anyhow::Result<T>> {
        self.slot.poll_with_timeout(timeout)
    }

    /// Is a request currently in flight?
    pub fn is_pending(&self) -> bool {
        self.slot.is_pending()
    }

    /// Abort the in-flight request and mark as stale so a re-request
    /// happens on the next tick.
    pub fn cancel_and_invalidate(&mut self) {
        self.slot.cancel();
        self.invalidate();
    }

    /// Abort the in-flight request without invalidating.
    pub fn cancel(&mut self) {
        self.slot.cancel();
    }
}

impl<T> Default for TrackedSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- TrackedSlot unit tests (pure state, no async) --

    /// Simulate firing by advancing the generation bookkeeping
    /// without needing a real tokio task. For state-machine tests only.
    fn simulate_fire<T>(slot: &mut TrackedSlot<T>) {
        slot.fired_at = slot.generation;
        slot.last_fired = Some(Instant::now());
    }

    #[test]
    fn fresh_slot_is_not_stale() {
        let slot: TrackedSlot<()> = TrackedSlot::new();
        assert!(!slot.is_stale());
        assert!(!slot.needs_refresh());
    }

    #[test]
    fn invalidate_makes_stale() {
        let mut slot: TrackedSlot<()> = TrackedSlot::new();
        slot.invalidate();
        assert!(slot.is_stale());
        assert!(slot.needs_refresh());
    }

    #[test]
    fn fire_clears_staleness() {
        let mut slot: TrackedSlot<String> = TrackedSlot::new();
        slot.invalidate();
        assert!(slot.is_stale());

        simulate_fire(&mut slot);
        assert!(!slot.is_stale());
        assert!(!slot.needs_refresh());
    }

    #[test]
    fn invalidate_during_flight_stays_stale_after_fire() {
        let mut slot: TrackedSlot<String> = TrackedSlot::new();
        slot.invalidate(); // gen 1
        simulate_fire(&mut slot); // fired_at = 1
        assert!(!slot.is_stale());

        slot.invalidate(); // gen 2 — new data arrived while request was in flight
        assert!(slot.is_stale()); // fired_at(1) < generation(2)
    }

    #[test]
    fn multiple_invalidates_without_fire_stay_stale() {
        let mut slot: TrackedSlot<()> = TrackedSlot::new();
        slot.invalidate();
        slot.invalidate();
        slot.invalidate();
        assert!(slot.is_stale());
        // generation is 3, fired_at is 0 — stale no matter how many times we check
        assert!(slot.is_stale());
        assert!(slot.is_stale());
    }

    #[test]
    fn debounce_suppresses_needs_refresh() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_secs(100));
        slot.invalidate();
        simulate_fire(&mut slot);

        // Immediately invalidate again — within debounce window
        slot.invalidate();
        assert!(slot.is_stale()); // generation advanced
        assert!(!slot.needs_refresh()); // but debounce says "too soon"
    }

    #[test]
    fn debounce_allows_refresh_after_window() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_millis(0));
        slot.invalidate();
        simulate_fire(&mut slot);

        slot.invalidate();
        // debounce is 0ms so it's immediately ready
        assert!(slot.needs_refresh());
    }

    /// Confirms that a tight loop of invalidate() calls is fully absorbed
    /// by the debounce window — the slot reports "stale but not yet ready"
    /// for every invalidation inside the window.
    ///
    /// This is the safety guarantee that lets us hoist slot invalidation
    /// into the canonical `mark_buffer_modified` hook without fear of
    /// thrashing the LSP server: even if mark_buffer_modified fires 10k
    /// times per second, `needs_refresh()` stays false until the debounce
    /// window elapses after the last fire.
    #[test]
    fn tight_invalidate_loop_is_debounced() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_secs(60));
        slot.invalidate();
        simulate_fire(&mut slot);

        // Invalidate 1000 times in rapid succession.
        for _ in 0..1000 {
            slot.invalidate();
        }

        // Every invalidation advanced the generation, but the debounce
        // window suppresses the refresh signal.
        assert!(slot.is_stale());
        assert!(
            !slot.needs_refresh(),
            "debounce must absorb a tight loop of invalidations"
        );
        assert_eq!(
            slot.generation,
            slot.fired_at + 1000,
            "each invalidate bumps generation — no coalescing"
        );
    }

    // -------------------------------------------------------------------
    // Sprint 2 — Idle-trigger semantics.
    //
    // The debounce window is anchored to the last `invalidate()` call,
    // not the last `fire()`. Rapid-fire invalidations therefore keep
    // pushing the deadline forward: `needs_refresh()` only returns true
    // once the caller goes quiet for at least `debounce`.
    // -------------------------------------------------------------------

    /// Signal 4: simulate rapid invalidation during a request's flight.
    ///
    /// Before the fix (`needs_refresh` debounced from `last_fired`), a
    /// refresh would fire `debounce` after the previous fire — even if
    /// the user was still actively editing and the captured
    /// `lsp_sent_version` was guaranteed to be behind by the time the
    /// server answered. That reply then hit the version-mismatch drop
    /// path in `poll_pending_inlay_hint_response`.
    ///
    /// After the fix, `needs_refresh` waits for an idle window after
    /// the last invalidation. Continuous invalidation keeps the slot
    /// quiet, so no stale-by-design request is ever spawned.
    #[test]
    fn continuous_invalidation_suppresses_refresh_under_idle_semantics() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_millis(100));

        // First invalidate + fire (simulates the initial refresh).
        slot.invalidate();
        simulate_fire(&mut slot);

        // Simulate a user typing continuously: five invalidations spread
        // across ~200ms, with a small gap between each.
        for _ in 0..5 {
            slot.invalidate();
            std::thread::sleep(Duration::from_millis(40));
            // Each tick of the event loop asks whether we should refresh.
            // Because the last invalidation is very recent, we must NOT.
            assert!(
                !slot.needs_refresh(),
                "needs_refresh must stay false while invalidations are still landing"
            );
        }

        // Now the user pauses. After the debounce window elapses, we
        // may fire — the server is idle, buffer is quiet, safe to ask.
        std::thread::sleep(Duration::from_millis(120));
        assert!(
            slot.needs_refresh(),
            "needs_refresh must become true once invalidations go quiet"
        );
    }

    /// Direct framing of the idle-trigger invariant:
    /// invalidate → fire → invalidate-during-flight → the next refresh
    /// happens `debounce` after the second invalidate, not `debounce`
    /// after the fire.
    #[test]
    fn needs_refresh_measures_from_last_invalidate_not_last_fire() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_millis(100));

        slot.invalidate();
        simulate_fire(&mut slot);

        // Let the post-fire clock run past the debounce window without
        // any further invalidation. Because the slot isn't stale, no
        // refresh is needed — independent of the timer.
        std::thread::sleep(Duration::from_millis(110));
        assert!(!slot.is_stale());
        assert!(!slot.needs_refresh());

        // Invalidate now. We're past the fire debounce, so under the
        // old "debounce-from-fire" rule this would trip needs_refresh
        // immediately. Under idle-trigger it should wait for the
        // debounce window anchored on *this* invalidation.
        slot.invalidate();
        assert!(slot.is_stale());
        assert!(
            !slot.needs_refresh(),
            "needs_refresh must debounce from the most recent invalidate"
        );

        std::thread::sleep(Duration::from_millis(110));
        assert!(
            slot.needs_refresh(),
            "needs_refresh must become true after the idle window since last invalidate"
        );
    }

    /// Bonus invariant: the stale-response drop path still exists for
    /// genuinely stale responses (the one in `lsp_integration.rs` guards
    /// against `lsp_version < current_file_lsp_sent_version`). What the
    /// fix changes is that normal typing no longer *produces* stale
    /// responses, so the drop path should essentially never fire during
    /// regular editing. Here we simply verify the state machine still
    /// surfaces staleness after a missed fire window — the drop path's
    /// invariant (stale responses are re-invalidated for retry) is
    /// unaffected because `cancel_and_invalidate` still bumps generation.
    #[test]
    fn cancel_and_invalidate_still_drives_retry() {
        let mut slot: TrackedSlot<String> = TrackedSlot::with_debounce(Duration::from_millis(0));
        slot.invalidate();
        simulate_fire(&mut slot);
        assert!(!slot.is_stale());

        // Simulate: server answered but we dropped the result (wrong
        // version). That path calls `invalidate()` on the slot.
        slot.cancel_and_invalidate();
        assert!(slot.is_stale());
        assert!(
            slot.needs_refresh(),
            "with 0ms debounce, a drop-path invalidate must allow an immediate retry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_returns_result_when_ready() {
        let mut slot: TrackedSlot<i32> = TrackedSlot::new();
        slot.invalidate();

        let (tx, rx) = oneshot::channel();
        tx.send(Ok(42)).ok();
        let task = tokio::spawn(async {});
        slot.fire(task, rx);

        let result = slot.poll_with_timeout(Duration::from_secs(15));
        assert!(result.is_some());
        assert_eq!(result.unwrap().unwrap(), 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_returns_none_while_pending() {
        let mut slot: TrackedSlot<i32> = TrackedSlot::new();
        slot.invalidate();

        let (_tx, rx) = oneshot::channel();
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        slot.fire(task, rx);

        assert!(slot.poll_with_timeout(Duration::from_secs(15)).is_none());
        assert!(slot.is_pending());
    }

    #[test]
    fn cancel_and_invalidate_makes_stale() {
        let mut slot: TrackedSlot<String> = TrackedSlot::new();
        slot.invalidate();
        simulate_fire(&mut slot);
        assert!(!slot.is_stale());

        slot.cancel_and_invalidate();
        assert!(!slot.is_pending());
        assert!(slot.is_stale());
    }

    /// The scenario that caused the diagnostic bug: invalidate arrives
    /// while a request is pending, but the old code's is_pending() guard
    /// prevented re-firing. With TrackedSlot, is_stale() is independent
    /// of is_pending().
    #[test]
    fn invalidate_while_pending_allows_refire() {
        let mut slot: TrackedSlot<String> = TrackedSlot::new();
        slot.invalidate(); // gen 1
        simulate_fire(&mut slot); // fired_at = 1
        assert!(!slot.is_stale());

        // Simulate: new data arrives from server while we're processing
        slot.invalidate(); // gen 2
        assert!(slot.is_stale());
        assert!(slot.needs_refresh());

        simulate_fire(&mut slot); // fired_at = 2
        assert!(!slot.is_stale()); // caught up
    }
}

// ---- Result types for each slot ----

/// Result of a goto-definition / goto-implementation / goto-type-definition request.
pub struct GotoLocationResult {
    pub location: Option<lsp_types::Location>,
    /// Whether to open the result in a new tab.
    pub new_tab: bool,
}

/// Result of a hover request.
pub struct HoverResult {
    /// The rendered hover text, or `None` if the server had nothing to show.
    pub hover_text: Option<String>,
}

/// Result of a completion request.
#[derive(Debug)]
pub struct CompletionResult {
    pub items: Vec<lsp_types::CompletionItem>,
    /// File path at the time the request was fired. Used to drop responses
    /// that arrive after the user switched files.
    pub file_path: String,
    /// Buffer version at the time the request was fired. Used to drop
    /// responses that arrive after the buffer has been edited further,
    /// so stale completions do not populate the menu.
    pub buffer_version: usize,
    /// If we successfully flushed content to LSP, record the new synced content.
    pub synced_content: Option<String>,
    pub synced_lsp_version: Option<i32>,
}

/// Result of an inlay hint request.
#[derive(Debug)]
pub struct InlayHintResult {
    pub request_key: super::lsp_state::InlayHintRequestKey,
    pub buffer_version: usize,
    /// If we successfully flushed content to LSP, record the new synced content.
    pub synced_content: Option<String>,
    pub synced_lsp_version: Option<i32>,
    pub hints: Vec<lsp_types::InlayHint>,
}

/// Result of a diagnostic refresh request.
#[derive(Debug)]
pub struct DiagnosticResult {
    pub file_path: String,
    pub buffer_version: usize,
    pub lsp_version: i32,
    pub lsp_sent_version: i32,
    pub diagnostics: Vec<lsp_types::Diagnostic>,
    pub count: (usize, usize, usize, usize),
    pub deferred: bool,
}

/// Result of a format-document request.
#[derive(Debug)]
pub struct FormatResult {
    pub edits: Vec<lsp_types::TextEdit>,
    /// File path at the time the request was fired. The edits are positions
    /// into that exact document snapshot — applying them to anything else
    /// corrupts the buffer.
    pub file_path: String,
    /// Buffer version at the time the request was fired.
    pub buffer_version: usize,
}

/// Result of a find-references request.
pub struct ReferencesResult {
    pub locations: Vec<lsp_types::Location>,
}

/// Result of a document-symbols request.
pub struct DocumentSymbolsResult {
    pub symbols: Vec<lsp_types::DocumentSymbol>,
    pub file_path: String,
}

/// Result of a workspace-symbols request.
pub struct WorkspaceSymbolsResult {
    pub symbols: Vec<lsp_types::SymbolInformation>,
}

/// Result of a code-actions request.
pub struct CodeActionsResult {
    pub actions: Vec<super::lsp_state::AvailableCodeAction>,
    /// File path / buffer version at fire time — action edits embed
    /// positions into that snapshot (stale ones corrupt the buffer).
    pub file_path: String,
    pub buffer_version: usize,
}

/// Result of a rename request.
pub struct RenameResult {
    pub edit: Option<lsp_types::WorkspaceEdit>,
    pub new_name: String,
    /// File path / buffer version at fire time — the workspace edit embeds
    /// positions into that snapshot (stale ones corrupt the buffer).
    pub file_path: String,
    pub buffer_version: usize,
}

/// Result of an organize-imports request.
pub struct OrganizeImportsResult {
    pub action: Option<super::lsp_state::AvailableCodeAction>,
    /// File path / buffer version at fire time — see `RenameResult`.
    pub file_path: String,
    pub buffer_version: usize,
}

/// Result of a call-hierarchy request (incoming or outgoing).
pub struct CallHierarchyResult {
    pub locations: Vec<lsp_types::Location>,
    pub direction: CallHierarchyDirection,
}

#[derive(Debug, Clone, Copy)]
pub enum CallHierarchyDirection {
    Incoming,
    Outgoing,
}

/// Result of a type-hierarchy request.
pub struct TypeHierarchyResult {
    pub types: Vec<(String, lsp_types::Location)>,
    pub all_locations: Vec<lsp_types::Location>,
}

/// Result of a semantic-tokens request.
pub struct SemanticTokensSlotResult {
    pub tokens: Option<lsp_types::SemanticTokens>,
    pub legend: Option<lsp_types::SemanticTokensLegend>,
}

/// All LSP request slots, grouped for easy access from the editor.
///
/// Each feature gets its own slot so different-type requests coexist.
/// Same-type requests cancel the previous one automatically via `Slot::fire()`.
pub struct LspSlots {
    // -- Navigation (Step 2-3) --
    pub goto_definition: Slot<GotoLocationResult>,
    pub goto_implementation: Slot<GotoLocationResult>,
    pub goto_type_definition: Slot<GotoLocationResult>,
    pub hover: Slot<HoverResult>,
    // -- Query (Step 4) --
    pub completion: Slot<CompletionResult>,
    pub inlay_hints: TrackedSlot<InlayHintResult>,
    pub diagnostics: TrackedSlot<DiagnosticResult>,
    // -- Actions (Step 5) --
    pub format: Slot<FormatResult>,
    pub references: Slot<ReferencesResult>,
    pub document_symbols: Slot<DocumentSymbolsResult>,
    pub workspace_symbols: Slot<WorkspaceSymbolsResult>,
    pub code_actions: Slot<CodeActionsResult>,
    pub rename: Slot<RenameResult>,
    pub organize_imports: Slot<OrganizeImportsResult>,
    pub call_hierarchy: Slot<CallHierarchyResult>,
    pub type_hierarchy: Slot<TypeHierarchyResult>,
    pub semantic_tokens: Slot<SemanticTokensSlotResult>,
}

impl LspSlots {
    /// Abort all in-flight requests.
    pub fn cancel_all(&mut self) {
        self.goto_definition.cancel();
        self.goto_implementation.cancel();
        self.goto_type_definition.cancel();
        self.hover.cancel();
        self.completion.cancel();
        self.inlay_hints.cancel();
        self.diagnostics.cancel();
        self.format.cancel();
        self.references.cancel();
        self.document_symbols.cancel();
        self.workspace_symbols.cancel();
        self.code_actions.cancel();
        self.rename.cancel();
        self.organize_imports.cancel();
        self.call_hierarchy.cancel();
        self.type_hierarchy.cancel();
        self.semantic_tokens.cancel();
    }

    /// Returns true if any slot has an in-flight request.
    pub fn any_pending(&self) -> bool {
        self.goto_definition.is_pending()
            || self.goto_implementation.is_pending()
            || self.goto_type_definition.is_pending()
            || self.hover.is_pending()
            || self.completion.is_pending()
            || self.inlay_hints.is_pending()
            || self.diagnostics.is_pending()
            || self.format.is_pending()
            || self.references.is_pending()
            || self.document_symbols.is_pending()
            || self.workspace_symbols.is_pending()
            || self.code_actions.is_pending()
            || self.rename.is_pending()
            || self.organize_imports.is_pending()
            || self.call_hierarchy.is_pending()
            || self.type_hierarchy.is_pending()
            || self.semantic_tokens.is_pending()
    }
}

impl Default for LspSlots {
    fn default() -> Self {
        Self {
            goto_definition: Slot::new(),
            goto_implementation: Slot::new(),
            goto_type_definition: Slot::new(),
            hover: Slot::new(),
            completion: Slot::new(),
            // Match CHANGE_DEBOUNCE_MS (150ms) so the first refresh fires
            // promptly after the last edit lands on the server rather
            // than waiting 350ms longer than necessary.
            inlay_hints: TrackedSlot::with_debounce(Duration::from_millis(150)),
            diagnostics: TrackedSlot::new(),
            format: Slot::new(),
            references: Slot::new(),
            document_symbols: Slot::new(),
            workspace_symbols: Slot::new(),
            code_actions: Slot::new(),
            rename: Slot::new(),
            organize_imports: Slot::new(),
            call_hierarchy: Slot::new(),
            type_hierarchy: Slot::new(),
            semantic_tokens: Slot::new(),
        }
    }
}
