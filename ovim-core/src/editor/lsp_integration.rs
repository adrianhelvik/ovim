//! LSP Integration for Editor
//!
//! This module contains all LSP-related functionality extracted from the main editor module.
//! It provides LSP initialization, document synchronization, LSP actions, and workspace editing.

// Submodules for focused functionality
#[path = "lsp_modules/mod.rs"]
mod lsp_modules;

use super::*;
use crate::lsp::{uri_from_file_path, LspManager};

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn dedupe_key_for_status(status_lower: &str) -> String {
    status_lower
        .split(':')
        .next()
        .unwrap_or(status_lower)
        .trim()
        .to_string()
}

fn is_lsp_toast_candidate(status_lower: &str) -> bool {
    status_lower.starts_with("lsp:")
        || status_lower.starts_with("java:")
        || status_lower.contains("completion")
        || status_lower.contains("hover")
        || status_lower.contains("definition")
        || status_lower.contains("implementation")
        || status_lower.contains("code action")
        || status_lower.contains("semantic token")
        || status_lower.contains("workspace edit")
        || status_lower.contains("organize imports")
        || status_lower.contains("rename")
        || status_lower.contains("diagnostic")
}

struct StatusToast {
    level: ToastLevel,
    ttl: Duration,
    dedupe_key: String,
}

fn classify_status_toast(status: &str) -> Option<StatusToast> {
    if status.is_empty() {
        return None;
    }

    let lower = status.to_ascii_lowercase();
    if !is_lsp_toast_candidate(&lower) {
        return None;
    }

    if lower.contains("failed") || lower.contains("error") {
        return Some(StatusToast {
            level: ToastLevel::Error,
            ttl: Duration::from_secs(8),
            dedupe_key: dedupe_key_for_status(&lower),
        });
    }

    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("cancelled")
        || lower.contains("canceled")
    {
        return Some(StatusToast {
            level: ToastLevel::Warning,
            ttl: Duration::from_secs(6),
            dedupe_key: dedupe_key_for_status(&lower),
        });
    }

    None
}

/// Context for making an LSP request, encapsulating all the common setup.
pub(in crate::editor) struct LspRequestContext {
    pub lsp: Arc<crate::lsp::LspManager>,
    pub uri: lsp_types::Uri,
    pub file_path: String,
    pub line: u32,
    pub character: u32,
    pub language_id: String,
    /// All server_ids serving this language (primary + companions)
    pub server_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DocumentSyncRequestAction {
    Noop,
    DidOpen,
    QueueChangeAndFlush,
    FlushQueued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentSyncRequestPlan {
    action: DocumentSyncRequestAction,
    old_content: Option<Arc<str>>,
}

impl Editor {
    /// Enables LSP support
    pub fn enable_lsp(&mut self) {
        self.lsp.state.lsp_manager = Some(Arc::new(LspManager::new()));
    }

    /// Gets a reference to the LSP manager
    pub fn lsp_manager(&self) -> Option<Arc<LspManager>> {
        self.lsp.state.lsp_manager.clone()
    }

    /// Close the LSP for the current file
    pub async fn close_current_file_lsp(&mut self) {
        let Some(ref lsp) = self.lsp.state.lsp_manager else {
            return;
        };

        let Some(file_path) = self.buffer().file_path().map(|p| p.to_string()) else {
            return;
        };

        let uri = match uri_from_file_path(&file_path) {
            Some(u) => u,
            None => return,
        };

        // Get language_id from file extension
        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => return,
        };

        // Send LSP close notification
        let file_path_string = file_path.to_string();
        let _ = lsp.did_close_broadcast(uri, &language_id).await;
        self.lsp.state.document_sync.remove(&file_path_string);
    }

    /// Check if LSP initialization is needed for the current file
    pub fn needs_lsp_init(&self) -> Option<String> {
        if self.lsp.state.needs_lsp_init {
            self.buffer().file_path().map(|s| s.to_string())
        } else {
            None
        }
    }

    /// Clear the LSP initialization flag after init is complete
    pub fn clear_lsp_init_flag(&mut self) {
        self.lsp.state.needs_lsp_init = false;
    }

    /// Marks a document as having sent didOpen notification
    /// Used by LSP pre-warming to prevent duplicate didOpen
    pub fn mark_document_opened(&mut self, file_path: &str) {
        let state = self
            .lsp
            .state
            .document_sync
            .entry(file_path.to_string())
            .or_default();
        state.did_open_sent = true;

        // Server-state-changed boundary: document is now open on the server
        // without any local buffer mutation. Invalidate derived slots so the
        // next event-loop tick requests fresh hints/diagnostics.
        self.lsp.slots.inlay_hints.invalidate();
        self.lsp.slots.diagnostics.invalidate();
    }

    /// Marks a document as opened and synced (didOpen sent with this exact content).
    pub fn mark_document_opened_with_content(&mut self, file_path: &str, content: String) {
        self.mark_document_flushed(file_path, Arc::from(content), 1);

        // Server-state-changed boundary: document is now open on the server
        // without any local buffer mutation. Invalidate derived slots so the
        // next event-loop tick requests fresh hints/diagnostics.
        self.lsp.slots.inlay_hints.invalidate();
        self.lsp.slots.diagnostics.invalidate();
    }

    /// Request LSP initialization for the current file
    pub fn request_lsp_init(&mut self) {
        self.lsp.state.needs_lsp_init = true;
    }

    /// Set LSP status message
    pub fn set_lsp_status(&mut self, status: String) {
        self.lsp.state.status = status.clone();
        self.set_status_message(status.clone());

        if let Some(policy) = classify_status_toast(&status) {
            let request = ToastRequest::new(ToastSource::Lsp, policy.level, status)
                .with_title("LSP")
                .with_ttl(Some(policy.ttl))
                .with_dedupe_key(format!("lsp:{}", policy.dedupe_key));
            self.push_toast(request);
        }
    }

    /// Check if there's a pending LSP install awaiting user consent
    pub fn has_pending_lsp_install(&self) -> bool {
        self.lsp.pending_install.is_some()
    }

    /// Get a summary of the pending LSP install for display
    pub fn pending_lsp_install_summary(&self) -> Option<(String, String, String)> {
        self.lsp.pending_install.as_ref().map(|p| {
            (
                p.language_name.clone(),
                p.server_command.clone(),
                p.method_description.clone(),
            )
        })
    }

    /// Resolve the pending LSP install consent dialog
    pub fn resolve_pending_lsp_install(&mut self, consent: super::LspInstallConsent) {
        let pending = self.lsp.pending_install.take();
        match consent {
            super::LspInstallConsent::Yes => {
                if let Some(p) = &pending {
                    self.set_lsp_status(format!("LSP: Installing {}...", p.server_command));
                }
                // The actual install is triggered by the event loop checking
                // lsp_install_approved. Store the approved info.
                self.lsp.approved_install = pending;
            }
            super::LspInstallConsent::Always => {
                self.options.lsp_auto_install = super::AutoInstallMode::Auto;
                if let Some(p) = &pending {
                    self.set_lsp_status(format!("LSP: Installing {}...", p.server_command));
                }
                self.lsp.approved_install = pending;
            }
            super::LspInstallConsent::No => {
                if let Some(p) = &pending {
                    self.set_lsp_status(format!(
                        "LSP: {} skipped. Use :set autoinstall=prompt to re-enable.",
                        p.server_command
                    ));
                }
            }
        }
    }

    /// Take the approved LSP install info (consumed by the event loop)
    pub fn take_approved_lsp_install(&mut self) -> Option<super::PendingLspInstall> {
        self.lsp.approved_install.take()
    }

    /// Get current LSP status
    pub fn lsp_status(&self) -> &str {
        &self.lsp.state.status
    }

    fn document_sync_request_plan(
        &self,
        file_path: &str,
        current_content: &str,
    ) -> DocumentSyncRequestPlan {
        let Some(state) = self.lsp.state.document_sync.get(file_path) else {
            return DocumentSyncRequestPlan {
                action: DocumentSyncRequestAction::DidOpen,
                old_content: None,
            };
        };

        if !state.did_open_sent {
            return DocumentSyncRequestPlan {
                action: DocumentSyncRequestAction::DidOpen,
                old_content: None,
            };
        }

        let queued_current = state.queued_content() == Some(current_content);
        if state.is_modified() && queued_current {
            return DocumentSyncRequestPlan {
                action: DocumentSyncRequestAction::FlushQueued,
                old_content: None,
            };
        }

        let flushed_current = state.flushed_content() == Some(current_content);
        if state.is_modified() || !flushed_current {
            return DocumentSyncRequestPlan {
                action: DocumentSyncRequestAction::QueueChangeAndFlush,
                old_content: state.last_flushed_content.clone(),
            };
        }

        DocumentSyncRequestPlan {
            action: DocumentSyncRequestAction::Noop,
            old_content: None,
        }
    }

    fn mark_document_flushed(&mut self, file_path: &str, content: Arc<str>, flushed_version: i32) {
        let current_content = self
            .buffer()
            .file_path()
            .filter(|path| *path == file_path)
            .map(|_| self.buffer().rope().to_string());
        let state = self
            .lsp
            .state
            .document_sync
            .entry(file_path.to_string())
            .or_default();
        state.did_open_sent = true;
        state.mark_change_flushed(content, flushed_version, current_content.as_deref());

        // Intentionally no slot invalidation here. `mark_document_flushed`
        // has dual semantics — it's called both for "server just received
        // new content" (pre-warm, ensure_lsp_document_synced DidOpen) AND
        // for "we just recorded that an in-flight LSP result came back
        // with flushed content" (poll response success). The first case
        // needs invalidation, the second does not — putting it here would
        // re-mark a just-completed request as stale, forcing an immediate
        // re-fire of the same query. Invalidation for the first case
        // lives in `mark_document_opened` / `mark_document_opened_with_content`
        // (pre-warm) and next to `did_open_broadcast` in
        // `ensure_lsp_document_synced` (on-demand DidOpen).
    }

    /// Get a reference to the pending LSP intents.
    pub fn pending_intents(&self) -> &crate::editor::lsp_state::LspIntents {
        &self.lsp.intents
    }

    /// Invalidate hover cache when buffer is modified
    pub fn invalidate_hover_cache(&mut self) {
        if self.lsp.state.hover_cache.is_some() {
            self.lsp.state.hover_cache = None;
        }
    }

    /// Returns true if there's a pending LSP response being waited for
    pub fn has_pending_lsp_response(&self) -> bool {
        self.lsp.slots.any_pending()
    }

    pub fn has_pending_completion_response(&self) -> bool {
        self.lsp.slots.completion.is_pending()
    }

    pub fn has_pending_inlay_hint_response(&self) -> bool {
        self.lsp.slots.inlay_hints.is_pending()
    }

    /// Polls pending LSP responses (non-blocking)
    /// Returns true if a response was processed and UI should redraw
    ///
    /// Returns true if a hover response is pending (spawned but not yet received).
    pub fn has_pending_hover(&self) -> bool {
        self.lsp.slots.hover.is_pending()
    }

    /// Each response type is polled independently so that e.g. a hover request
    /// doesn't block or clobber a goto-definition request.
    ///
    /// Note: diagnostics and inlay hints are NOT polled here — they have
    /// dedicated sections earlier in the tick (sync_lsp_and_refresh_diagnostics
    /// and the inlay hints block) to ensure correct ordering with document sync.
    pub fn poll_pending_lsp_responses(&mut self) -> bool {
        let mut changed = false;

        // --- Navigation ---
        changed |= self.poll_hover_slot();
        changed |= self.poll_goto_slots();

        // --- Completion ---
        changed |= self.poll_pending_completion_response();

        // --- User-triggered actions ---
        changed |= self.poll_action_slots();

        changed
    }

    /// Poll the hover response slot.
    fn poll_hover_slot(&mut self) -> bool {
        let timeout = std::time::Duration::from_secs(10);
        let Some(result) = self.lsp.slots.hover.poll_with_timeout(timeout) else {
            return false;
        };

        match result {
            Ok(hover_result) => {
                if let Some(hover_text) = hover_result.hover_text {
                    crate::lsp_debug!("LSP-HOVER", "Received hover response");

                    let cursor = self.buffer().cursor();
                    let buffer_version = self.buffer().version();
                    let cursor_line = cursor.line();
                    let cursor_col = cursor.col().0;
                    let file_path = self.buffer().file_path().unwrap_or("").to_string();

                    self.lsp.state.hover_cache = Some(crate::editor::lsp_state::HoverCache::new(
                        file_path,
                        cursor_line,
                        cursor_col,
                        buffer_version,
                        hover_text.clone(),
                    ));

                    self.lsp.state.hover_info = Some(hover_text);
                    self.lsp.state.hover_scroll = 0;
                    self.lsp.state.hover_h_scroll = 0;
                    self.lsp.state.hover_position = Some((cursor_line, cursor_col));
                    self.lsp.state.hover_content_type =
                        crate::editor::lsp_state::HoverContentType::LspHover;
                    self.mode = crate::mode::Mode::HoverPreview;
                    self.mark_dirty();
                    self.set_lsp_status(String::new());
                    true
                } else if self.has_diagnostics_on_line(self.buffer().cursor().line()) {
                    // The server has nothing to say about the symbol, but the
                    // line has a diagnostic — that message is almost always
                    // what the user pressed hover to read. Show it instead of
                    // a dead-end "No hover info available" status (which also
                    // lingered and suppressed the message-line echo).
                    crate::lsp_debug!("LSP-HOVER", "No hover info; showing diagnostic");
                    self.show_diagnostic_at_cursor();
                    true
                } else {
                    crate::lsp_debug!("LSP-HOVER", "No hover info available");
                    self.set_lsp_status("No hover info available".to_string());
                    false
                }
            }
            Err(e) => {
                crate::lsp_debug!("LSP-HOVER", "Hover request failed: {:?}", e);
                self.set_lsp_status(format!("Hover failed: {}", e));
                false
            }
        }
    }

    /// Poll goto-definition, goto-implementation, and goto-type-definition
    /// slots (all use `Slot<GotoLocationResult>`).
    fn poll_goto_slots(&mut self) -> bool {
        let mut changed = false;

        // Helper: process a GotoLocationResult from any goto slot.
        // We poll each slot with a 10-second timeout to match the old behaviour.
        let timeout = std::time::Duration::from_secs(10);

        if let Some(result) = self.lsp.slots.goto_definition.poll_with_timeout(timeout) {
            changed |= self.handle_goto_slot_result(result, "Definition", "LSP-DEFINITION");
        }

        if let Some(result) = self
            .lsp
            .slots
            .goto_implementation
            .poll_with_timeout(timeout)
        {
            changed |= self.handle_goto_slot_result(result, "Implementation", "LSP-IMPLEMENTATION");
        }

        if let Some(result) = self
            .lsp
            .slots
            .goto_type_definition
            .poll_with_timeout(timeout)
        {
            changed |= self.handle_goto_slot_result(result, "Type", "LSP-TYPE");
        }

        changed
    }

    /// Apply the result from a goto slot — shared logic for definition,
    /// implementation, and type-definition.
    fn handle_goto_slot_result(
        &mut self,
        result: anyhow::Result<crate::editor::lsp_slot::GotoLocationResult>,
        label: &str,
        log_tag: &str,
    ) -> bool {
        match result {
            Ok(goto) => {
                // Reuse the existing handle_location_result_raw logic
                self.handle_goto_location(goto.location, label, log_tag, goto.new_tab)
            }
            Err(e) => {
                crate::lsp_debug!(log_tag, "{} request failed: {:?}", label, e);
                self.set_lsp_status(format!("{} failed: {}", label, e));
                false
            }
        }
    }

    /// Navigate to a location returned by a goto LSP request.
    fn handle_goto_location(
        &mut self,
        location: Option<lsp_types::Location>,
        label: &str,
        log_tag: &str,
        new_tab: bool,
    ) -> bool {
        match location {
            Some(location) => {
                crate::lsp_debug!(log_tag, "Received {} response", label.to_lowercase());

                let Some(path) = crate::lsp::uri_to_file_path(&location.uri) else {
                    self.set_lsp_status("Invalid file path in LSP response".to_string());
                    return false;
                };

                let target_line = location.range.start.line as usize;
                let target_character = location.range.start.character;

                self.push_tag();

                if new_tab {
                    self.new_tab();
                    match crate::buffer::Buffer::load_file(&path) {
                        Ok(mut buffer) => {
                            let modeline =
                                crate::modeline::Modeline::parse(&buffer.rope().to_string());
                            self.initialize_buffer_indent_options(&mut buffer);
                            self.buffers[self.current_buffer_index] = buffer;
                            if let Some(modeline) = modeline.as_ref() {
                                self.apply_modeline(modeline);
                            }
                            // The replacement buffer has a fresh id; repoint
                            // the tab at it
                            self.sync_current_tab_buffer();
                            if let Some(path) = self.buffer().file_path() {
                                self.registers.set_current_file(path.to_string());
                            }
                        }
                        Err(_) => {
                            self.set_lsp_status("Failed to open file".to_string());
                            return false;
                        }
                    }
                } else if self.buffer().file_path() != Some(path.to_string_lossy().as_ref())
                    && self.open_file(path.to_string_lossy().as_ref()).is_err()
                {
                    self.set_lsp_status("Failed to open file".to_string());
                    return false;
                }

                let target_col = self.utf16_to_grapheme_col(target_line, target_character);
                self.buffer_mut()
                    .cursor_mut()
                    .set_position(target_line, crate::unicode::GraphemeCol(target_col));
                self.buffer_mut().validate_cursor_position();
                self.center_cursor_in_viewport();
                let actual_col = self.buffer().cursor().col();

                let suffix = if new_tab { " (new tab)" } else { "" };
                self.set_lsp_status(format!(
                    "{}{}: {}:{}:{}",
                    label,
                    suffix,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    target_line + 1,
                    actual_col.0 + 1
                ));
                self.mark_dirty();
                true
            }
            None => {
                crate::lsp_debug!(log_tag, "No {} found", label.to_lowercase());
                self.set_lsp_status(format!("No {} found", label.to_lowercase()));
                false
            }
        }
    }

    /// True when a buffer-mutating LSP result computed against
    /// (`file_path`, `buffer_version`) can still be applied safely: same
    /// file, not a single edit since the request fired. Results carry
    /// positions into that exact snapshot — splicing them into a buffer the
    /// user kept editing garbles the text (OV-00327).
    fn lsp_mutation_target_is_current(&self, file_path: &str, buffer_version: usize) -> bool {
        self.buffer().file_path() == Some(file_path) && self.buffer().version() == buffer_version
    }

    /// Poll all action slots (Step 5 — format, references, symbols, code actions,
    /// rename, organize imports, call/type hierarchy, semantic tokens).
    fn poll_action_slots(&mut self) -> bool {
        let mut changed = false;
        let timeout = Duration::from_secs(15);

        // Format
        if let Some(result) = self.lsp.slots.format.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.edits.is_empty() => {
                    if self.lsp_mutation_target_is_current(&r.file_path, r.buffer_version) {
                        self.apply_lsp_edits(r.edits);
                        self.set_lsp_status("Document formatted".to_string());
                        changed = true;
                    } else {
                        self.set_lsp_status(
                            "Format result discarded: buffer changed (rerun format)".to_string(),
                        );
                    }
                }
                Ok(_) => {
                    self.set_lsp_status("No formatting changes".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Format request failed: {}", e));
                }
            }
        }

        // Find references
        if let Some(result) = self.lsp.slots.references.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.locations.is_empty() => {
                    let count = r.locations.len();
                    self.lsp.state.available_references = r.locations.clone();
                    self.lsp.state.active_lsp_result_type =
                        Some(crate::editor::LspResultType::References);
                    let items = self.locations_to_picker_items(&r.locations);
                    self.open_location_picker(items, "References");
                    self.set_lsp_status(format!("Found {} references", count));
                    changed = true;
                }
                Ok(_) => {
                    self.set_lsp_status("No references found".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("References request failed: {}", e));
                }
            }
        }

        // Document symbols
        if let Some(result) = self.lsp.slots.document_symbols.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.symbols.is_empty() => {
                    let count = r.symbols.len();
                    self.lsp.state.available_document_symbols = r.symbols.clone();
                    self.lsp.state.active_lsp_result_type =
                        Some(crate::editor::LspResultType::DocumentSymbols);
                    let file_path = r.file_path;
                    let items: Vec<crate::editor::picker::PickerResult> = r
                        .symbols
                        .iter()
                        .map(|sym| {
                            let line = sym.range.start.line as usize;
                            let col = self.utf16_to_grapheme_col(line, sym.range.start.character);
                            crate::editor::picker::PickerResult {
                                display: format!(
                                    "{}:{}:{} {}",
                                    file_path,
                                    line + 1,
                                    col + 1,
                                    sym.name
                                ),
                                location: file_path.to_string(),
                                line,
                                col,
                                match_positions: Vec::new(),
                                content: None,
                            }
                        })
                        .collect();
                    self.open_location_picker(items, "Document Symbols");
                    self.set_lsp_status(format!("Found {} symbols", count));
                    changed = true;
                }
                Ok(_) => {
                    self.set_lsp_status("No symbols found".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Document symbols request failed: {}", e));
                }
            }
        }

        // Workspace symbols
        if let Some(result) = self.lsp.slots.workspace_symbols.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.symbols.is_empty() => {
                    let count = r.symbols.len();
                    self.lsp.state.available_workspace_symbols = r.symbols.clone();
                    self.lsp.state.active_lsp_result_type =
                        Some(crate::editor::LspResultType::WorkspaceSymbols);
                    let items: Vec<crate::editor::picker::PickerResult> = r
                        .symbols
                        .iter()
                        .filter_map(|sym| {
                            let path = crate::lsp::uri_to_file_path(&sym.location.uri)?;
                            let line = sym.location.range.start.line as usize;
                            let col = self
                                .utf16_to_grapheme_col(line, sym.location.range.start.character);
                            Some(crate::editor::picker::PickerResult {
                                display: format!(
                                    "{}:{}:{}",
                                    path.file_name().unwrap_or_default().to_string_lossy(),
                                    line + 1,
                                    col + 1
                                ),
                                location: path.to_string_lossy().to_string(),
                                line,
                                col,
                                match_positions: Vec::new(),
                                content: None,
                            })
                        })
                        .collect();
                    self.open_location_picker(items, "Workspace Symbols");
                    self.set_lsp_status(format!("Found {} symbols", count));
                    changed = true;
                }
                Ok(_) => {
                    self.set_lsp_status("No workspace symbols found".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Workspace symbols request failed: {}", e));
                }
            }
        }

        // Code actions
        if let Some(result) = self.lsp.slots.code_actions.poll_with_timeout(timeout) {
            match result {
                Ok(r)
                    if !r.actions.is_empty()
                        && !self.lsp_mutation_target_is_current(&r.file_path, r.buffer_version) =>
                {
                    // The action edits embed positions into the request-time
                    // snapshot; the buffer has moved on (OV-00327).
                    self.set_lsp_status(
                        "Code actions discarded: buffer changed (rerun)".to_string(),
                    );
                }
                Ok(r) if !r.actions.is_empty() => {
                    let titles: Vec<String> = r
                        .actions
                        .iter()
                        .map(|a| lsp_modules::actions::code_action_title(&a.action))
                        .collect();
                    self.lsp.state.available_code_actions = r.actions;
                    let base_dir =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let picker = crate::editor::picker::Picker::new_custom(base_dir, titles);
                    self.set_picker(picker);
                    self.set_mode(crate::mode::Mode::Picker);
                    self.mark_picker_selection_changed();
                    changed = true;
                }
                Ok(_) => {
                    self.set_lsp_status("No code actions available".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Code actions request failed: {}", e));
                }
            }
        }

        // Rename
        if let Some(result) = self.lsp.slots.rename.poll_with_timeout(timeout) {
            match result {
                Ok(r) => {
                    if let Some(workspace_edit) = r.edit {
                        if !self.lsp_mutation_target_is_current(&r.file_path, r.buffer_version) {
                            // Positions in the edit are stale (OV-00327).
                            self.set_lsp_status(
                                "Rename discarded: buffer changed (rerun rename)".to_string(),
                            );
                        } else {
                            match self.apply_workspace_edit(workspace_edit) {
                                Ok(true) => {
                                    self.set_lsp_status(format!("Renamed to '{}'", r.new_name));
                                    changed = true;
                                }
                                Ok(false) => {
                                    self.set_lsp_status("Rename failed to apply".to_string());
                                }
                                Err(e) => {
                                    self.set_lsp_status(format!("Failed to apply rename: {}", e));
                                }
                            }
                        }
                    } else {
                        self.set_lsp_status("Rename not available at this location".to_string());
                    }
                }
                Err(e) => {
                    self.set_lsp_status(format!("Rename request failed: {}", e));
                }
            }
        }

        // Organize imports
        if let Some(result) = self.lsp.slots.organize_imports.poll_with_timeout(timeout) {
            match result {
                Ok(r) => {
                    if let Some(action) = r.action {
                        if !self.lsp_mutation_target_is_current(&r.file_path, r.buffer_version) {
                            // Positions in the action edit are stale (OV-00327).
                            self.set_lsp_status(
                                "Organize imports discarded: buffer changed (rerun)".to_string(),
                            );
                        } else {
                            self.lsp.state.available_code_actions = vec![action];
                            self.apply_code_action(0);
                            self.set_lsp_status("Imports organized".to_string());
                            changed = true;
                        }
                    } else {
                        self.set_lsp_status("No organize imports action available".to_string());
                    }
                }
                Err(e) => {
                    self.set_lsp_status(format!("Organize imports failed: {}", e));
                }
            }
        }

        // Call hierarchy
        if let Some(result) = self.lsp.slots.call_hierarchy.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.locations.is_empty() => {
                    let count = r.locations.len();
                    let direction_label = match r.direction {
                        crate::editor::lsp_slot::CallHierarchyDirection::Incoming => {
                            "Incoming Calls"
                        }
                        crate::editor::lsp_slot::CallHierarchyDirection::Outgoing => {
                            "Outgoing Calls"
                        }
                    };
                    self.store_call_hierarchy(&r.locations);
                    let picker_items = self.locations_to_picker_items(&r.locations);
                    self.open_location_picker(picker_items, direction_label);
                    self.set_lsp_status(format!(
                        "Found {} {}",
                        count,
                        direction_label.to_lowercase()
                    ));
                    changed = true;
                }
                Ok(r) => {
                    let msg = match r.direction {
                        crate::editor::lsp_slot::CallHierarchyDirection::Incoming => {
                            "No incoming calls found"
                        }
                        crate::editor::lsp_slot::CallHierarchyDirection::Outgoing => {
                            "No outgoing calls found"
                        }
                    };
                    self.set_lsp_status(msg.to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Call hierarchy request failed: {}", e));
                }
            }
        }

        // Type hierarchy
        if let Some(result) = self.lsp.slots.type_hierarchy.poll_with_timeout(timeout) {
            match result {
                Ok(r) if !r.all_locations.is_empty() => {
                    let count = r.all_locations.len();
                    self.lsp.state.available_type_hierarchy = r.types;
                    self.lsp.state.active_lsp_result_type =
                        Some(crate::editor::LspResultType::TypeHierarchy);
                    let picker_items = self.locations_to_picker_items(&r.all_locations);
                    self.open_location_picker(picker_items, "Type Hierarchy");
                    self.set_lsp_status(format!("Found {} types", count));
                    changed = true;
                }
                Ok(_) => {
                    self.set_lsp_status("No type hierarchy found".to_string());
                }
                Err(e) => {
                    self.set_lsp_status(format!("Type hierarchy request failed: {}", e));
                }
            }
        }

        // Semantic tokens
        if let Some(result) = self.lsp.slots.semantic_tokens.poll_with_timeout(timeout) {
            match result {
                Ok(r) => {
                    if let Some(tokens) = r.tokens {
                        if let Some(legend) = r.legend {
                            self.buffer_mut().decode_semantic_tokens(&tokens, &legend);
                            self.set_lsp_status("Semantic tokens applied".to_string());
                        } else {
                            self.set_lsp_status(
                                "Semantic tokens received (no legend available)".to_string(),
                            );
                        }
                        changed = true;
                    } else {
                        self.set_lsp_status("No semantic tokens available".to_string());
                    }
                }
                Err(e) => {
                    self.set_lsp_status(format!("Semantic tokens request failed: {}", e));
                }
            }
        }

        changed
    }

    /// Poll completion responses (non-blocking)
    /// Returns true if a response was processed and UI should redraw
    pub fn poll_pending_completion_response(&mut self) -> bool {
        let timeout = Duration::from_secs(3);
        let Some(result) = self.lsp.slots.completion.poll_with_timeout(timeout) else {
            return false;
        };

        match result {
            Ok(result) => {
                if self.mode() != crate::mode::Mode::Insert {
                    self.hide_completion_menu();
                    return false;
                }

                // Drop responses that arrived after the user switched files.
                // Without this, completions from file A leak into file B's menu.
                let matches_file = self
                    .buffer()
                    .file_path()
                    .is_some_and(|path| path == result.file_path);
                if !matches_file {
                    self.hide_completion_menu();
                    return false;
                }

                // Drop responses that arrived after the buffer has been edited
                // further. The mode check above is not sufficient: the user
                // can Esc out and re-enter insert mode before the response
                // arrives, keeping mode=Insert while the context has shifted.
                if result.buffer_version != self.buffer().version() {
                    self.hide_completion_menu();
                    return false;
                }

                if let (Some(synced), Some(flushed_version)) =
                    (result.synced_content, result.synced_lsp_version)
                {
                    self.mark_document_flushed(
                        &result.file_path,
                        Arc::from(synced),
                        flushed_version,
                    );
                }

                let (trigger_col, trigger_prefix) = self.derive_completion_prefix(&result.items);
                let items_version = result.buffer_version;
                let menu = self.completion_menu_mut();
                menu.show(result.items.clone(), trigger_col, trigger_prefix);
                // The version check above proved the items' textEdit ranges
                // target the buffer as it is right now (OV-00327).
                menu.set_items_buffer_version(items_version);
                self.lsp.state.available_completions = result.items;
                self.mark_dirty();
                true
            }
            Err(e) => {
                self.hide_completion_menu();
                self.set_lsp_status(format!("Completion failed: {}", e));
                self.mark_dirty();
                true
            }
        }
    }

    /// Poll inlay hint responses (non-blocking)
    /// Returns true if a response was processed and UI should redraw
    pub fn poll_pending_inlay_hint_response(&mut self) -> bool {
        let timeout = Duration::from_secs(5);
        let Some(result) = self.lsp.slots.inlay_hints.poll_with_timeout(timeout) else {
            return false;
        };

        match result {
            Ok(result) => {
                // File-scoped hints: only check that the file matches.
                // Scroll position is irrelevant since hints cover the full file.
                let matches_file = self
                    .buffer()
                    .file_path()
                    .is_some_and(|path| path == result.request_key.file_path);
                if !matches_file {
                    self.invalidate_inlay_hint_debounce();
                    return false;
                }

                if result.request_key.lsp_version < self.lsp.state.current_file_lsp_sent_version {
                    self.invalidate_inlay_hint_debounce();
                    return false;
                }

                // OV-00258: drop stale-buffer hints rather than placing them
                // mis-aligned. The LSP returned `Position {line, character}`
                // pairs computed against `result.buffer_version`. If the
                // buffer has advanced since (the user kept typing), those
                // positions index into a different rope's content — a
                // `line_to_char(line) + char_idx` lookup against the current
                // rope can land in the middle of a completely different
                // identifier or even a comment line that has no hints at all
                // (the user reported `r:string|undefined` sliced into
                // `awEmbedParam` and `s:boolean` injected into a `// SSR`
                // comment).
                //
                // Why drop instead of project:
                // - Option (2): tag decorations with `source_version =
                //   result.buffer_version` and project forward via
                //   `edit_log.edits_since(...)`. Requires either rope
                //   snapshots at request boundaries or backward edit-log
                //   replay to derive historical line→char mappings. Larger
                //   lift; not justified while inlay hints are off-by-default
                //   per OV-00259.
                // - Option (3): project the LSP `Position`s through the
                //   edit log before converting to char offsets. Same
                //   complexity; same gating.
                //
                // Dropping is correct (zero mis-aligned placements) and
                // cheap; the trailing `invalidate_inlay_hint_debounce()`
                // ensures the next tick fires a fresh request against the
                // current buffer. When OV-00257 is closed and inlay hints
                // are re-enabled by default, revisit (2)/(3) as a quality
                // bump.
                if result.buffer_version != self.buffer().version() {
                    self.invalidate_inlay_hint_debounce();
                    return false;
                }

                if let (Some(synced), Some(flushed_version)) =
                    (result.synced_content, result.synced_lsp_version)
                {
                    self.mark_document_flushed(
                        &result.request_key.file_path,
                        Arc::from(synced),
                        flushed_version,
                    );
                }

                self.lsp.state.current_file_lsp_version = result.request_key.lsp_version;
                self.lsp.state.current_file_lsp_sent_version = result.request_key.lsp_version;
                self.lsp.state.inlay_hints = result.hints;
                // Build decorations from the new hints, anchored to the
                // *current* buffer version: the stored `char_offset` is
                // computed against the current rope, so `edits_since(current)`
                // is empty at placement and the projected accessors yield the
                // stored offset as-is. Later edits fill the edit log and the
                // projection replays them forward. See `project_decoration`.
                let rope = self.buffer().rope().clone();
                let hint_source_version = self.buffer().version() as u64;
                let hint_decs = crate::editor::decoration::decorations_from_inlay_hints(
                    &self.lsp.state.inlay_hints,
                    &rope,
                    // Canonical "text the user sees on this line" — strips the
                    // line terminator (`\n` / `\r\n` / bare `\r`), so UTF-16
                    // → char-index conversion never miscounts a terminator and
                    // anchors a hint onto the next line. (OV-00265)
                    |line_idx| {
                        self.buffer()
                            .line_text(line_idx)
                            .map(|c| c.into_owned())
                            .unwrap_or_default()
                    },
                    hint_source_version,
                );
                self.decorations.replace_source(
                    crate::editor::decoration::DecorationSource::InlayHint,
                    hint_decs,
                    &rope,
                );

                self.mark_dirty();
                true
            }
            Err(_) => {
                self.invalidate_inlay_hint_debounce();
                false
            }
        }
    }

    /// Mark inlay hints as stale so the next tick re-requests them.
    /// Called when a poll result is dropped (wrong file, stale version)
    /// or when the buffer version changes.
    fn invalidate_inlay_hint_debounce(&mut self) {
        self.lsp.slots.inlay_hints.invalidate();
    }

    /// Legacy handler — delegates to `handle_goto_location`.
    /// Kept only for test compatibility; will be removed once all callers migrate.
    #[cfg(test)]
    fn handle_location_result(
        &mut self,
        result: anyhow::Result<Option<lsp_types::Location>>,
        label: &str,
        log_tag: &str,
        new_tab: bool,
    ) -> bool {
        match result {
            Ok(loc) => self.handle_goto_location(loc, label, log_tag, new_tab),
            Err(e) => {
                crate::lsp_debug!(log_tag, "{} request failed: {:?}", label, e);
                self.set_lsp_status(format!("{} failed: {}", label, e));
                false
            }
        }
    }

    /// Register a new LSP server
    pub fn register_lsp_server(&mut self, language_id: String, server_name: String) {
        let status = format!("LSP: {server_name} ready");
        self.lsp
            .state
            .active_lsp_servers
            .insert(language_id, server_name);
        self.set_lsp_status(status);
    }

    /// Unregister an LSP server
    pub fn unregister_lsp_server(&mut self, language_id: &str) {
        self.lsp.state.active_lsp_servers.remove(language_id);
        if self.lsp.state.active_lsp_servers.is_empty() {
            let status_was_visible = self.status_message() == self.lsp_status();
            self.lsp.state.status.clear();
            if status_was_visible {
                self.clear_status_message();
            }
        }
    }

    /// Clear file-scoped LSP state while retaining the global server registry.
    pub(crate) fn clear_lsp_state(&mut self) {
        let lsp_status_was_visible =
            !self.lsp.state.status.is_empty() && self.status_message() == self.lsp_status();
        self.lsp.state.status.clear();
        if lsp_status_was_visible {
            self.clear_status_message();
        }

        self.lsp.state.diagnostic_count = (0, 0, 0, 0);
        self.lsp.state.hover_info = None;
        self.lsp.state.hover_scroll = 0;
        self.lsp.state.hover_h_scroll = 0;
        self.lsp.state.hover_position = None;
        self.lsp.state.available_code_actions.clear();
        self.lsp.state.available_completions.clear();
        self.lsp.state.available_references.clear();
        self.lsp.state.available_document_symbols.clear();
        self.lsp.state.available_workspace_symbols.clear();
        self.lsp.state.available_call_hierarchy.clear();
        self.lsp.state.available_type_hierarchy.clear();
        self.lsp.state.active_lsp_result_type = None;
        self.lsp.state.inlay_hints.clear();
        // Drop the cached diagnostic vector too — it belongs to the file we're
        // leaving. The decorations get wiped below regardless, but leaving the
        // raw vector populated is an inconsistency with how inlay_hints is
        // treated. (OV-00269)
        self.lsp.state.clear_current_file_diagnostics();
        self.lsp.slots.inlay_hints.cancel_and_invalidate();
        self.lsp.intents.clear();
        self.lsp.slots.cancel_all();
        self.lsp.state.hover_cache = None;
        self.completion_menu.hide();
        // Reset LSP version tracking (new file has its own version space)
        self.lsp.state.current_file_lsp_version = 0;
        self.lsp.state.current_file_lsp_sent_version = 0;
        self.lsp.state.diagnostics_file_path = None;
        self.decorations.clear();
    }

    /// Get active LSP servers map
    pub fn active_lsp_servers(&self) -> &HashMap<String, String> {
        &self.lsp.state.active_lsp_servers
    }

    /// Running server for the current buffer's language, if one is active.
    pub fn current_lsp_server_name(&self) -> Option<&str> {
        let language_id = self
            .buffer()
            .file_path()
            .and_then(|path| self.language_id_for_path(path))?;
        self.lsp
            .state
            .active_lsp_servers
            .get(&language_id)
            .map(String::as_str)
    }

    /// Get LSP progress message (e.g., "indexing...")
    pub fn lsp_progress_message(&self) -> Option<String> {
        if let Some(lsp_manager) = &self.lsp.state.lsp_manager {
            lsp_manager.get_progress_message()
        } else {
            None
        }
    }

    /// Get LSP info for status line
    pub fn get_lsp_info(&self) -> String {
        let mut info = String::new();

        // LSP Manager status
        if self.lsp.state.lsp_manager.is_some() {
            info.push_str("LSP: enabled\n");
        } else {
            info.push_str("LSP: disabled\n");
        }

        // Active servers
        if self.lsp.state.active_lsp_servers.is_empty() {
            info.push_str("Servers: none\n");
        } else {
            info.push_str("Servers:\n");
            for (lang_id, server_name) in &self.lsp.state.active_lsp_servers {
                info.push_str(&format!("  - {} ({})\n", server_name, lang_id));
            }
        }

        // Progress messages
        if let Some(progress) = self.lsp_progress_message() {
            info.push_str(&format!("Progress: {}\n", progress));
        }

        // Diagnostic counts
        let (errors, warnings, infos, hints) = self.lsp.state.diagnostic_count;
        info.push_str(&format!(
            "Diagnostics: E:{} W:{} I:{} H:{}\n",
            errors, warnings, infos, hints
        ));

        // Current status
        if !self.lsp_status().is_empty() {
            info.push_str(&format!("\nStatus: {}\n", self.lsp_status()));
        }

        info
    }

    // -------------------------------------------------------------------------
    // LSP Action Requests (set per-feature intent flags)
    // -------------------------------------------------------------------------

    /// Request document format
    pub fn request_format_document(&mut self) {
        self.lsp.intents.format_document = true;
    }

    /// Request code actions at current cursor position
    pub fn request_code_actions(&mut self) {
        self.lsp.intents.code_actions = true;
    }

    /// Request call hierarchy (incoming calls) at current cursor position
    pub fn request_call_hierarchy_incoming(&mut self) {
        self.lsp.intents.call_hierarchy_incoming = true;
    }

    /// Request call hierarchy (outgoing calls) at current cursor position
    pub fn request_call_hierarchy_outgoing(&mut self) {
        self.lsp.intents.call_hierarchy_outgoing = true;
    }

    /// Request type hierarchy at current cursor position
    pub fn request_type_hierarchy(&mut self) {
        self.lsp.intents.type_hierarchy = true;
    }

    /// Request organize imports for the current document
    pub fn request_organize_imports(&mut self) {
        self.lsp.intents.organize_imports = true;
    }

    /// Request find references at current cursor position
    pub fn request_find_references(&mut self) {
        self.lsp.intents.find_references = true;
    }

    /// Request document symbols for the current document
    pub fn request_document_symbols(&mut self) {
        self.lsp.intents.document_symbols = true;
    }

    /// Request workspace symbols
    pub fn request_workspace_symbols(&mut self) {
        self.lsp.intents.workspace_symbols = true;
    }

    /// Request rename at current cursor position
    pub fn request_rename(&mut self, new_name: String) {
        self.lsp.intents.rename = Some(new_name);
    }

    /// Request semantic tokens for the current document
    pub fn request_semantic_tokens(&mut self) {
        self.lsp.intents.semantic_tokens = true;
    }

    fn document_sync_state_mut(&mut self) -> Option<&mut lsp_state::DocumentSyncState> {
        let file_path = self.buffer().file_path()?.to_string();
        Some(self.lsp.state.document_sync.entry(file_path).or_default())
    }

    fn reconcile_document_sync_with_manager(
        &mut self,
        file_path: &str,
        current_content: Option<&str>,
        manager_version: i32,
        sent_version: i32,
    ) {
        if manager_version <= 0 {
            return;
        }

        self.lsp.state.current_file_lsp_version = manager_version;
        self.lsp.state.current_file_lsp_sent_version = sent_version;

        let state = self
            .lsp
            .state
            .document_sync
            .entry(file_path.to_string())
            .or_default();

        if sent_version > 0 && !state.did_open_sent {
            state.did_open_sent = true;
        }

        if sent_version > 0 && state.last_flushed_content.is_none() && !state.force_full_resend {
            let seeded_content = state
                .last_queued_content
                .clone()
                .or_else(|| current_content.map(Arc::from));
            if let Some(content) = seeded_content {
                state.mark_change_flushed(content, sent_version, current_content);
            }
        }

        if state
            .target_lsp_version
            .is_some_and(|target_version| sent_version >= target_version)
        {
            let flushed_content = state
                .last_queued_content
                .clone()
                .or_else(|| current_content.map(Arc::from));
            if let Some(content) = flushed_content {
                state.mark_change_flushed(content, sent_version, current_content);
            }
        }
    }

    pub async fn refresh_current_lsp_sync_versions(&mut self) {
        let Some(lsp) = self.lsp.state.lsp_manager.clone() else {
            self.lsp.state.current_file_lsp_version = 0;
            self.lsp.state.current_file_lsp_sent_version = 0;
            return;
        };

        let Some(file_path) = self.buffer().file_path().map(str::to_string) else {
            self.lsp.state.current_file_lsp_version = 0;
            self.lsp.state.current_file_lsp_sent_version = 0;
            return;
        };

        let Some(uri) = crate::lsp::uri_from_file_path(&file_path) else {
            self.lsp.state.current_file_lsp_version = 0;
            self.lsp.state.current_file_lsp_sent_version = 0;
            return;
        };

        let manager_version = lsp.get_document_version(&uri).await;
        let sent_version = lsp.get_last_sent_version(&uri).await;
        let needs_content = self
            .lsp
            .state
            .document_sync
            .get(&file_path)
            .is_some_and(|state| {
                (sent_version > 0 && state.last_flushed_content.is_none())
                    || state
                        .target_lsp_version
                        .is_some_and(|target_version| sent_version >= target_version)
            });
        let current_content = needs_content.then(|| self.buffer().rope().to_string());
        self.reconcile_document_sync_with_manager(
            &file_path,
            current_content.as_deref(),
            manager_version,
            sent_version,
        );
    }

    /// Mark buffer as modified (for LSP didChange tracking).
    ///
    /// This is the **canonical "buffer just mutated" hook**. Every path that
    /// touches buffer content (operators, insert-mode keystrokes, undo/redo,
    /// completion accept, substitute, workspace-edit apply, dot-repeat, etc.)
    /// routes through here. That makes it the natural home for invalidating
    /// derived LSP state that becomes stale whenever the buffer changes —
    /// specifically, inlay hints and diagnostics.
    ///
    /// Hoisting the invalidation here (rather than inside `send_lsp_changes_if_modified`)
    /// fixes a class of silent staleness bugs: when buffer content equals
    /// the last-flushed content (e.g. type-then-backspace, or any undo that
    /// returns the document to a flushed state), the send path takes an
    /// early return and the slot invalidation downstream was never reached.
    /// Here the invalidation is unconditional on mutation, independent of
    /// document-sync state. `TrackedSlot`'s debounce absorbs tight loops.
    pub fn mark_buffer_modified(&mut self) {
        if let Some(state) = self.document_sync_state_mut() {
            state.mark_modified();
        }
        self.lsp.slots.inlay_hints.invalidate();
        self.lsp.slots.diagnostics.invalidate();
    }

    pub fn mark_buffer_modified_force_send(&mut self) {
        if let Some(state) = self.document_sync_state_mut() {
            state.mark_modified();
            // Clear flushed content so the next sync sends a full document
            // update rather than an incremental diff.  This is critical after
            // `:e!` (reload from disk): if a prior desync corrupted
            // last_flushed_content, incremental diffs against it would produce
            // further incorrect updates.
            state.last_flushed_content = None;
            // Clearing last_flushed_content alone is not enough: the manager
            // reconcile treats "None" as "unknown, assume in sync" and re-seeds
            // it with the CURRENT buffer content, after which the no-op guard
            // in send_lsp_changes_if_modified compares equal and silently
            // drops the update — the server keeps analyzing the pre-reload
            // text and its stale diagnostics never clear. (OV-00324)
            state.force_full_resend = true;
        }
        self.lsp.slots.inlay_hints.invalidate();
        self.lsp.slots.diagnostics.invalidate();
    }

    pub fn request_diagnostics_refresh(&mut self) {
        self.lsp.slots.diagnostics.invalidate();
    }

    /// Canonical hook called after a successful `didSave` broadcast.
    ///
    /// This is a server-state-changed boundary that does NOT ride through
    /// `mark_buffer_modified` (the buffer didn't mutate), so it needs its
    /// own explicit invalidation of the slot generations. Without this,
    /// servers that re-analyze on save (e.g. rust-analyzer's cargo check)
    /// produce diagnostics/hints that never get re-requested.
    pub fn on_lsp_save_sent(&mut self, file_path: &str) {
        let state = self
            .lsp
            .state
            .document_sync
            .entry(file_path.to_string())
            .or_default();
        state.mark_save_sent();
        self.lsp.slots.inlay_hints.invalidate();
        self.lsp.slots.diagnostics.invalidate();
    }

    /// Sync pending edits/saves to the LSP server, then poll and refresh
    /// diagnostics.  Colocating these operations enforces the invariant that
    /// the server always has the latest content before we check for fresh
    /// diagnostics — preventing the one-tick-behind staleness bug where
    /// diagnostics were fetched before `didChange` was sent.
    ///
    /// Returns `true` if diagnostics changed and the UI should redraw.
    pub async fn sync_lsp_and_refresh_diagnostics(&mut self) -> bool {
        // Step 1: Push pending content to the server.
        self.send_lsp_changes_if_modified().await;
        self.send_lsp_save_if_needed().await;

        // Step 2: Now that the server is up-to-date, process diagnostics.
        let Some(lsp_manager) = self.lsp.state.lsp_manager.clone() else {
            return false;
        };

        self.refresh_current_lsp_sync_versions().await;

        // Apply publications held during the post-edit settle window. Deferring
        // rather than discarding ensures an empty update can clear old errors.
        lsp_manager.apply_deferred_diagnostics().await;

        // Transfer cross-thread signal to the TrackedSlot's generation counter.
        // diagnostics_changed() is consumed-on-read (AtomicBool swap), but
        // invalidate() is a monotonic counter that can never be lost.
        if lsp_manager.diagnostics_changed() {
            self.lsp.slots.diagnostics.invalidate();
        }

        // Poll completed results, then fire a new request if stale.
        let changed = self.poll_pending_diagnostic_refresh_response();
        let document_sync_dirty = self
            .buffer()
            .file_path()
            .and_then(|path| self.lsp.state.document_sync.get(path))
            .is_some_and(|state| state.is_modified());
        if self.lsp.slots.diagnostics.needs_refresh() && !document_sync_dirty {
            self.spawn_diagnostic_cache_refresh();
        }
        changed
    }

    /// Invalidate cached diagnostics and request a fresh pull from the LSP server.
    pub fn clear_and_refresh_diagnostics(&mut self) {
        self.lsp.state.diagnostics_file_path = None;
        self.lsp.slots.diagnostics.invalidate();
    }

    /// Handle LSP/diagnostics state when current buffer path changes (e.g. :w newfile).
    pub fn handle_file_path_transition_after_save(
        &mut self,
        old_path: Option<String>,
        new_path: Option<String>,
    ) {
        if old_path == new_path {
            return;
        }

        if let Some(old) = old_path {
            self.lsp.state.document_sync.remove(&old);
            self.lsp.state.pending_did_close_file = Some(old);
        }
        if let Some(newp) = &new_path {
            // The target path may already be open on the server (e.g.
            // `:w other.rs` onto a file that was previously edited): the
            // manager's didOpen claim survives, so reconcile would seed the
            // fresh sync entry with "server has the current buffer" while
            // the server actually holds the target file's OLD text. Start
            // the new entry in force_full_resend so the first sync pushes
            // the full buffer instead of assuming it's in sync (OV-00334).
            let state = self
                .lsp
                .state
                .document_sync
                .entry(newp.clone())
                .or_default();
            *state = Default::default();
            state.buffer_modified = true;
            state.force_full_resend = true;
        }

        self.lsp.state.needs_lsp_init = true;
        self.clear_and_refresh_diagnostics();
    }

    /// Returns true if diagnostics need refreshing.
    /// Note: unlike the old consumed-on-read flag, this is non-destructive —
    /// calling it multiple times returns the same result until fire() is called.
    /// Tests that previously used this as a "consume and check" should use
    /// `lsp.slots.diagnostics.is_stale()` directly for clarity.
    pub fn take_diagnostics_refresh_request(&mut self) -> bool {
        self.lsp.slots.diagnostics.is_stale()
    }

    pub fn lsp_document_sync_exists(&self) -> bool {
        let Some(file_path) = self.buffer().file_path() else {
            return false;
        };
        self.lsp.state.document_sync.contains_key(file_path)
    }

    pub fn lsp_document_is_modified(&self) -> Option<bool> {
        let file_path = self.buffer().file_path()?;
        self.lsp
            .state
            .document_sync
            .get(file_path)
            .map(|s| s.is_modified())
    }

    /// Mark buffer as saved (for LSP didSave tracking)
    pub fn mark_buffer_saved(&mut self) {
        if let Some(state) = self.document_sync_state_mut() {
            state.mark_saved();
        }
    }

    /// Sends buffered text changes to LSP if modified.
    ///
    /// Forwards the latest buffer content to LspManager on every tick where the
    /// buffer is dirty.  Debouncing is handled entirely by LspManager's
    /// `ChangeDebouncer` (single-owner, 150 ms).  The editor side no longer
    /// adds its own 150 ms gate — that was causing a redundant double-debounce
    /// (OV-00165).
    pub async fn send_lsp_changes_if_modified(&mut self) {
        let Some(lsp) = self.lsp.state.lsp_manager.clone() else {
            return;
        };

        let Some(file_path) = self.buffer().file_path().map(|p| p.to_string()) else {
            return;
        };

        let uri = match uri_from_file_path(&file_path) {
            Some(u) => u,
            None => return,
        };

        let state_key = file_path.clone();
        let manager_version = lsp.get_document_version(&uri).await;
        let sent_version = lsp.get_last_sent_version(&uri).await;
        let needs_reconcile = manager_version > 0
            && self
                .lsp
                .state
                .document_sync
                .get(&state_key)
                .is_some_and(|state| {
                    (sent_version > 0 && state.last_flushed_content.is_none())
                        || state
                            .target_lsp_version
                            .is_some_and(|target_version| sent_version >= target_version)
                });

        let mut content: Option<Arc<str>> = None;
        if needs_reconcile {
            content = Some(Arc::from(self.buffer().rope().to_string()));
            self.reconcile_document_sync_with_manager(
                &state_key,
                content.as_deref(),
                manager_version,
                sent_version,
            );
        } else if manager_version > 0 {
            self.lsp.state.current_file_lsp_version = manager_version;
            self.lsp.state.current_file_lsp_sent_version = sent_version;
        }

        // Check if we need to send — only guard is didOpen + modified
        let should_send = self
            .lsp
            .state
            .document_sync
            .get(&state_key)
            .is_some_and(|state| state.did_open_sent && state.is_modified());

        if should_send {
            // Snapshot current content once for queue/no-op checks and potential send.
            let content: Arc<str> =
                content.unwrap_or_else(|| Arc::from(self.buffer().rope().to_string()));

            let force_full_resend = {
                let state = self
                    .lsp
                    .state
                    .document_sync
                    .entry(state_key.clone())
                    .or_default();
                // A forced resend bypasses both no-op guards: they assume the
                // sync-state snapshots reflect what the server has, which is
                // exactly what a reload after an external write broke.
                if !state.force_full_resend {
                    if state.target_lsp_version.is_none()
                        && state.flushed_content() == Some(&*content)
                    {
                        state.buffer_modified = false;
                        state.last_queued_content = None;
                        return;
                    }

                    if state.queued_content() == Some(&*content) {
                        return;
                    }
                }
                state.force_full_resend
            };

            // Get language_id from file extension
            let language_id = match self.language_id_for_path(&file_path) {
                Some(id) => id,
                None => return,
            };

            // Get old content for incremental sync. Under a forced resend
            // there is no trustworthy baseline — send the full document.
            let old_content = if force_full_resend {
                None
            } else {
                self.lsp
                    .state
                    .document_sync
                    .get(&state_key)
                    .and_then(|state| state.last_flushed_content.clone())
            };

            // Send the didChange notification to all servers for this language
            if lsp
                .did_change_broadcast(uri.clone(), &language_id, content.clone(), old_content)
                .await
                .is_err()
            {
                return;
            }

            // Track the queued LSP document version (bumped immediately in did_change).
            let queued_version = lsp.get_document_version(&uri).await;
            self.lsp.state.current_file_lsp_version = queued_version;
            self.lsp.state.current_file_lsp_sent_version = lsp.get_last_sent_version(&uri).await;

            // Record the newest queued snapshot; manager reconciliation will only
            // promote it to flushed once last_sent catches up.
            let state = self.lsp.state.document_sync.entry(state_key).or_default();
            state.mark_change_queued(content, queued_version);
            state.force_full_resend = false;

            // Note: slot invalidation for inlay_hints and diagnostics happens
            // upstream in the canonical `mark_buffer_modified` hook (and in
            // `mark_document_flushed` for server-state-changed boundaries).
            // The explicit invalidate that used to live here was pure
            // duplication — every path that produced modified content
            // already bumped the generation counter.
        }
    }

    /// Sends didSave notification to LSP if needed
    pub async fn send_lsp_save_if_needed(&mut self) {
        let Some(file_path) = self.buffer().file_path().map(|p| p.to_string()) else {
            return;
        };

        let uri = match uri_from_file_path(&file_path) {
            Some(u) => u,
            None => return,
        };

        let state_key = file_path.to_string();
        let mut should_send = false;

        // Check if we should send save notification
        if let Some(state) = self.lsp.state.document_sync.get(&state_key) {
            if state.should_send_save() {
                should_send = true;
            }
        }

        if should_send {
            // Ensure didOpen/didChange state for this URI before sending didSave.
            self.ensure_lsp_document_synced().await;

            // Get buffer content BEFORE we update the state
            let content = self.buffer().rope().to_string();

            // Get language_id from file extension
            let language_id = match self.language_id_for_path(&file_path) {
                Some(id) => id,
                None => return,
            };

            let Some(ref lsp) = self.lsp.state.lsp_manager else {
                return;
            };

            // Send the didSave notification to all servers for this language
            match lsp
                .did_save_broadcast(uri, &language_id, Some(content))
                .await
            {
                Ok(()) => {
                    // Mark as sent AFTER successful send; this hook also
                    // invalidates inlay_hints + diagnostics because servers
                    // may re-analyze on save (e.g. rust-analyzer's cargo
                    // check pass) and emit fresh data for unchanged buffers.
                    self.on_lsp_save_sent(&state_key);
                }
                Err(e) => {
                    crate::lsp_warn!("LSP", "didSave failed for {}: {}", file_path, e);
                }
            }
        }
    }

    /// Ensures the LSP server has the latest document content before making a request
    ///
    /// CRITICAL FIX: When we make a hover/goto request immediately after typing,
    /// the debounced didChange (150ms) might not have been sent yet. This causes
    /// LSP to return stale results. We flush pending changes here to ensure LSP
    /// has the latest content.
    async fn ensure_lsp_document_synced(&mut self) -> bool {
        let Some(lsp) = self.lsp.state.lsp_manager.clone() else {
            return false;
        };

        let Some(file_path) = self.buffer().file_path().map(|p| p.to_string()) else {
            return false;
        };

        let uri = match uri_from_file_path(&file_path) {
            Some(u) => u,
            None => return false,
        };

        let state_key = file_path.clone();

        // Get language_id from file extension
        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => return false,
        };

        // Get buffer content
        let content_str = self.buffer().rope().to_string();
        let manager_version = lsp.get_document_version(&uri).await;
        let sent_version = lsp.get_last_sent_version(&uri).await;
        self.reconcile_document_sync_with_manager(
            &state_key,
            Some(&content_str),
            manager_version,
            sent_version,
        );

        let content: Arc<str> = Arc::from(content_str);
        let plan = self.document_sync_request_plan(&state_key, &content);
        match plan.action {
            DocumentSyncRequestAction::Noop => false,
            DocumentSyncRequestAction::DidOpen => {
                match lsp
                    .did_open_broadcast(uri.clone(), &language_id, 1, content.to_string())
                    .await
                {
                    Ok(_) => {
                        let flushed_version = lsp.get_last_sent_version(&uri).await;
                        self.lsp.state.current_file_lsp_version =
                            lsp.get_document_version(&uri).await;
                        self.lsp.state.current_file_lsp_sent_version = flushed_version;
                        self.mark_document_flushed(&state_key, content, flushed_version);
                        // Server just got a fresh view of the document —
                        // invalidate derived slots so hints/diagnostics
                        // get re-requested against the newly-open doc.
                        self.lsp.slots.inlay_hints.invalidate();
                        self.lsp.slots.diagnostics.invalidate();
                    }
                    Err(e) => {
                        crate::lsp_warn!(
                            "LSP",
                            "didOpen failed for {}: {} (will retry)",
                            state_key,
                            e
                        );
                    }
                }
                true
            }
            DocumentSyncRequestAction::FlushQueued => {
                // Use actual flushed content to avoid desync with LSP server.
                let flushed = lsp
                    .flush_pending_changes_broadcast(&uri, &language_id)
                    .await
                    .ok()
                    .flatten();
                self.lsp.state.current_file_lsp_version = lsp.get_document_version(&uri).await;
                self.lsp.state.current_file_lsp_sent_version =
                    lsp.get_last_sent_version(&uri).await;
                if let Some((text, ver)) = flushed {
                    self.mark_document_flushed(&state_key, Arc::from(text), ver);
                }
                // On None/Err nothing new reached the server — recording the
                // CURRENT buffer as flushed here used to claim content the
                // server never received, silently swallowing the next resend
                // (OV-00326). Manager reconciliation promotes the queued
                // snapshot once last_sent actually catches up.
                true
            }
            DocumentSyncRequestAction::QueueChangeAndFlush => {
                if lsp
                    .did_change_broadcast(
                        uri.clone(),
                        &language_id,
                        content.clone(),
                        plan.old_content,
                    )
                    .await
                    .is_err()
                {
                    return true;
                }

                let queued_version = lsp.get_document_version(&uri).await;
                {
                    let state = self
                        .lsp
                        .state
                        .document_sync
                        .entry(state_key.clone())
                        .or_default();
                    state.mark_change_queued(content.clone(), queued_version);
                }

                // Use actual flushed content to avoid desync with LSP server.
                let flushed = lsp
                    .flush_pending_changes_broadcast(&uri, &language_id)
                    .await
                    .ok()
                    .flatten();
                self.lsp.state.current_file_lsp_version = lsp.get_document_version(&uri).await;
                self.lsp.state.current_file_lsp_sent_version =
                    lsp.get_last_sent_version(&uri).await;
                if let Some((text, ver)) = flushed {
                    self.mark_document_flushed(&state_key, Arc::from(text), ver);
                }
                // On None/Err: see FlushQueued above — never record content
                // the server did not receive as flushed (OV-00326).
                true
            }
        }
    }

    /// Sends didClose notification to LSP for the pending file
    pub async fn send_lsp_close_if_needed(&mut self) {
        let Some(file_path) = self.lsp.state.pending_did_close_file.take() else {
            return;
        };

        let Some(ref lsp) = self.lsp.state.lsp_manager else {
            return;
        };

        let uri = match uri_from_file_path(&file_path) {
            Some(u) => u,
            None => return,
        };

        // Get language_id from file extension
        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => return,
        };

        let file_path_string = file_path.to_string();
        let _ = lsp.did_close_broadcast(uri, &language_id).await;
        self.lsp.state.document_sync.remove(&file_path_string);
    }

    // -------------------------------------------------------------------------
    // LSP Action Processing (process pending actions from event loop)
    // -------------------------------------------------------------------------

    /// Process pending LSP actions
    /// Called from the event loop to handle LSP requests asynchronously
    /// Dispatches all pending LSP intents.
    ///
    /// Each intent is checked independently — multiple intents can fire in the
    /// same tick (unlike the old single-slot `pending_lsp_action` which lost
    /// actions when two were queued in the same frame). Each `_impl()` method
    /// fires into its own `Slot<T>` and returns immediately.
    pub async fn dispatch_pending_intents(&mut self) {
        if std::mem::take(&mut self.lsp.intents.goto_definition) {
            let _ = self.goto_definition_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.goto_definition_new_tab) {
            let _ = self.goto_definition_new_tab_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.goto_implementation) {
            let _ = self.goto_implementation_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.goto_implementation_new_tab) {
            let _ = self.goto_implementation_new_tab_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.goto_type) {
            let _ = self.goto_type_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.hover) {
            let _ = self.hover_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.completion) {
            let _ = self.completion_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.format_document) {
            let _ = self.format_document_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.code_actions) {
            let _ = self.code_actions_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.type_hierarchy) {
            let _ = self.type_hierarchy_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.call_hierarchy_incoming) {
            let _ = self.call_hierarchy_incoming_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.call_hierarchy_outgoing) {
            let _ = self.call_hierarchy_outgoing_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.find_references) {
            let _ = self.find_references_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.document_symbols) {
            let _ = self.document_symbols_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.workspace_symbols) {
            let _ = self.workspace_symbols_impl().await;
        }
        if std::mem::take(&mut self.lsp.intents.organize_imports) {
            let _ = self.organize_imports_impl().await;
        }
        if let Some(new_name) = self.lsp.intents.rename.take() {
            let _ = self.rename_impl(new_name).await;
        }
        if std::mem::take(&mut self.lsp.intents.semantic_tokens) {
            let _ = self.semantic_tokens_impl().await;
        }
    }

    // -------------------------------------------------------------------------
    // UTF-16 Conversion Helpers (LSP uses UTF-16 code units for positions)
    // -------------------------------------------------------------------------

    /// Converts a **grapheme** column (from `cursor.col()`) to UTF-16 code units
    /// for LSP `Position.character`.
    ///
    /// The conversion chain is: grapheme index → char index → UTF-16 code units.
    /// Skipping the grapheme→char step was OV-00226 — combining characters (é = e + ◌́)
    /// caused every outbound LSP position to be wrong.
    pub(crate) fn col_to_utf16(&self, line: usize, grapheme_col: usize) -> u32 {
        let rope = self.buffer().rope();
        if line >= rope.len_lines() {
            return 0;
        }

        let line_text = rope.line(line);

        // rope.line() includes the trailing line terminator — strip it for LSP.
        // We strip `\r` too: the rope is LF-only by convention, but a stray `\r`
        // can slip past the input seams (mixed line endings), and the server
        // never saw it (we strip on send), so it must not count toward the
        // UTF-16 offset. (OV-00268)
        let line_str: String = line_text
            .chars()
            .take_while(|&c| c != '\n' && c != '\r')
            .collect();

        // Step 1: grapheme index → char index
        let char_col = crate::unicode::grapheme_to_char_col(
            &line_str,
            crate::unicode::GraphemeCol(grapheme_col),
        );
        let safe_col = char_col.0.min(line_str.chars().count());

        // Step 2: char index → UTF-16 code units
        line_str
            .chars()
            .take(safe_col)
            .map(|c| c.len_utf16() as u32)
            .sum()
    }

    /// Converts UTF-16 code units (from LSP) to a **char** column index.
    ///
    /// Returns a char index suitable for rope operations (`insert_text_at`,
    /// `delete_range`). For cursor positioning (which needs grapheme indices),
    /// use [`utf16_to_grapheme_col`] instead.
    pub(crate) fn utf16_to_col(&self, line: usize, utf16_col: u32) -> crate::unicode::CharCol {
        let rope = self.buffer().rope();
        if line >= rope.len_lines() {
            return crate::unicode::CharCol::ZERO;
        }

        let line_text = rope.line(line);
        let mut utf16_offset = 0u32;
        let mut char_position = 0usize;

        for ch in line_text.chars() {
            if utf16_offset >= utf16_col {
                break;
            }
            // Stop at the line terminator: a server-supplied offset past the
            // end of the line content must not advance into `\n` / `\r`
            // (mirrors the `\r`-strip in `col_to_utf16`). (OV-00268)
            if ch == '\n' || ch == '\r' {
                break;
            }
            utf16_offset += ch.len_utf16() as u32;
            char_position += 1;
        }

        crate::unicode::CharCol(char_position)
    }

    /// Converts UTF-16 code units (from LSP) to a **grapheme** column index.
    ///
    /// Returns a grapheme index suitable for `cursor.set_position()`. This
    /// is the correct conversion for goto-definition targets, reference
    /// locations, and any LSP position that becomes a cursor position.
    pub(crate) fn utf16_to_grapheme_col(&self, line: usize, utf16_col: u32) -> usize {
        let char_col = self.utf16_to_col(line, utf16_col);

        let rope = self.buffer().rope();
        if line >= rope.len_lines() {
            return 0;
        }
        let line_text = rope.line(line);
        let line_str: String = line_text
            .chars()
            .take_while(|&c| c != '\n' && c != '\r')
            .collect();

        crate::unicode::char_to_grapheme_col(&line_str, char_col).0
    }

    /// Prepare common context for an LSP request.
    /// Handles: LSP manager check, file path resolution, URI creation,
    /// cursor position (UTF-16), language detection, and document sync flush.
    pub(in crate::editor) async fn prepare_lsp_request(
        &mut self,
        feature_name: &str,
    ) -> Result<LspRequestContext> {
        let lsp = self
            .lsp
            .state
            .lsp_manager
            .clone()
            .ok_or_else(|| anyhow!("LSP not available"))?;

        let file_path = self
            .buffer()
            .file_path()
            .ok_or_else(|| anyhow!("Save file first to use {}", feature_name))?
            .to_string();

        let abs_path = if std::path::Path::new(&file_path).is_absolute() {
            file_path.clone()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&file_path).to_string_lossy().to_string())
                .map_err(|_| anyhow!("Failed to resolve file path"))?
        };

        let uri = crate::lsp::uri_from_file_path(&abs_path)
            .ok_or_else(|| anyhow!("Invalid file path"))?;

        let cursor = self.buffer().cursor();
        let line = cursor.line() as u32;
        let character = self.col_to_utf16(cursor.line(), cursor.col().0);

        let language_id = self
            .language_id_for_path(&file_path)
            .ok_or_else(|| anyhow!("Language not supported for LSP"))?
            .to_string();

        // Flush pending document changes so LSP has the latest content
        self.ensure_lsp_document_synced().await;

        // Resolve the server group responsible for this document (primary + companions).
        let server_ids = lsp.servers_for_document(&language_id, std::path::Path::new(&abs_path));
        if server_ids.is_empty() {
            return Err(anyhow!(
                "No LSP server available for {} in {}",
                feature_name,
                abs_path
            ));
        }

        Ok(LspRequestContext {
            lsp,
            uri,
            file_path,
            line,
            character,
            language_id,
            server_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::lsp_slot::{CompletionResult, InlayHintResult};
    use crate::editor::lsp_state::InlayHintRequestKey;
    use crate::lsp::uri_from_file_path;
    use lsp_types::{CompletionItem, InlayHint, InlayHintLabel, Location, Position, Range};
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_handle_location_result_new_tab_updates_current_file_register() {
        let test_dir = tempfile::tempdir().expect("tempdir");

        let source = test_dir.path().join("source.rs");
        let target = test_dir.path().join("target.rs");

        std::fs::write(&source, "source\n").unwrap();
        std::fs::write(&target, "target\n").unwrap();

        let source_path = std::fs::canonicalize(&source)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let target_path = std::fs::canonicalize(&target)
            .unwrap()
            .to_string_lossy()
            .to_string();

        let mut editor = Editor::with_content("source\n");
        editor.set_file_path(source_path);

        let uri = uri_from_file_path(&target).unwrap();
        let location = Location::new(uri, Range::new(Position::new(0, 0), Position::new(0, 0)));

        let handled =
            editor.handle_location_result(Ok(Some(location)), "Definition", "LSP-DEFINITION", true);
        assert!(handled);
        assert_eq!(editor.registers().get(Some('%')), target_path);
    }

    #[test]
    fn document_sync_request_plan_flushes_already_queued_content() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());

        let state = editor
            .lsp
            .state
            .document_sync
            .entry(file_path.clone())
            .or_default();
        state.did_open_sent = true;
        state.buffer_modified = true;
        state.last_flushed_content = Some(Arc::from("class Test {\n"));
        state.last_queued_content = Some(Arc::from("class Test {}\n"));
        state.target_lsp_version = Some(4);

        let plan = editor.document_sync_request_plan(&file_path, "class Test {}\n");
        assert_eq!(plan.action, DocumentSyncRequestAction::FlushQueued);
        assert!(plan.old_content.is_none());
    }

    #[test]
    fn reconcile_document_sync_with_manager_promotes_flushed_queue() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());

        let state = editor
            .lsp
            .state
            .document_sync
            .entry(file_path.clone())
            .or_default();
        state.did_open_sent = true;
        state.mark_change_queued(Arc::from("class Test {}\n"), 4);

        editor.reconcile_document_sync_with_manager(&file_path, Some("class Test {}\n"), 4, 4);

        let state = editor
            .lsp
            .state
            .document_sync
            .get(&file_path)
            .expect("document sync state");
        assert!(!state.buffer_modified);
        assert!(state.target_lsp_version.is_none());
        assert!(state.last_queued_content.is_none());
        assert_eq!(
            state.last_flushed_content.as_deref(),
            Some("class Test {}\n")
        );
    }

    #[test]
    fn reconcile_document_sync_with_manager_keeps_dirty_flag_for_newer_buffer_content() {
        let mut editor = Editor::with_content("class Test { int value; }\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());

        let state = editor
            .lsp
            .state
            .document_sync
            .entry(file_path.clone())
            .or_default();
        state.did_open_sent = true;
        state.mark_change_queued(Arc::from("class Test {}\n"), 4);

        editor.reconcile_document_sync_with_manager(
            &file_path,
            Some("class Test { int value; }\n"),
            4,
            4,
        );

        let state = editor
            .lsp
            .state
            .document_sync
            .get(&file_path)
            .expect("document sync state");
        assert!(state.buffer_modified);
        assert!(state.target_lsp_version.is_none());
        assert_eq!(
            state.last_flushed_content.as_deref(),
            Some("class Test {}\n")
        );
    }

    /// Helper: fire a pre-built `InlayHintResult` into the inlay hints slot.
    fn fire_inlay_hint_result(editor: &mut Editor, result: InlayHintResult) {
        let (tx, rx) = oneshot::channel::<anyhow::Result<InlayHintResult>>();
        tx.send(Ok(result)).unwrap();
        let task = tokio::spawn(async {});
        editor.lsp.slots.inlay_hints.fire(task, rx);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_inlay_hint_response_applies_latest_result() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());
        editor.set_viewport_height(20);

        let request_key = InlayHintRequestKey {
            file_path: file_path.clone(),
            start_line: 0,
            end_line: 30,
            lsp_version: 4,
        };
        let hint = InlayHint {
            position: Position::new(0, 5),
            label: InlayHintLabel::String(": Test".to_string()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        };

        let bv = editor.buffer().version();
        fire_inlay_hint_result(
            &mut editor,
            InlayHintResult {
                request_key: request_key.clone(),
                buffer_version: bv,
                synced_content: Some("class Test {}\n".to_string()),
                synced_lsp_version: Some(4),
                hints: vec![hint],
            },
        );

        assert!(editor.poll_pending_inlay_hint_response());
        assert_eq!(editor.lsp.state.current_file_lsp_version, 4);
        assert_eq!(editor.lsp.state.current_file_lsp_sent_version, 4);
        assert_eq!(editor.lsp.state.inlay_hints.len(), 1);
        // TrackedSlot: after a successful poll, the slot is no longer stale
        // (the result was applied for the generation that was current at fire time).
        assert!(!editor.lsp.slots.inlay_hints.is_stale());

        let sync_state = editor
            .lsp
            .state
            .document_sync
            .get(&file_path)
            .expect("document sync state");
        assert!(sync_state.did_open_sent);
        assert_eq!(
            sync_state.last_flushed_content.as_deref(),
            Some("class Test {}\n")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_inlay_hint_response_drops_stale_buffer_result() {
        // OV-00258: when the buffer version has advanced since the LSP
        // request was spawned, the hint positions index into the wrong
        // rope. Render-skip is correct; the next tick re-requests against
        // the current version.
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());
        editor.set_viewport_height(20);

        let request_key = InlayHintRequestKey {
            file_path: file_path.clone(),
            start_line: 0,
            end_line: 30,
            lsp_version: 4,
        };

        // Reply was prepared against a buffer version one ahead of the
        // current version — i.e., the user typed something between
        // request-spawn and reply-arrival.
        let stale_buffer_version = editor.buffer().version() + 1;
        let stale_hint = InlayHint {
            position: Position::new(0, 5),
            label: InlayHintLabel::String(": Stale".to_string()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        };
        fire_inlay_hint_result(
            &mut editor,
            InlayHintResult {
                request_key: request_key.clone(),
                buffer_version: stale_buffer_version,
                synced_content: None,
                synced_lsp_version: None,
                hints: vec![stale_hint],
            },
        );

        // Drop the response — return value is `false` (no UI mutation).
        assert!(!editor.poll_pending_inlay_hint_response());

        // Cached hints and InlayHint decorations must remain empty —
        // we did not commit a mis-aligned render.
        assert!(
            editor.lsp.state.inlay_hints.is_empty(),
            "stale hints must not enter `lsp.state.inlay_hints`"
        );
        assert!(
            !editor
                .decorations
                .iter_all()
                .any(|(_, d)| d.source == crate::editor::decoration::DecorationSource::InlayHint),
            "no InlayHint decoration may be placed when the reply is stale"
        );

        // Slot is invalidated so the next event-loop tick re-requests.
        assert!(editor.lsp.slots.inlay_hints.is_stale());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_inlay_hint_response_drops_result_behind_current_sent_version() {
        let mut editor = Editor::with_content("class Test {}\n");
        let file_path = "/tmp/Test.java".to_string();
        editor.set_file_path(file_path.clone());
        editor.set_viewport_height(20);
        editor.lsp.state.current_file_lsp_sent_version = 5;

        let request_key = InlayHintRequestKey {
            file_path,
            start_line: 0,
            end_line: 30,
            lsp_version: 4,
        };

        let bv = editor.buffer().version();
        fire_inlay_hint_result(
            &mut editor,
            InlayHintResult {
                request_key: request_key.clone(),
                buffer_version: bv,
                synced_content: None,
                synced_lsp_version: None,
                hints: Vec::new(),
            },
        );

        assert!(!editor.poll_pending_inlay_hint_response());
        assert!(editor.lsp.state.inlay_hints.is_empty());
        // Slot is stale (invalidated) because the result was dropped.
        assert!(editor.lsp.slots.inlay_hints.is_stale());
    }

    /// Helper: fire a pre-built `CompletionResult` into the completion slot so
    /// `poll_pending_completion_response` can pick it up immediately.
    fn fire_completion_result(editor: &mut Editor, result: CompletionResult) {
        let (tx, rx) = oneshot::channel::<anyhow::Result<CompletionResult>>();
        tx.send(Ok(result)).unwrap();
        let task = tokio::spawn(async {});
        editor.lsp.slots.completion.fire(task, rx);
    }

    fn completion_item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            insert_text: Some(label.to_string()),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_completion_response_shows_menu_on_fresh_result() {
        let mut editor = Editor::with_content("let x = fo");
        editor.set_file_path("/tmp/a.rs".to_string());
        editor.set_mode(crate::mode::Mode::Insert);

        // Read the EFFECTIVE path back: set_file_path canonicalizes when the
        // file happens to exist on the host (macOS /tmp → /private/tmp), and
        // the poll validates result.file_path against it by string equality.
        // Hard-coding "/tmp/a.rs" made this test depend on whether a stray
        // /tmp/a.rs existed on the machine.
        let effective_path = editor.buffer().file_path().unwrap().to_string();

        let bv = editor.buffer().version();
        fire_completion_result(
            &mut editor,
            CompletionResult {
                items: vec![completion_item("foo")],
                file_path: effective_path,
                buffer_version: bv,
                synced_content: None,
                synced_lsp_version: None,
            },
        );

        assert!(editor.poll_pending_completion_response());
        assert!(editor.completion_menu().is_visible());
        assert_eq!(editor.completion_menu().items().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_completion_response_drops_result_from_old_file() {
        let mut editor = Editor::with_content("let x = fo");
        editor.set_file_path("/tmp/b.rs".to_string());
        editor.set_mode(crate::mode::Mode::Insert);

        let bv = editor.buffer().version();
        // Response was fired for /tmp/a.rs but the user has since switched
        // to /tmp/b.rs. Without validation this would apply to the wrong file.
        fire_completion_result(
            &mut editor,
            CompletionResult {
                items: vec![completion_item("foo")],
                file_path: "/tmp/a.rs".to_string(),
                buffer_version: bv,
                synced_content: None,
                synced_lsp_version: None,
            },
        );

        assert!(!editor.poll_pending_completion_response());
        assert!(!editor.completion_menu().is_visible());
        assert!(editor.completion_menu().items().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_pending_completion_response_drops_result_from_stale_buffer_version() {
        let mut editor = Editor::with_content("let x = fo");
        editor.set_file_path("/tmp/a.rs".to_string());
        editor.set_mode(crate::mode::Mode::Insert);

        // Simulate: request fired at version N, user kept typing (bumping
        // the buffer version), response arrived carrying the old N. Without
        // validation the stale items would populate the menu.
        let effective_path = editor.buffer().file_path().unwrap().to_string();
        let stale_version = editor.buffer().version();
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol(10), "o");
        assert!(editor.buffer().version() > stale_version);

        fire_completion_result(
            &mut editor,
            CompletionResult {
                items: vec![completion_item("foo")],
                file_path: effective_path,
                buffer_version: stale_version,
                synced_content: None,
                synced_lsp_version: None,
            },
        );

        assert!(!editor.poll_pending_completion_response());
        assert!(!editor.completion_menu().is_visible());
        assert!(editor.completion_menu().items().is_empty());
    }

    fn fire_format_result(editor: &mut Editor, result: crate::editor::lsp_slot::FormatResult) {
        let (tx, rx) = oneshot::channel::<anyhow::Result<crate::editor::lsp_slot::FormatResult>>();
        tx.send(Ok(result)).unwrap();
        let task = tokio::spawn(async {});
        editor.lsp.slots.format.fire(task, rx);
    }

    fn whole_buffer_replace_edit(new_text: &str, end_line: u32) -> lsp_types::TextEdit {
        lsp_types::TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(end_line, 0)),
            new_text: new_text.to_string(),
        }
    }

    /// OV-00327: a format response computed against buffer version N must
    /// not be applied after the user kept editing (version N+k) — the edits
    /// would splice at stale offsets, reverting or garbling what was typed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stale_format_result_is_discarded() {
        let mut editor = Editor::with_content("fn main( ){}\n");
        editor.set_file_path("/tmp/fmt.rs".to_string());

        let stale_version = editor.buffer().version();
        let file_path = editor.buffer().file_path().unwrap().to_string();

        // User keeps typing while the format request is in flight.
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol(0), "// note\n");
        let typed_content = editor.buffer().rope().to_string();

        fire_format_result(
            &mut editor,
            crate::editor::lsp_slot::FormatResult {
                edits: vec![whole_buffer_replace_edit("fn main() {}\n", 1)],
                file_path,
                buffer_version: stale_version,
            },
        );

        editor.poll_action_slots();
        assert_eq!(
            editor.buffer().rope().to_string(),
            typed_content,
            "stale format edits must not be applied over newer typing"
        );
    }

    /// Companion: a format response for the current buffer version applies.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn current_format_result_applies() {
        let mut editor = Editor::with_content("fn main( ){}\n");
        editor.set_file_path("/tmp/fmt.rs".to_string());

        let file_path = editor.buffer().file_path().unwrap().to_string();
        let buffer_version = editor.buffer().version();
        fire_format_result(
            &mut editor,
            crate::editor::lsp_slot::FormatResult {
                edits: vec![whole_buffer_replace_edit("fn main() {}\n", 1)],
                file_path,
                buffer_version,
            },
        );

        editor.poll_action_slots();
        assert_eq!(editor.buffer().rope().to_string(), "fn main() {}\n");
    }

    /// OV-00327: accepting a completion item whose textEdit range targets an
    /// older buffer version must fall back to trigger-prefix replacement
    /// instead of splicing the stale range into the current text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stale_completion_accept_falls_back_to_prefix_replacement() {
        let mut editor = Editor::with_content("self.fo");
        editor.set_file_path("/tmp/a.rs".to_string());
        editor.set_mode(crate::mode::Mode::Insert);
        editor
            .buffer_mut()
            .set_cursor_char_col(0, crate::unicode::CharCol(7));

        // Server responded when the buffer was "self.fo": textEdit replaces
        // cols [5,7) ("fo") with "foo_bar()".
        let item = CompletionItem {
            label: "foo_bar".to_string(),
            insert_text: Some("foo_bar()".to_string()),
            text_edit: Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                range: Range::new(Position::new(0, 5), Position::new(0, 7)),
                new_text: "foo_bar()".to_string(),
            })),
            ..Default::default()
        };
        let response_version = editor.buffer().version();
        editor
            .completion_menu_mut()
            .show(vec![item.clone()], 5, "fo".to_string());
        editor
            .completion_menu_mut()
            .set_items_buffer_version(response_version);

        // User types one more char before accepting: buffer is "self.foo",
        // cursor at col 8. The stale range [5,7) no longer covers the
        // typed prefix — applying it verbatim used to produce
        // "self.foo_bar()o" (orphaned trailing char).
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol(7), "o");
        editor
            .buffer_mut()
            .set_cursor_char_col(0, crate::unicode::CharCol(8));
        assert!(editor.buffer().version() > response_version);

        editor.accept_completion();

        assert_eq!(
            editor.buffer().rope().to_string(),
            "self.foo_bar()\n",
            "stale textEdit range must not leave orphaned typed characters"
        );
    }
}
