//! LSP diagnostics handling
//!
//! This module handles LSP diagnostics (errors, warnings, hints).
//! It provides diagnostic querying, caching, and display functionality.

use super::super::Editor;
use crate::edit::Edit;
use crate::editor::lsp_state::{DiagnosticAnchor, DiagnosticAnchors, ProjectedDiagnostics};
use crate::lsp::uri_from_file_path;
use ropey::Rope;
use std::collections::BTreeMap;

fn diagnostic_counts(diagnostics: &[lsp_types::Diagnostic]) -> (usize, usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut info = 0;
    let mut hints = 0;
    for diagnostic in diagnostics {
        match diagnostic.severity {
            Some(lsp_types::DiagnosticSeverity::ERROR) => errors += 1,
            Some(lsp_types::DiagnosticSeverity::WARNING) => warnings += 1,
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => info += 1,
            Some(lsp_types::DiagnosticSeverity::HINT) => hints += 1,
            // LSP 3.17 leaves missing severity "implementation-defined"; VS Code
            // treats it as an error, so we do too — counting it as a warning
            // under-reported the error count for servers that omit severity
            // (some Java/Scala tooling). (OV-00270)
            None => errors += 1,
            _ => {}
        }
    }
    (errors, warnings, info, hints)
}

/// Like `decoration::project_offset`, but an offset engulfed by a delete
/// clamps to the deletion point instead of dropping. Used for diagnostic
/// range endpoints: a delete that eats the exact diagnosed span must not
/// hide the gutter sign while the EOL message (anchored at line start) is
/// still rendered.
fn project_offset_clamped(source_offset: usize, edits: &[&Edit]) -> usize {
    let mut pos = source_offset;
    for edit in edits {
        match edit {
            Edit::Insert { offset, text } => {
                if pos >= *offset {
                    pos += text.chars().count();
                }
            }
            Edit::Delete { offset, text } => {
                let len = text.chars().count();
                let end = *offset + len;
                if pos >= end {
                    pos -= len;
                } else if pos > *offset {
                    pos = *offset;
                }
            }
        }
    }
    pos
}

/// Project one diagnostic's anchored offsets through `edits`, rebuilding an
/// LSP range against the current rope.
///
/// The target line is derived from the projected *line-start* anchor — the
/// exact projection the diagnostic's EOL decoration performs — so both
/// pipelines always agree on the line. Returns `None` when a delete engulfed
/// the line anchor (the decoration is dropped in that case too).
fn project_diagnostic(
    diag: &lsp_types::Diagnostic,
    anchor: &DiagnosticAnchor,
    edits: &[&Edit],
    rope: &Rope,
) -> Option<lsp_types::Diagnostic> {
    let line_anchor = crate::editor::decoration::project_offset(anchor.line_start, edits)?;
    let line = rope.char_to_line(line_anchor.min(rope.len_chars()));

    let start_off = project_offset_clamped(anchor.start, edits).min(rope.len_chars());
    let end_off = project_offset_clamped(anchor.end, edits)
        .min(rope.len_chars())
        .max(start_off);

    let line_start_char = rope.line_to_char(line);
    let line_text = crate::display::line_content(rope, line);
    let line_len = line_text.chars().count();

    // Clamp into the anchor line: if an edit split the diagnosed line, the
    // range stays on the line the EOL message renders on rather than
    // disagreeing with it. Column drift here self-heals on the next publish.
    let start_idx = start_off.saturating_sub(line_start_char).min(line_len);
    let start = lsp_types::Position {
        line: line as u32,
        character: crate::lsp::char_col_to_utf16(&line_text, start_idx),
    };

    let end_line = rope.char_to_line(end_off);
    let end = if end_line <= line {
        let end_idx = start_idx.max(end_off.saturating_sub(line_start_char).min(line_len));
        lsp_types::Position {
            line: line as u32,
            character: crate::lsp::char_col_to_utf16(&line_text, end_idx),
        }
    } else {
        // Multi-line diagnostic: keep the projected end on its own line; the
        // renderer clamps the first-line squiggle to the line length.
        let end_line_text = crate::display::line_content(rope, end_line);
        let end_idx = (end_off - rope.line_to_char(end_line)).min(end_line_text.chars().count());
        lsp_types::Position {
            line: end_line as u32,
            character: crate::lsp::char_col_to_utf16(&end_line_text, end_idx),
        }
    };

    let mut projected = diag.clone();
    projected.range = lsp_types::Range { start, end };
    Some(projected)
}

/// How cached diagnostics map onto the current buffer (OV-00328).
enum DiagnosticProjection<'a> {
    /// Anchors match the current buffer (or there are no usable anchors):
    /// the raw line-indexed view is already correct.
    RawLines,
    /// Replay these edits over the anchors.
    Replay(&'a DiagnosticAnchors, Vec<&'a Edit>),
    /// Edit-log history evicted: derive lines from the stored anchor
    /// offsets against the current rope — the decorations' fallback.
    AnchorOffsets(&'a DiagnosticAnchors),
}

impl Editor {
    /// Get current file diagnostics from LSP
    pub async fn get_current_file_diagnostics(&self) -> Option<Vec<lsp_types::Diagnostic>> {
        let lsp = self.lsp.state.lsp_manager.as_ref()?;
        let file_path = self.buffer().file_path()?;
        let uri = uri_from_file_path(file_path)?;
        let diagnostics = lsp.get_diagnostics(&uri).await;
        Some(diagnostics)
    }

    /// Get total diagnostic count (errors, warnings, info, hints) from cached diagnostics
    pub async fn get_diagnostic_count(&self) -> (usize, usize, usize, usize) {
        self.lsp.state.diagnostic_count
    }

    /// Spawn a background diagnostics refresh for the current file.
    ///
    /// INVARIANT: Callers must sync pending edits to the LSP server
    /// (`send_lsp_changes_if_modified`) before calling this.  Use
    /// `sync_lsp_and_refresh_diagnostics()` which enforces this ordering.
    pub fn spawn_diagnostic_cache_refresh(&mut self) {
        // Catch ordering violations in dev builds.  If the document sync
        // state is still dirty, diagnostics will be fetched against stale
        // server state — the exact bug we fixed by colocating sync + refresh.
        if let Some(file_path) = self.buffer().file_path() {
            if let Some(sync_state) = self.lsp.state.document_sync.get(file_path) {
                debug_assert!(
                    !sync_state.is_modified(),
                    "spawn_diagnostic_cache_refresh called while document sync is dirty \
                     for {} — send_lsp_changes_if_modified must run first",
                    file_path
                );
            }
        }

        let Some(lsp) = self.lsp.state.lsp_manager.clone() else {
            self.lsp.state.clear_current_file_diagnostics();
            self.lsp.state.diagnostic_count = (0, 0, 0, 0);
            self.lsp.state.diagnostics_file_path = None;
            return;
        };

        let Some(file_path) = self.buffer().file_path().map(str::to_string) else {
            self.lsp.state.clear_current_file_diagnostics();
            self.lsp.state.diagnostic_count = (0, 0, 0, 0);
            self.lsp.state.diagnostics_file_path = None;
            return;
        };

        let Some(uri) = uri_from_file_path(&file_path) else {
            self.lsp.state.clear_current_file_diagnostics();
            self.lsp.state.diagnostic_count = (0, 0, 0, 0);
            self.lsp.state.diagnostics_file_path = None;
            return;
        };

        let buffer_version = self.buffer().version();

        // If diagnostics are already in flight, Slot::fire() will cancel the
        // old request and start a fresh one. This is correct: if new diagnostics
        // arrived via publishDiagnostics while a fetch was in progress, the
        // in-flight fetch has stale data and should be replaced.

        let file_path_for_task = file_path.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let doc_version = lsp.get_document_version(&uri).await;
            let last_sent = lsp.get_last_sent_version(&uri).await;
            let diagnostics = if last_sent < doc_version {
                Vec::new()
            } else {
                lsp.get_diagnostics(&uri).await
            };
            let task_result = crate::editor::lsp_slot::DiagnosticResult {
                file_path: file_path_for_task,
                buffer_version,
                lsp_version: doc_version,
                lsp_sent_version: last_sent,
                count: diagnostic_counts(&diagnostics),
                diagnostics,
                deferred: last_sent < doc_version,
            };

            let _ = tx.send(Ok(task_result));
        });

        self.lsp.slots.diagnostics.fire(task, rx);
    }

    /// Poll background diagnostics refresh responses without blocking the UI tick.
    pub fn poll_pending_diagnostic_refresh_response(&mut self) -> bool {
        let timeout = std::time::Duration::from_secs(15);
        let Some(result) = self.lsp.slots.diagnostics.poll_with_timeout(timeout) else {
            return false;
        };

        match result {
            Ok(result) => {
                if result.deferred {
                    self.lsp.slots.diagnostics.invalidate();
                    return false;
                }

                // Wrong file — ignore entirely.
                if self.buffer().file_path() != Some(result.file_path.as_str()) {
                    self.lsp.slots.diagnostics.invalidate();
                    return false;
                }

                // Stamp version fields only after both guards: a refresh
                // spawned for file A that completes after switching to file B
                // must not poison B's version state (OV-00335). And only when
                // the buffer hasn't advanced since the refresh was spawned —
                // stamping an old lsp_version would move the tracked version
                // BACKWARD, letting a stale versioned workspace edit pass the
                // OV-00330 guard (external review finding).
                if self.buffer().version() == result.buffer_version {
                    self.lsp.state.current_file_lsp_version = result.lsp_version;
                    self.lsp.state.current_file_lsp_sent_version = result.lsp_sent_version;
                }

                // Always store and display the latest diagnostics — they're the
                // best data we have.  Showing slightly stale positions during
                // editing is better UX than hiding all feedback for 150ms+.
                // If the buffer changed since spawn, also request a fresh set.
                self.lsp.state.diagnostic_count = result.count;
                self.on_diagnostic_counts_changed(result.count.0, result.count.1);
                self.lsp
                    .state
                    .set_current_file_diagnostics(result.diagnostics);
                self.lsp.state.diagnostics_file_path = Some(result.file_path);

                // Build unified decorations from the new diagnostics.  Step E
                // anchors each decoration to the *current* buffer version at
                // placement time (Resolution A from the phase-05 plan): the
                // stored `char_offset` is computed against the current rope,
                // so `edits_since(current_version)` is empty and projection
                // trivially yields the stored offset. Edits that land after
                // placement fill the edit log and the projection replays them
                // forward at render time.
                let rope = self.buffer().rope().clone();
                let diag_source_version = self.buffer().version() as u64;
                // Anchor the raw diagnostics against the same rope/version so
                // squiggle/gutter/echo lookups replay the same edit-log
                // projection as the decorations below. (OV-00328)
                self.lsp
                    .state
                    .anchor_current_file_diagnostics(&rope, diag_source_version);
                let diag_decs = crate::editor::decoration::decorations_from_diagnostics(
                    &self.lsp.state.current_file_diagnostics,
                    &rope,
                    diag_source_version,
                );
                self.decorations.replace_source(
                    crate::editor::decoration::DecorationSource::Diagnostic,
                    diag_decs,
                    &rope,
                );

                if self.buffer().version() != result.buffer_version {
                    // Buffer was edited during the fetch — request a fresh set
                    // for the current content.  The stale diagnostics stay visible
                    // until the refresh completes (better than blank).
                    self.lsp.slots.diagnostics.invalidate();
                }

                true
            }
            Err(e) => {
                crate::lsp_warn!("LSP", "Diagnostics refresh failed: {}", e);
                self.lsp.slots.diagnostics.invalidate();
                false
            }
        }
    }

    /// Returns true if the cached diagnostics are for a different file.
    ///
    /// Content edits do NOT make diagnostics stale — showing slightly
    /// out-of-date diagnostics (possibly at wrong positions) is better UX
    /// than hiding all feedback for 150ms+ on every keystroke.  Fresh
    /// diagnostics replace stale ones atomically when the LSP responds.
    pub(crate) fn diagnostics_cache_stale(&self) -> bool {
        self.lsp.state.diagnostics_file_path.as_deref() != self.buffer().file_path()
    }

    /// How cached diagnostics map onto the current buffer.
    fn diagnostic_projection(&self) -> DiagnosticProjection<'_> {
        let Some(anchors) = self.lsp.state.diagnostic_anchors.as_ref() else {
            return DiagnosticProjection::RawLines;
        };
        if anchors.anchors.len() != self.lsp.state.current_file_diagnostics.len() {
            return DiagnosticProjection::RawLines;
        }
        match self.buffer().edit_log().edits_since(anchors.source_version) {
            Some(edits) if edits.is_empty() => DiagnosticProjection::RawLines,
            Some(edits) => DiagnosticProjection::Replay(anchors, edits),
            // History evicted: derive lines from the stored anchor offsets
            // against the CURRENT rope — the same fallback the eol
            // decorations use (`project_decoration` returns the stored
            // offset, `project_all` maps it with char_to_line). Falling
            // back to the raw LSP line here instead would put the squiggle
            // and the message on different lines exactly when history is
            // gone (external review finding on OV-00328).
            None => DiagnosticProjection::AnchorOffsets(anchors),
        }
    }

    /// True when per-line lookups must go through `project_diagnostics()`
    /// instead of the raw `diagnostics_by_line` index.
    fn diagnostics_need_projection(&self) -> bool {
        !matches!(self.diagnostic_projection(), DiagnosticProjection::RawLines)
    }

    /// Snapshot of the cached diagnostics with their ranges projected through
    /// the edit log onto the current buffer, grouped by projected line.
    ///
    /// This is the raw-diagnostic analogue of `DecorationMap::project_all`:
    /// the renderer builds it once per frame so the squiggle, gutter sign,
    /// and echo land on the same line as the projected EOL virtual text.
    /// (OV-00328)
    pub fn project_diagnostics(&self) -> ProjectedDiagnostics {
        if self.diagnostics_cache_stale() {
            return ProjectedDiagnostics::default();
        }
        let diagnostics = &self.lsp.state.current_file_diagnostics;
        let mut by_line: BTreeMap<usize, Vec<lsp_types::Diagnostic>> = BTreeMap::new();
        match self.diagnostic_projection() {
            DiagnosticProjection::Replay(anchors, edits) => {
                let rope = self.buffer().rope();
                for (diag, anchor) in diagnostics.iter().zip(&anchors.anchors) {
                    if let Some(projected) = project_diagnostic(diag, anchor, &edits, rope) {
                        by_line
                            .entry(projected.range.start.line as usize)
                            .or_default()
                            .push(projected);
                    }
                }
            }
            DiagnosticProjection::AnchorOffsets(anchors) => {
                // Eviction fallback: line = current rope's line at the
                // stored anchor offset, mirroring the decorations. Range
                // lines shift by the same delta so the squiggle stays on
                // the derived line (columns may drift until a fresh
                // publication lands — same policy as clamped replay).
                let rope = self.buffer().rope();
                for (diag, anchor) in diagnostics.iter().zip(&anchors.anchors) {
                    let offset = anchor.line_start.min(rope.len_chars());
                    let derived_line = rope.char_to_line(offset);
                    let original_line = diag.range.start.line as usize;
                    let mut projected = diag.clone();
                    let delta = derived_line as i64 - original_line as i64;
                    let shift = |line: u32| -> u32 { (line as i64 + delta).max(0) as u32 };
                    projected.range.start.line = shift(projected.range.start.line);
                    projected.range.end.line = shift(projected.range.end.line);
                    by_line.entry(derived_line).or_default().push(projected);
                }
            }
            DiagnosticProjection::RawLines => {
                for diag in diagnostics {
                    by_line
                        .entry(diag.range.start.line as usize)
                        .or_default()
                        .push(diag.clone());
                }
            }
        }
        ProjectedDiagnostics::new(by_line)
    }

    /// Get diagnostics for a specific line from cached diagnostics, with
    /// ranges projected through the edit log. (OV-00328)
    pub fn diagnostics_for_line(&self, line: usize) -> Vec<lsp_types::Diagnostic> {
        if self.diagnostics_cache_stale() {
            return Vec::new();
        }
        if self.diagnostics_need_projection() {
            return self.project_diagnostics().take_line(line);
        }
        let Some(indices) = self.lsp.state.diagnostics_by_line.get(&line) else {
            return Vec::new();
        };
        let diagnostics = &self.lsp.state.current_file_diagnostics;
        indices.iter().map(|&i| diagnostics[i].clone()).collect()
    }

    /// Returns true if any diagnostics are cached for the given line (in the
    /// projected view). Cheaper than building a Vec when there are no edits
    /// to replay.
    pub fn has_diagnostics_on_line(&self, line: usize) -> bool {
        if self.diagnostics_cache_stale() {
            return false;
        }
        if self.diagnostics_need_projection() {
            return !self.project_diagnostics().for_line(line).is_empty();
        }
        self.lsp.state.diagnostics_by_line.contains_key(&line)
    }

    /// Get the current diagnostic at the cursor position
    pub fn current_diagnostic(&self) -> Option<String> {
        self.diagnostic_nearest_to_cursor()
            .map(|diagnostic| diagnostic.message)
    }

    /// The full diagnostic nearest the cursor on the cursor line, if any.
    /// Used by the renderer to echo the message in the message line.
    pub fn diagnostic_at_cursor(&self) -> Option<lsp_types::Diagnostic> {
        self.diagnostic_nearest_to_cursor()
    }

    /// Get the total number of diagnostics
    pub fn diagnostic_count(&self) -> usize {
        if self.diagnostics_cache_stale() {
            return 0;
        }
        let diagnostics = &self.lsp.state.current_file_diagnostics;
        diagnostics.len()
    }

    /// Get all diagnostics for the current file
    pub fn all_diagnostics(&self) -> &[lsp_types::Diagnostic] {
        if self.diagnostics_cache_stale() {
            return &[];
        }
        &self.lsp.state.current_file_diagnostics
    }

    /// Show diagnostic at cursor in hover popup (like vim.diagnostic.open_float())
    pub fn show_diagnostic_at_cursor(&mut self) {
        use crate::mode::Mode;

        let line = self.buffer().cursor().line();
        let col = self.buffer().cursor().col().0;
        let Some(diagnostic) = self.diagnostic_nearest_to_cursor() else {
            self.set_lsp_status("No diagnostics at cursor".to_string());
            return;
        };

        // Format severity with markdown for nice rendering
        let severity_label = match diagnostic.severity {
            Some(lsp_types::DiagnosticSeverity::ERROR) => "Error",
            Some(lsp_types::DiagnosticSeverity::WARNING) => "Warning",
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => "Info",
            Some(lsp_types::DiagnosticSeverity::HINT) => "Hint",
            _ => "Diagnostic",
        };

        // Build markdown-formatted message
        // **Severity**: Message
        // Source: source (if available)
        let mut message = format!("**{}**: {}", severity_label, diagnostic.message);

        // Add source if available (e.g., "rustc", "clippy")
        if let Some(ref source) = diagnostic.source {
            message.push_str(&format!("\n\n`{}`", source));
        }

        // Add diagnostic code if available
        if let Some(ref code) = diagnostic.code {
            let code_str = match code {
                lsp_types::NumberOrString::Number(n) => n.to_string(),
                lsp_types::NumberOrString::String(s) => s.clone(),
            };
            message.push_str(&format!(" `{}`", code_str));
        }
        self.lsp.state.hover_info = Some(message);
        self.lsp.state.hover_position = Some((line, col));
        self.lsp.state.hover_content_type = crate::editor::lsp_state::HoverContentType::Diagnostic;
        self.set_mode(Mode::HoverPreview);
    }

    /// Pick the diagnostic under the cursor, or the nearest diagnostic on the
    /// same line. LSP columns are UTF-16 offsets while the editor cursor is
    /// grapheme-indexed, so both are normalized to scalar-value columns before
    /// comparing them.
    fn diagnostic_nearest_to_cursor(&self) -> Option<lsp_types::Diagnostic> {
        let line = self.buffer().cursor().line();
        let diagnostics = self.diagnostics_for_line(line);
        if diagnostics.is_empty() {
            return None;
        }

        let line_text: String = {
            let rope = self.buffer().rope();
            if line < rope.len_lines() {
                rope.line(line).chars().take_while(|&c| c != '\n').collect()
            } else {
                String::new()
            }
        };
        let col = crate::unicode::grapheme_to_char_col(&line_text, self.buffer().cursor().col()).0;

        diagnostics.into_iter().min_by_key(|diagnostic| {
            let start = crate::lsp::utf16_to_char_col(&line_text, diagnostic.range.start.character);
            let end = crate::lsp::utf16_to_char_col(&line_text, diagnostic.range.end.character);
            if col >= start && col <= end {
                0
            } else if col < start {
                start - col
            } else {
                col.saturating_sub(end)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::lsp_slot::DiagnosticResult;
    use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
    use tokio::sync::oneshot;

    fn diag(severity: Option<DiagnosticSeverity>) -> Diagnostic {
        Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            severity,
            message: "x".to_string(),
            ..Diagnostic::default()
        }
    }

    fn ranged_diag(start: u32, end: u32, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range::new(Position::new(0, start), Position::new(0, end)),
            message: message.to_string(),
            ..Diagnostic::default()
        }
    }

    #[test]
    fn diagnostic_counts_by_severity() {
        let diags = vec![
            diag(Some(DiagnosticSeverity::ERROR)),
            diag(Some(DiagnosticSeverity::WARNING)),
            diag(Some(DiagnosticSeverity::WARNING)),
            diag(Some(DiagnosticSeverity::INFORMATION)),
            diag(Some(DiagnosticSeverity::HINT)),
        ];
        assert_eq!(diagnostic_counts(&diags), (1, 2, 1, 1));
    }

    #[test]
    fn diagnostic_counts_missing_severity_is_error() {
        // OV-00270: a diagnostic with no severity counts as an error (matching
        // VS Code), not a warning — so the status line doesn't under-report
        // errors for servers that omit `severity`.
        let diags = vec![diag(None), diag(Some(DiagnosticSeverity::WARNING))];
        assert_eq!(diagnostic_counts(&diags), (1, 1, 0, 0));
    }

    #[test]
    fn current_diagnostic_uses_cursor_column_on_shared_line() {
        let mut editor = Editor::with_content("first second\n");
        editor.lsp.state.set_current_file_diagnostics(vec![
            ranged_diag(0, 5, "first diagnostic"),
            ranged_diag(6, 12, "second diagnostic"),
        ]);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(0, crate::unicode::GraphemeCol(9));

        assert_eq!(
            editor.current_diagnostic().as_deref(),
            Some("second diagnostic")
        );
    }

    #[test]
    fn current_diagnostic_normalizes_grapheme_and_utf16_columns() {
        let mut editor = Editor::with_content("👨‍👩‍👧‍👦 alpha beta\n");
        editor.lsp.state.set_current_file_diagnostics(vec![
            ranged_diag(12, 17, "alpha diagnostic"),
            ranged_diag(18, 22, "beta diagnostic"),
        ]);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(0, crate::unicode::GraphemeCol(8));

        assert_eq!(
            editor.current_diagnostic().as_deref(),
            Some("beta diagnostic")
        );
    }

    /// Helper: fire a pre-built `DiagnosticResult` into the diagnostics slot so
    /// that `poll_pending_diagnostic_refresh_response` can pick it up immediately.
    fn fire_diagnostic_result(editor: &mut Editor, result: DiagnosticResult) {
        let (tx, rx) = oneshot::channel::<anyhow::Result<DiagnosticResult>>();
        tx.send(Ok(result)).unwrap();
        let task = tokio::spawn(async {});
        editor.lsp.slots.diagnostics.fire(task, rx);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_diagnostic_refresh_response_applies_latest_result() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());

        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Example warning".to_string(),
            ..Diagnostic::default()
        };

        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: bv,
                lsp_version: 4,
                lsp_sent_version: 4,
                diagnostics: vec![diagnostic],
                count: (0, 1, 0, 0),
                deferred: false,
            },
        );

        assert!(editor.poll_pending_diagnostic_refresh_response());
        assert_eq!(editor.lsp.state.diagnostic_count, (0, 1, 0, 0));
        assert_eq!(editor.lsp.state.current_file_diagnostics.len(), 1);
        assert_eq!(editor.lsp.state.current_file_lsp_version, 4);
        assert_eq!(editor.lsp.state.current_file_lsp_sent_version, 4);
    }

    /// When the buffer is edited between spawning a diagnostic refresh and
    /// receiving the result, diagnostics should still be stored and displayed
    /// (stale data is better than no data), and a re-request should be scheduled.
    #[tokio::test(flavor = "current_thread")]
    async fn poll_keeps_diagnostics_when_buffer_edited_during_fetch() {
        use crate::editor::decoration::{
            Decoration, DecorationPlacement, DecorationSource, DecorationStyle,
        };

        let mut editor = Editor::with_content("let x = 1;\n");
        let file_path = "/tmp/test.rs".to_string();
        editor.set_file_path(file_path.clone());

        let initial_version = editor.buffer().version();

        // Simulate a diagnostic decoration already present from a prior refresh.
        let rope = editor.buffer().rope().clone();
        editor.decorations.replace_source(
            DecorationSource::Diagnostic,
            vec![Decoration {
                placement: DecorationPlacement::EndOfLine { char_offset: 0 },
                source: DecorationSource::Diagnostic,
                text: "old error".to_string(),
                display_width: 9,
                style: DecorationStyle::new(crate::color::Color::Red),
                priority: 0,
                source_version: 0,
            }],
            &rope,
        );
        assert_eq!(editor.decorations.for_line(0).len(), 1);

        // Build a result that was spawned at the initial buffer version.
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 4), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "unused variable".to_string(),
            ..Diagnostic::default()
        };

        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: initial_version,
                lsp_version: 2,
                lsp_sent_version: 2,
                diagnostics: vec![diagnostic],
                count: (1, 0, 0, 0),
                deferred: false,
            },
        );

        // Simulate a buffer edit AFTER the refresh was spawned.
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol::ZERO, "// ");

        assert_ne!(
            editor.buffer().version(),
            initial_version,
            "buffer version should have changed after edit"
        );

        // Poll should succeed (result ready) and detect the version mismatch.
        let changed = editor.poll_pending_diagnostic_refresh_response();
        assert!(changed);

        // File path still matches, so diagnostics are NOT stale (show-until-replaced).
        assert!(!editor.diagnostics_cache_stale());
        // A refresh should be requested for the current buffer content.
        assert!(editor.lsp.slots.diagnostics.is_stale());

        // Diagnostic decorations should PERSIST (stale data is better than blank).
        assert!(
            !editor.decorations.for_line(0).is_empty(),
            "diagnostic decorations should persist when buffer was edited during fetch"
        );
    }

    /// Verify that when the buffer hasn't changed, decorations ARE applied and
    /// diagnostics are marked valid.
    #[tokio::test(flavor = "current_thread")]
    async fn poll_applies_decorations_when_buffer_unchanged() {
        let mut editor = Editor::with_content("let x = 1;\n");
        let file_path = "/tmp/test.rs".to_string();
        editor.set_file_path(file_path.clone());

        let buffer_version = editor.buffer().version();

        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 4), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "unused variable".to_string(),
            ..Diagnostic::default()
        };

        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version,
                lsp_version: 2,
                lsp_sent_version: 2,
                diagnostics: vec![diagnostic],
                count: (1, 0, 0, 0),
                deferred: false,
            },
        );

        // No buffer edits — poll should apply decorations.
        let changed = editor.poll_pending_diagnostic_refresh_response();
        assert!(changed);
        assert!(!editor.diagnostics_cache_stale());

        // Decorations should be present.
        assert!(
            !editor.decorations.for_line(0).is_empty(),
            "diagnostic decorations should be applied when buffer is unchanged"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_diagnostic_refresh_response_requeues_deferred_result() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());

        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: bv,
                lsp_version: 5,
                lsp_sent_version: 4,
                diagnostics: Vec::new(),
                count: (0, 0, 0, 0),
                deferred: true,
            },
        );

        assert!(!editor.poll_pending_diagnostic_refresh_response());
        assert!(editor.lsp.slots.diagnostics.is_stale());
        assert!(editor.lsp.state.current_file_diagnostics.is_empty());
        // OV-00335: version fields are only stamped for results that pass
        // both the deferred and wrong-file guards.
        assert_eq!(editor.lsp.state.current_file_lsp_version, 0);
        assert_eq!(editor.lsp.state.current_file_lsp_sent_version, 0);
    }

    /// OV-00335: a refresh spawned for file A that completes after switching
    /// to file B must not stamp B's version fields with A's versions.
    #[tokio::test(flavor = "current_thread")]
    async fn poll_wrong_file_result_does_not_stamp_version_fields() {
        let mut editor = Editor::with_content("class B {}\n");
        editor.set_file_path("/tmp/B.java".to_string());
        editor.lsp.state.current_file_lsp_version = 7;
        editor.lsp.state.current_file_lsp_sent_version = 7;

        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path: "/tmp/A.java".to_string(),
                buffer_version: bv,
                lsp_version: 3,
                lsp_sent_version: 2,
                diagnostics: Vec::new(),
                count: (0, 0, 0, 0),
                deferred: false,
            },
        );

        assert!(!editor.poll_pending_diagnostic_refresh_response());
        assert_eq!(editor.lsp.state.current_file_lsp_version, 7);
        assert_eq!(editor.lsp.state.current_file_lsp_sent_version, 7);
    }

    /// OV-00328 regression: after a line-inserting edit recorded in the edit
    /// log, every raw-diagnostic consumer (squiggle/gutter/echo via
    /// `diagnostics_for_line` / `has_diagnostics_on_line`) must resolve to the
    /// SHIFTED line — the same line the projected EOL decoration moved to —
    /// not the diagnostic's original `range.start.line`.
    #[tokio::test(flavor = "current_thread")]
    async fn diagnostics_for_line_projects_through_line_shifting_edits() {
        let mut editor = Editor::with_content("fn a() {}\nfn b() {}\nlet bad = 1;\n");
        let file_path = "/tmp/test.rs".to_string();
        editor.set_file_path(file_path.clone());

        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 4), Position::new(2, 7)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "bad name".to_string(),
            ..Diagnostic::default()
        };
        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: bv,
                lsp_version: 1,
                lsp_sent_version: 1,
                diagnostics: vec![diagnostic],
                count: (1, 0, 0, 0),
                deferred: false,
            },
        );
        assert!(editor.poll_pending_diagnostic_refresh_response());
        assert!(editor.has_diagnostics_on_line(2));

        // Open a line above: everything below shifts down one line and the
        // edit log records the insert.
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol::ZERO, "// note\n");

        assert!(
            !editor.has_diagnostics_on_line(2),
            "original line must not keep a stale sign/squiggle"
        );
        assert!(
            editor.has_diagnostics_on_line(3),
            "diagnostic must follow its code to the shifted line"
        );
        let projected = editor.diagnostics_for_line(3);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].range.start.line, 3);
        assert_eq!(projected[0].range.start.character, 4);
        assert_eq!(projected[0].range.end.character, 7);
        assert!(editor.diagnostics_for_line(2).is_empty());

        // The cursor-line echo/float path agrees.
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(3, crate::unicode::GraphemeCol(4));
        assert_eq!(editor.current_diagnostic().as_deref(), Some("bad name"));

        // And the raw-diagnostic view lands on the same line as the projected
        // EOL decoration (the other rendering pipeline).
        let rope = editor.buffer().rope().clone();
        let eol = editor
            .decorations
            .eol_for_line_projected(3, &rope, editor.buffer().edit_log());
        assert_eq!(
            eol.len(),
            1,
            "eol decoration and diagnostics must agree on the shifted line"
        );
    }

    /// External review on OV-00328: when the edit-log ring has evicted the
    /// anchoring history, the raw-diagnostic fallback must derive its line
    /// from the stored anchor offset against the CURRENT rope — exactly the
    /// decorations' fallback — not the raw LSP line. Falling back to the raw
    /// line put the squiggle/gutter and the EOL message on different lines
    /// precisely when history was gone.
    #[tokio::test(flavor = "current_thread")]
    async fn eviction_fallback_agrees_with_decoration_line() {
        let mut editor = Editor::with_content("fn a() {}\nfn b() {}\nlet bad = 1;\n");
        let file_path = "/tmp/evict.rs".to_string();
        editor.set_file_path(file_path.clone());

        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 4), Position::new(2, 7)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "bad name".to_string(),
            ..Diagnostic::default()
        };
        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: bv,
                lsp_version: 1,
                lsp_sent_version: 1,
                diagnostics: vec![diagnostic],
                count: (1, 0, 0, 0),
                deferred: false,
            },
        );
        assert!(editor.poll_pending_diagnostic_refresh_response());

        // A long single-line insert above the diagnostic makes the stored
        // char-offset resolve to a DIFFERENT line than the raw LSP line,
        // then enough further edits evict the anchor version from the ring.
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol::ZERO, &"x".repeat(40));
        for _ in 0..70 {
            editor
                .buffer_mut()
                .insert_text_at(0, crate::unicode::CharCol::ZERO, "y");
        }

        // Whatever lines the two pipelines land on, they must AGREE.
        let rope = editor.buffer().rope().clone();
        let projected = editor.project_diagnostics();
        let diag_lines: Vec<usize> = (0..editor.buffer().line_count())
            .filter(|&line| !projected.for_line(line).is_empty())
            .collect();
        let eol_lines: Vec<usize> = (0..editor.buffer().line_count())
            .filter(|&line| {
                !editor
                    .decorations
                    .eol_for_line_projected(line, &rope, editor.buffer().edit_log())
                    .is_empty()
            })
            .collect();
        assert_eq!(
            diag_lines, eol_lines,
            "squiggle/gutter lines and eol-message lines must agree after ring eviction"
        );
        assert_eq!(diag_lines.len(), 1, "single diagnostic renders on one line");
    }

    /// OV-00328: same-line edits before the diagnostic shift its columns so
    /// the squiggle stays under the diagnosed code.
    #[tokio::test(flavor = "current_thread")]
    async fn diagnostics_for_line_projects_columns_within_line() {
        let mut editor = Editor::with_content("let bad = 1;\n");
        let file_path = "/tmp/test.rs".to_string();
        editor.set_file_path(file_path.clone());

        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 4), Position::new(0, 7)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "bad name".to_string(),
            ..Diagnostic::default()
        };
        let bv = editor.buffer().version();
        fire_diagnostic_result(
            &mut editor,
            DiagnosticResult {
                file_path,
                buffer_version: bv,
                lsp_version: 1,
                lsp_sent_version: 1,
                diagnostics: vec![diagnostic],
                count: (1, 0, 0, 0),
                deferred: false,
            },
        );
        assert!(editor.poll_pending_diagnostic_refresh_response());

        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol::ZERO, "// ");

        let projected = editor.diagnostics_for_line(0);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].range.start.character, 7);
        assert_eq!(projected[0].range.end.character, 10);
    }

    /// OV-00329: the per-line diagnostic fingerprint must change when the
    /// line's diagnostic set changes without a buffer edit (save →
    /// cargo-check republish adding a second span while the top message —
    /// the only thing the decoration hash sees — stays the same).
    #[test]
    fn projected_line_hash_changes_when_set_changes_without_edit() {
        let mut editor = Editor::with_content("let x = 1;\n");
        editor.set_test_diagnostics(vec![ranged_diag(4, 5, "top message")]);
        let h1 = editor.project_diagnostics().line_hash(0);

        editor.set_test_diagnostics(vec![
            ranged_diag(4, 5, "top message"),
            ranged_diag(8, 9, "top message"),
        ]);
        let h2 = editor.project_diagnostics().line_hash(0);

        assert_ne!(h1, h2, "new span on the line must change the fingerprint");
        // Untouched lines keep a stable fingerprint.
        assert_eq!(
            editor.project_diagnostics().line_hash(5),
            editor.project_diagnostics().line_hash(6)
        );
    }
}
