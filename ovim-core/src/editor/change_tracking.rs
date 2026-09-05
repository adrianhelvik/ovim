//! Change tracking and undo/redo operations

use super::{Change, CursorPos, Editor, ToastLevel, ToastRequest, ToastSource};
use crate::change::{ChangeToken, UndoOutcome};
use crate::edit::Edit;
use crate::repeat_action::RepeatAction;

// Phase-05 Step F: decoration positions are now projected on demand from
// `edit_log.edits_since(source_version)` at render time, so the undo/redo
// and recorded-undo paths no longer need to call `adjust_for_edits` — the
// edit log already captures the forward/inverse edit groups that projection
// replays. See `ovim-core/src/editor/decoration.rs` module docs.

impl Editor {
    /// Records a buffer mutation with undo tracking and optional dot-repeat.
    ///
    /// Captures cursor_before/after, records edits via `buffer.record()`,
    /// pushes an undo entry, sets the repeat action, and marks the buffer
    /// modified for LSP sync. Returns the closure's result so callers can
    /// use it (e.g., deleted text for registers).
    pub fn record_operation<R>(
        &mut self,
        f: impl FnOnce(&mut crate::buffer::Buffer) -> R,
        repeat_action: Option<RepeatAction>,
    ) -> R {
        let cursor_before = self.cursor_position();
        let (result, edits) = self.buffer_mut().record(f);
        if !edits.is_empty() {
            let cursor_after = self.cursor_position();
            // push_recorded_undo() calls mark_buffer_modified() internally
            self.push_recorded_undo(edits, cursor_before, cursor_after);
            if let Some(action) = repeat_action {
                self.set_repeat_action(action);
            }
        }
        result
    }

    /// Applies a buffer-level edit inside an active recording session.
    ///
    /// Used by insert/replace-mode keystroke handlers after the removal of
    /// `Change::InsertText` / `DeleteText`. The caller must have opened a
    /// session via `start_change_building` (directly, or indirectly via an
    /// operator that routes through `PendingChangeRepeat`). Edits flow into
    /// the ambient session; the undo push happens at
    /// `finalize_change_building`.
    ///
    /// Snapshots the recording origin on the first edit so
    /// `RepeatAction::InsertSession` dot-repeat can re-anchor correctly.
    ///
    /// `f` must mutate the buffer via `insert_text_at_positioning_cursor` /
    /// `delete_range_positioning_cursor` (or equivalent) — it returns whether
    /// the buffer version changed, which this function propagates to the
    /// caller so they can early-return on no-op.
    ///
    /// # Panics (debug)
    ///
    /// Panics in debug builds if called outside an active recording session.
    /// In release builds the check is elided; callers that forget to open a
    /// session will still execute `f` but the edit will miss undo/dot-repeat.
    pub fn record_session_edit<F>(&mut self, f: F) -> bool
    where
        F: FnOnce(&mut crate::buffer::Buffer) -> bool,
    {
        debug_assert!(
            self.buffer().is_recording(),
            "record_session_edit called without an active recording session; \
             open one via start_change_building first"
        );
        // If an outer stateful session is still missing its origin (this is
        // the first edit of an insert session), snapshot the cursor's char
        // offset now — that's the reference point `InsertSession` dot-repeat
        // translates against. Also capture the grapheme-space cursor so
        // `g;` / the changelist can land here rather than at the
        // pre-entry-mode cursor stored on the Recorded.
        if self.buffer().recording_origin().is_none() {
            let (offset, cursor) = {
                let buf = self.buffer();
                let off = buf.rope().line_to_char(buf.cursor().line()) + buf.cursor_char_col().0;
                let cur = CursorPos::new(buf.cursor().line(), buf.cursor().col());
                (off, cur)
            };
            self.buffer_mut().set_recording_origin(offset, cursor);
        }
        // Outer `record()` caller owns edit capture.
        f(self.buffer_mut())
    }

    /// Pops the last change from the undo stack (without undoing it)
    /// Used when replacing a change with a composite version
    pub fn pop_last_change(&mut self) -> Option<Change> {
        self.buffer_mut().change_manager_mut().pop_last_change()
    }

    /// Undoes the last change.
    ///
    /// Surfaces an error toast if a `ResourceOp` snapshot restore fails
    /// (e.g., write to a read-only directory). Pre-OV-00212 the failure was
    /// silently swallowed and undo appeared to succeed; now the user sees
    /// the OS error and the offending change stays on the undo stack so a
    /// retry is possible once they fix the filesystem.
    pub fn undo(&mut self) {
        self.undo_count(1);
    }

    /// Undoes up to `count` history groups, stopping at the boundary or an error.
    pub fn undo_count(&mut self, count: usize) {
        for _ in 0..count {
            let (outcome, _edits) = self.buffer_mut().undo();
            self.surface_undo_outcome(&outcome, "Undo");
            if !matches!(outcome, UndoOutcome::Done) {
                break;
            }
        }
        self.invalidate_hover_cache();
        self.mark_buffer_modified();
        self.sync_modified_flag_with_save_point();
        self.mark_dirty();
    }

    /// Redoes the next change. See `undo` for the OV-00212 toast rationale.
    pub fn redo(&mut self) {
        self.redo_count(1);
    }

    /// Redoes up to `count` history groups, stopping at the boundary or an error.
    pub fn redo_count(&mut self, count: usize) {
        for _ in 0..count {
            let (outcome, _edits) = self.buffer_mut().redo();
            self.surface_undo_outcome(&outcome, "Redo");
            if !matches!(outcome, UndoOutcome::Done) {
                break;
            }
        }
        self.invalidate_hover_cache();
        self.mark_buffer_modified();
        self.sync_modified_flag_with_save_point();
        self.mark_dirty();
    }

    /// After undo/redo, reconcile the buffer's sticky `modified` bool with
    /// the undo save point: landing exactly on the save point means the
    /// content equals what was last written, so the buffer is unmodified
    /// again (vim: edit + `u` → &modified == 0, redo back to the save
    /// point → 0; verified `nvim --clean`, OV-00340). The sticky bool is
    /// otherwise left alone — it covers mutations that bypass the change
    /// manager, and those clear the save point entirely so this can never
    /// mask them (OV-00341).
    fn sync_modified_flag_with_save_point(&mut self) {
        if self.buffer().change_manager().is_at_save_point() {
            self.buffer_mut().mark_clean();
        }
    }

    /// Pushes an error toast for `UndoOutcome::Failed`. No-op for `Done` /
    /// `Nothing` — quiet success matches the pre-existing UX where the
    /// status line is not refreshed on every undo.
    fn surface_undo_outcome(&mut self, outcome: &UndoOutcome, action: &str) {
        if let UndoOutcome::Failed(err) = outcome {
            let request = ToastRequest::new(
                ToastSource::System,
                ToastLevel::Error,
                format!("{action} failed: {err}"),
            )
            .with_sticky(true)
            .with_dedupe_key(format!("{}-resource-op-failure", action.to_lowercase()));
            self.push_toast(request);
        }
    }

    /// Repeats the last change with proper cursor position tracking.
    ///
    /// Records cursor_before/cursor_after so undo after dot-repeat restores
    /// the cursor to where the repeat happened, not the original change.
    /// Buffer mutations are captured via `record()` so the undo entry uses
    /// mechanical inverse edits rather than semantic replay.
    pub fn repeat_last_change(&mut self) {
        self.repeat_last_change_with_count(None);
    }

    pub fn repeat_last_change_with_count(&mut self, count: Option<usize>) {
        // Try RepeatAction first (semantic repeat for Pattern B operations)
        if let Some(action) = self.buffer().change_manager().last_repeat_action.clone() {
            let action = if let Some(count) = count {
                let action = action.with_count(count);
                self.set_repeat_action(action.clone());
                action
            } else {
                action
            };
            let register = self
                .input
                .pending_register
                .take()
                .or(self.buffer().change_manager().last_repeat_register);
            // Paste repeat needs Editor-level access (registers), handle specially
            match &action {
                RepeatAction::PasteAfter { count } | RepeatAction::PasteBefore { count } => {
                    self.input.pending_register = register;
                    let count = *count;
                    let is_after = matches!(action, RepeatAction::PasteAfter { .. });
                    let _ = if is_after {
                        crate::editor::input::helpers::paste_after(self, count)
                    } else {
                        crate::editor::input::helpers::paste_before(self, count)
                    };
                    return;
                }
                _ => {}
            }

            let original = self.buffer().rope().clone();
            let (before, after, edits) = {
                let buf = self.buffer_mut();
                let before = CursorPos::new(buf.cursor().line(), buf.cursor().col());
                let ((), edits) = buf.record(|b| {
                    action.execute(b);
                });
                let after = CursorPos::new(buf.cursor().line(), buf.cursor().col());
                (before, after, edits)
            };

            if !edits.is_empty() {
                if let Some((text, register_type)) = action.deleted_register(&edits, &original) {
                    self.input.pending_register = register;
                    self.delete_to_register_with_type(text, register_type);
                }
                self.push_recorded_undo(edits, before, after);
            }
            return;
        }

        // Fall back to Change-based repeat
        if let Some(mut repeated) = self.buffer().change_manager().last_change().cloned() {
            let (before, after, edits) = {
                let buf = self.buffer_mut();
                let before = CursorPos::new(buf.cursor().line(), buf.cursor().col());

                // Record the repeat's buffer mutations for mechanical undo.
                let ((), edits) = buf.record(|b| {
                    // Call repeat() BEFORE set_cursor_before() — repeat() uses the
                    // original cursor_before to detect deletion direction (forward vs
                    // backward). It also updates range/deleted_text so undo works.
                    repeated.repeat(b);
                });

                let after = CursorPos::new(buf.cursor().line(), buf.cursor().col());
                (before, after, edits)
            };

            if !edits.is_empty() {
                // Push recorded undo (mechanical) — single `u` undoes the whole repeat.
                self.push_recorded_undo(edits, before, after);

                // Update repeat template positions for next repeat.
                repeated.set_cursor_before(before);
                repeated.set_cursor_after(after);
                self.buffer_mut().change_manager_mut().last_change = Some(repeated);
            }
        }
    }

    /// Pushes a recorded undo entry without setting the repeat register.
    /// Use for compound operations (join, case change, indent) where the
    /// dot-repeat change is set separately.
    ///
    /// If an AI chat undo group is active, the change is stamped with the
    /// group ID so that `u` undoes the entire agent turn at once.
    ///
    /// Returns a `ChangeToken` that can later be redeemed with `pop_by_token`
    /// to safely retrieve this exact undo entry (used by visual-`c` / `cw`
    /// delete-then-insert flows that merge the delete into a Recorded on
    /// insert-mode exit). Callers that don't need the token can ignore the
    /// return value.
    pub fn push_recorded_undo(
        &mut self,
        edits: Vec<Edit>,
        cursor_before: CursorPos,
        cursor_after: CursorPos,
    ) -> ChangeToken {
        // Decoration positions follow the edits through projection at render
        // time — the edit log already captured the recorded edits, so no
        // per-decoration mutation is required here.

        let group_id = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|c| c.current_undo_group);

        let change = if let Some(gid) = group_id {
            Change::recorded_grouped(edits, cursor_before, cursor_after, gid)
        } else {
            Change::recorded(edits, cursor_before, cursor_after)
        };
        let cm = self.buffer_mut().change_manager_mut();
        let token = cm.push_undo_change_preserving_repeat(change);
        // Ensure LSP is notified of buffer changes — callers that use record()
        // directly instead of record_operation() were previously missing this.
        self.mark_buffer_modified();
        token
    }

    /// Pops a change only if the token matches the current stack top.
    /// Returns None if the token is stale.
    pub fn pop_by_token(&mut self, token: ChangeToken) -> Option<Change> {
        self.buffer_mut().change_manager_mut().pop_by_token(token)
    }

    /// Sets a semantic repeat action for dot-repeat (mutually exclusive with last_change).
    pub fn set_repeat_action(&mut self, action: RepeatAction) {
        let cm = self.buffer_mut().change_manager_mut();
        cm.last_repeat_action = Some(action);
        cm.last_change = None; // Mutual exclusion: RepeatAction wins
    }

    /// Returns the current cursor position (grapheme-space).
    pub fn cursor_position(&self) -> CursorPos {
        CursorPos::new(self.buffer().cursor().line(), self.buffer().cursor().col())
    }

    /// Returns the last position where an edit occurred (for g; navigation).
    pub fn last_edit_position(&self) -> Option<CursorPos> {
        self.buffer().change_manager().last_edit_position
    }

    /// Jump to older changelist position (g;).
    pub fn jump_change_older(&mut self, count: usize) -> Option<CursorPos> {
        self.buffer_mut()
            .change_manager_mut()
            .jump_change_older(count)
    }

    /// Jump to newer changelist position (g,).
    pub fn jump_change_newer(&mut self, count: usize) -> Option<CursorPos> {
        self.buffer_mut()
            .change_manager_mut()
            .jump_change_newer(count)
    }

    /// Updates the . register with the last inserted text
    pub fn update_last_inserted_register(&mut self) {
        if let Some(change) = self.buffer().change_manager().last_change() {
            let inserted_text = change.get_inserted_text();
            if !inserted_text.is_empty() {
                self.registers.set_last_inserted(inserted_text);
            }
        }
    }

    /// Checks if buffer is modified relative to last save
    pub fn is_modified(&self) -> bool {
        self.buffer().is_modified() || !self.buffer().change_manager().is_at_save_point()
    }

    /// True when ANY buffer has unsaved changes — not just the current one.
    ///
    /// Guards `:qa` and last-window `:q` against silently discarding hidden
    /// modified buffers (e.g. files touched by a multi-file rename that the
    /// user chose not to write, OV-00331). Scratch buffers (`[Title]` paths)
    /// are UI artifacts and never block quitting.
    pub fn any_buffer_modified(&self) -> bool {
        (0..self.buffers.len()).any(|index| {
            !super::buffer_manager::is_scratch_buffer(&self.buffers[index])
                && self.buffer_index_is_modified(index)
        })
    }

    /// Marks current state as saved
    pub fn mark_saved(&mut self) {
        self.buffer_mut().change_manager_mut().mark_saved();
        self.buffer_mut().mark_clean();
    }

    /// Post-mutation fixup for call sites that bypass `record()`.
    ///
    /// Clears the buffer's `edit_log` (its projection is no longer sound)
    /// and invalidates inlay-hint and diagnostic slots so the next event-loop
    /// tick re-pulls them. Also routes through `mark_buffer_modified` so
    /// LSP didChange fires.
    ///
    /// Use this from bypass sites — direct `insert_text_at`/`delete_range`
    /// calls outside a `record()` session — to preserve the invariant that
    /// decoration positions stay aligned with buffer content.
    pub fn fixup_after_bypass_mutation(&mut self) {
        self.buffer_mut().edit_log_mut().clear();
        // An untracked mutation means no undo-stack state can be identified
        // with the on-disk content anymore — without this, undoing back to
        // the old save point would falsely report the buffer unmodified
        // (OV-00341).
        self.buffer_mut().change_manager_mut().clear_save_point();
        self.mark_buffer_modified();
    }
}
