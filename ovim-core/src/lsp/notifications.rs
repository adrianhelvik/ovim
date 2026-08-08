//! LSP notification handling for LspManager
//!
//! This module contains all notification-related methods including:
//! - did_open, did_change, did_save, did_close
//! - Debouncing and flushing mechanisms
//! - Processing incoming notifications from servers

use super::{
    protocol, utils::compute_simple_diff, ChangeDebouncer, JsonRpcMessage, LspManager,
    LspNotification, CHANGE_DEBOUNCE_MS, MAX_DOCUMENT_SIZE,
};
use anyhow::{anyhow, Result};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, PublishDiagnosticsParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, Uri, VersionedTextDocumentIdentifier,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

fn workspace_configuration_values(
    language: &str,
    params: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    let items = params
        .and_then(|params| params.get("items"))
        .and_then(serde_json::Value::as_array);
    let Some(items) = items else {
        return Vec::new();
    };
    if language != "lua" {
        return vec![serde_json::Value::Null; items.len()];
    }

    let settings = super::server::workspace_settings_for_language(language)
        .expect("Lua workspace settings are defined");
    items
        .iter()
        .map(|item| {
            let Some(section) = item.get("section").and_then(serde_json::Value::as_str) else {
                return settings.clone();
            };
            section
                .split('.')
                .try_fold(&settings, |value, key| value.get(key))
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        })
        .collect()
}

impl LspManager {
    pub async fn did_open(
        &self,
        uri: Uri,
        language_id: &str,
        version: i32,
        text: String,
    ) -> Result<()> {
        lsp_debug!(
            "LSP-NOTIFY",
            "textDocument/didOpen | URI: {} | Language: {} | Version: {} | Size: {} bytes",
            uri.as_str(),
            language_id,
            version,
            text.len()
        );

        // Check document size to prevent OOM
        if text.len() > MAX_DOCUMENT_SIZE {
            return Err(anyhow!(
                "Document '{}' too large: {} bytes (max {} bytes / {:.1} MB)",
                uri.as_str(),
                text.len(),
                MAX_DOCUMENT_SIZE,
                MAX_DOCUMENT_SIZE as f64 / (1024.0 * 1024.0)
            ));
        }

        // Atomically claim the didOpen: insert into document_versions under the
        // lock so concurrent callers see the URI as already-claimed and return
        // early. Without this guard, two concurrent callers could both pass a
        // contains_key check, drop the lock, and each send a didOpen — a
        // protocol violation. (OV-00210)
        {
            let mut versions = self.document_versions.lock().await;
            if versions.contains_key(&uri) {
                lsp_debug!(
                    "LSP-NOTIFY",
                    "textDocument/didOpen: skipping duplicate open for {}",
                    uri.as_str()
                );
                return Ok(());
            }
            versions.insert(uri.clone(), version);
        }

        let server = match self.servers.get(language_id) {
            Some(s) => s,
            None => {
                // Roll back the claim so a future open with a registered server can succeed.
                self.document_versions.lock().await.remove(&uri);
                return Err(anyhow::anyhow!("No server for language: {}", language_id));
            }
        };

        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version,
                text,
            },
        };

        if let Err(e) = server
            .notify("textDocument/didOpen", serde_json::to_value(params)?)
            .await
        {
            // Roll back so the next attempt isn't silently masked by the claim.
            self.document_versions.lock().await.remove(&uri);
            return Err(e);
        }

        lsp_debug!("LSP-NOTIFY", "textDocument/didOpen sent successfully");

        let mut sent = self.last_sent_versions.lock().await;
        sent.insert(uri, version);

        Ok(())
    }

    /// Sends textDocument/didChange notification to a specific server with an
    /// explicit version (pre-assigned by `did_change()`).
    /// Supports both full and incremental sync.
    ///
    /// Returns `Ok(true)` if the notification was actually sent, or `Ok(false)`
    /// if the old and new text were identical (no diff to send). Callers should
    /// still update `last_sent_versions` on `Ok(false)` because the server's
    /// content already matches — only the version number was bumped locally.
    async fn send_did_change_with_version(
        &self,
        uri: Uri,
        server_id: &str,
        text: String,
        old_text: Option<String>,
        version: i32,
    ) -> Result<bool> {
        // Get server reference
        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("No server for language: {}", server_id))?;

        // Check if server supports incremental sync and we have old content
        let supports_incremental = server.supports_incremental_sync().await;

        let full_doc_size = text.len();
        let content_changes = if supports_incremental && old_text.is_some() {
            if let Some(old) = old_text {
                if let Some((range, new_text)) = compute_simple_diff(&old, &text) {
                    vec![TextDocumentContentChangeEvent {
                        range: Some(range),
                        range_length: None,
                        text: new_text,
                    }]
                } else {
                    // No changes detected — content is identical to what server has.
                    // Return Ok(false) so callers sync last_sent_versions without
                    // sending a redundant notification.
                    return Ok(false);
                }
            } else {
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }]
            }
        } else {
            if std::env::var("OVIM_LSP_DEBUG").is_ok() {
                let reason = if !supports_incremental {
                    "server doesn't support incremental"
                } else {
                    "no old_text provided"
                };
                crate::lsp_debug!(
                    "LSP-SYNC",
                    "Full sync ({}): {} bytes | File: {}",
                    reason,
                    full_doc_size,
                    uri.path()
                );
            }
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text,
            }]
        };

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes,
        };

        crate::metrics::LSP_DIDCHANGE_TOTAL.inc();
        server
            .notify("textDocument/didChange", serde_json::to_value(params)?)
            .await?;

        Ok(true)
    }

    /// Flushes pending changes for a document (sends immediately).
    /// Uses the pre-assigned version from `did_change()`.
    pub async fn flush_pending_changes(&self, uri: &Uri) -> Result<()> {
        // Remove debouncer and get pending change
        if let Some((_, debouncer_arc)) = self.change_debouncers.remove(uri) {
            let mut debouncer = debouncer_arc.lock().await;
            debouncer.cancel_timer(); // Cancel timer

            // Send the pending change using the pre-assigned version
            let language_id = debouncer.language_id.clone();
            let text = debouncer.pending_text.clone();
            let old_text = debouncer.old_text.clone();
            let uri = debouncer.uri.clone();
            let version = debouncer.pending_version;
            drop(debouncer); // Release lock before async call

            match tokio::time::timeout(
                Duration::from_secs(5),
                self.send_did_change_with_version(
                    uri.clone(),
                    &language_id,
                    text.to_string(),
                    old_text.map(|s| s.to_string()),
                    version,
                ),
            )
            .await
            {
                Ok(Ok(actually_sent)) => {
                    // Sync last_sent_versions whether or not the notification
                    // was actually sent — if no-diff, the server's content
                    // already matches so the version should be considered
                    // synced (OV-00211).
                    let mut sent = self.last_sent_versions.lock().await;
                    sent.insert(uri.clone(), version);
                    drop(sent);

                    if actually_sent {
                        // Re-stamp last_local_edit to the flush instant so that
                        // the unversioned-diagnostics settle timer measures time
                        // since the server *received* new content, not since the
                        // edit was queued locally.  Without this, the settle
                        // (150ms) expires at the same time the debounce (150ms)
                        // fires, allowing stale diagnostics through.
                        self.last_local_edit
                            .lock()
                            .await
                            .insert(uri.clone(), std::time::Instant::now());
                    }
                }
                Ok(Err(e)) => {
                    lsp_error!(
                        "Manager",
                        "Failed to flush changes for {}: {}",
                        uri.as_str(),
                        e
                    );
                }
                Err(_) => {
                    lsp_error!(
                        "Manager",
                        "Timeout flushing changes for {} (5s)",
                        uri.as_str()
                    );
                }
            }
        }
        Ok(())
    }

    /// Sends textDocument/didChange notification with debouncing.
    /// Coalesces rapid changes to reduce LSP traffic by ~1000x.
    ///
    /// **Version is bumped immediately** (not on flush) so that stale
    /// `publishDiagnostics` arriving during the debounce window are correctly
    /// rejected by `set_diagnostics()`.  The assigned version is stored in the
    /// debouncer and used when the flush finally sends the content (OV-00163).
    pub async fn did_change(
        &self,
        uri: Uri,
        language_id: &str,
        text: Arc<str>,
        old_text: Option<Arc<str>>,
    ) -> Result<()> {
        // Bump the LSP document version immediately so that set_diagnostics()
        // can reject stale diagnostics even before the debounce timer fires.
        let assigned_version = {
            let mut versions = self.document_versions.lock().await;
            let v = versions.entry(uri.clone()).or_insert(0);
            *v += 1;
            *v
        };
        {
            let mut local_edits = self.last_local_edit.lock().await;
            local_edits.insert(uri.clone(), std::time::Instant::now());
        }

        // Get or create debouncer for this document atomically.
        let debouncer_arc = self
            .change_debouncers
            .entry(uri.clone())
            .or_insert_with(|| {
                Arc::new(Mutex::new(ChangeDebouncer::new(
                    uri.clone(),
                    language_id.to_string(),
                    assigned_version,
                )))
            })
            .clone();

        // Update pending change and restart timer
        let mut debouncer = debouncer_arc.lock().await;

        // Cancel existing timer if any
        debouncer.cancel_timer();

        // Update pending text, version, and old text
        debouncer.pending_text = text;
        debouncer.pending_version = assigned_version;
        // Always update old_text when the caller provides a baseline.
        // The caller passes last_flushed_content — what the server actually has.
        // Between flushes this value is stable (rapid typing passes the same
        // baseline repeatedly). After undo, it's still the correct baseline
        // because the server hasn't changed. The previous guard
        // (`if debouncer.old_text.is_none()`) kept a stale baseline from
        // pre-undo edits, causing wrong incremental diffs.
        if let Some(new_old) = old_text {
            debouncer.old_text = Some(new_old);
        }

        // Clone flush channel for timer closure
        let flush_tx = self.flush_tx.clone();
        let uri_clone = uri.clone();

        // Start new timer
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(CHANGE_DEBOUNCE_MS)).await;

            // Timer expired - request flush via channel
            if let Err(e) = flush_tx.send(uri_clone).await {
                lsp_error!("Debounce", "Error sending flush request: {}", e);
            }
        });

        debouncer.timer_handle = Some(handle);

        Ok(())
    }

    /// Sends textDocument/didSave notification
    pub async fn did_save(&self, uri: Uri, language_id: &str, text: Option<String>) -> Result<()> {
        // Flush any pending changes before saving
        self.flush_pending_changes(&uri).await?;

        let server = self
            .servers
            .get(language_id)
            .ok_or_else(|| anyhow::anyhow!("No server for language: {}", language_id))?;

        let params = DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text,
        };

        server
            .notify("textDocument/didSave", serde_json::to_value(params)?)
            .await?;

        Ok(())
    }

    /// Sends textDocument/didClose notification
    pub async fn did_close(&self, uri: Uri, language_id: &str) -> Result<()> {
        // Flush any pending changes before closing
        self.flush_pending_changes(&uri).await?;

        let server = self
            .servers
            .get(language_id)
            .ok_or_else(|| anyhow::anyhow!("No server for language: {}", language_id))?;

        let params = DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
        };

        server
            .notify("textDocument/didClose", serde_json::to_value(params)?)
            .await?;

        // Clean up internal state
        let mut versions = self.document_versions.lock().await;
        versions.remove(&uri);
        drop(versions);
        self.last_sent_versions.lock().await.remove(&uri);

        // Remove debouncer for this document
        self.change_debouncers.remove(&uri);
        self.last_local_edit.lock().await.remove(&uri);

        // Note: We keep diagnostics - they should remain visible even after file is closed

        Ok(())
    }

    // =========================================================================
    // Broadcast methods: send to ALL servers for a language (primary + companions)
    // =========================================================================

    /// Sends didOpen to the server group responsible for this document.
    pub async fn did_open_broadcast(
        &self,
        uri: Uri,
        language_id: &str,
        version: i32,
        text: String,
    ) -> Result<()> {
        // Check document size once
        if text.len() > MAX_DOCUMENT_SIZE {
            return Err(anyhow!(
                "Document too large: {} bytes (max {} bytes)",
                text.len(),
                MAX_DOCUMENT_SIZE
            ));
        }

        // Atomically claim the didOpen so concurrent callers don't broadcast
        // duplicate notifications to every server in the group. The claim
        // is taken before any other check so duplicates short-circuit
        // regardless of transient server-registry state. (OV-00210)
        {
            let mut versions = self.document_versions.lock().await;
            if versions.contains_key(&uri) {
                lsp_debug!(
                    "LSP-BROADCAST",
                    "textDocument/didOpen: skipping duplicate broadcast for {}",
                    uri.as_str()
                );
                return Ok(());
            }
            versions.insert(uri.clone(), version);
        }

        let server_ids = self.servers_for_document_uri(language_id, &uri);
        if server_ids.is_empty() {
            // Roll back the claim so a future broadcast (with servers
            // registered) is not silently masked.
            self.document_versions.lock().await.remove(&uri);
            return Err(anyhow!(
                "No servers for language '{}' matched document {}",
                language_id,
                uri.as_str()
            ));
        }

        for sid in &server_ids {
            if let Some(server) = self.servers.get(sid.as_str()) {
                let params = DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: language_id.to_string(),
                        version,
                        text: text.clone(),
                    },
                };
                if let Err(e) = server
                    .notify("textDocument/didOpen", serde_json::to_value(params)?)
                    .await
                {
                    lsp_warn!("LSP-BROADCAST", "didOpen failed for server {}: {}", sid, e);
                }
            }
        }

        // Initialize version tracking (once, shared) — version was claimed above.
        let mut sent = self.last_sent_versions.lock().await;
        sent.insert(uri, version);

        Ok(())
    }

    /// Sends didChange to all servers serving a language (debounced, shared timer)
    pub async fn did_change_broadcast(
        &self,
        uri: Uri,
        language_id: &str,
        text: Arc<str>,
        old_text: Option<Arc<str>>,
    ) -> Result<()> {
        // The debouncer is shared across all servers for a URI.
        // When the timer fires and flush happens, we send to all servers.
        // For now, reuse the existing debounce mechanism which sends to the primary.
        // The flush_pending_changes_broadcast will handle sending to all servers.
        self.did_change(uri, language_id, text, old_text).await
    }

    /// Flushes pending changes and broadcasts to all servers for the language.
    ///
    /// Uses the version that was pre-assigned in `did_change()` rather than
    /// re-incrementing.  This ensures the version in the didChange notification
    /// matches what `set_diagnostics()` already uses for staleness checks.
    /// Flushes pending changes and broadcasts to all servers for the language.
    ///
    /// Returns the `(content, version)` that was actually sent to the LSP
    /// server, or `None` if there was nothing to flush.  Callers that record
    /// `synced_content` (e.g. inlay-hint / completion tasks) **must** use the
    /// returned content — the debouncer may have been updated by another thread
    /// since the caller captured its snapshot.
    pub async fn flush_pending_changes_broadcast(
        &self,
        uri: &Uri,
        language_id: &str,
    ) -> Result<Option<(String, i32)>> {
        // First, remove the debouncer to get pending text
        if let Some((_, debouncer_arc)) = self.change_debouncers.remove(uri) {
            let mut debouncer = debouncer_arc.lock().await;
            debouncer.cancel_timer();

            let text = debouncer.pending_text.clone();
            let old_text = debouncer.old_text.clone();
            let uri = debouncer.uri.clone();
            // Use the version assigned in did_change() — already bumped in
            // document_versions, no need to re-increment.
            let version = debouncer.pending_version;
            drop(debouncer);

            // Convert Arc<str> → String once for the LSP protocol layer.
            let text_string = text.to_string();
            let old_text_string = old_text.map(|s| s.to_string());

            // Send to the server group responsible for this document with the same version
            let server_ids = self.servers_for_document_uri(language_id, &uri);
            let mut any_sent = false;
            let mut any_synced = false;
            for sid in &server_ids {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.send_did_change_with_version(
                        uri.clone(),
                        sid,
                        text_string.clone(),
                        old_text_string.clone(),
                        version,
                    ),
                )
                .await
                {
                    Ok(Ok(actually_sent)) => {
                        any_sent = any_sent || actually_sent;
                        // Even no-diff (Ok(false)) means content is in sync —
                        // version should be considered synced (OV-00211).
                        any_synced = true;
                    }
                    Ok(Err(e)) => {
                        lsp_warn!("LSP-BROADCAST", "Flush failed for server {}: {}", sid, e);
                    }
                    Err(_) => {
                        lsp_warn!(
                            "LSP-BROADCAST",
                            "Timeout flushing changes for server {} (5s)",
                            sid
                        );
                    }
                }
            }
            if any_synced {
                let mut sent = self.last_sent_versions.lock().await;
                sent.insert(uri.clone(), version);
                drop(sent);

                if any_sent {
                    // Re-stamp last_local_edit so unversioned-diagnostics settle
                    // timer measures from flush, not from queue time.
                    self.last_local_edit
                        .lock()
                        .await
                        .insert(uri.clone(), std::time::Instant::now());
                }

                return Ok(Some((text_string, version)));
            }
        }
        Ok(None)
    }

    /// Sends didSave to the server group responsible for this document.
    pub async fn did_save_broadcast(
        &self,
        uri: Uri,
        language_id: &str,
        text: Option<String>,
    ) -> Result<()> {
        // Flush pending changes to ALL servers (not just primary)
        let _ = self
            .flush_pending_changes_broadcast(&uri, language_id)
            .await?;

        let server_ids = self.servers_for_document_uri(language_id, &uri);
        for sid in &server_ids {
            if let Some(server) = self.servers.get(sid.as_str()) {
                let params = DidSaveTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    text: text.clone(),
                };
                if let Err(e) = server
                    .notify("textDocument/didSave", serde_json::to_value(params)?)
                    .await
                {
                    lsp_warn!("LSP-BROADCAST", "didSave failed for server {}: {}", sid, e);
                }
            }
        }
        Ok(())
    }

    /// Sends didClose to the server group responsible for this document.
    pub async fn did_close_broadcast(&self, uri: Uri, language_id: &str) -> Result<()> {
        let _ = self
            .flush_pending_changes_broadcast(&uri, language_id)
            .await?;

        let server_ids = self.servers_for_document_uri(language_id, &uri);
        for sid in &server_ids {
            if let Some(server) = self.servers.get(sid.as_str()) {
                let params = DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                };
                if let Err(e) = server
                    .notify("textDocument/didClose", serde_json::to_value(params)?)
                    .await
                {
                    lsp_warn!("LSP-BROADCAST", "didClose failed for server {}: {}", sid, e);
                }
            }
        }

        // Clean up shared state
        let mut versions = self.document_versions.lock().await;
        versions.remove(&uri);
        drop(versions);
        self.last_sent_versions.lock().await.remove(&uri);
        self.change_debouncers.remove(&uri);
        self.last_local_edit.lock().await.remove(&uri);
        self.deferred_diagnostics
            .lock()
            .await
            .retain(|(pending_uri, _), _| pending_uri != &uri);

        Ok(())
    }

    /// Handles incoming requests from language servers that expect a response
    async fn handle_server_request(&self, server_id: &str, request: JsonRpcMessage) {
        let method = request.method.as_deref().unwrap_or("");
        let request_id = request.id.clone();

        lsp_info!(
            "LSP-SERVER-REQUEST",
            "Received request from server: {} | ID: {:?}",
            method,
            request_id
        );

        match method {
            "workspace/applyEdit" => {
                // Parse the ApplyWorkspaceEditParams
                if let Some(params) = request.params {
                    match serde_json::from_value::<lsp_types::ApplyWorkspaceEditParams>(params) {
                        Ok(apply_params) => {
                            // Queue the workspace edit for the Editor to apply
                            // The Editor has access to buffers, we just queue the edits here
                            let edit = apply_params.edit;

                            lsp_info!(
                                "LSP-WORKSPACE",
                                "Queuing workspace edit with {} document changes",
                                edit.document_changes
                                    .as_ref()
                                    .map(|changes| match changes {
                                        lsp_types::DocumentChanges::Edits(edits) => edits.len(),
                                        lsp_types::DocumentChanges::Operations(ops) => ops.len(),
                                    })
                                    .unwrap_or_else(|| edit
                                        .changes
                                        .as_ref()
                                        .map(|c| c.len())
                                        .unwrap_or(0))
                            );

                            // Send to channel for Editor to process
                            match self.workspace_edit_tx.send(edit).await {
                                Ok(_) => {
                                    // Send success response to LSP server
                                    let response = lsp_types::ApplyWorkspaceEditResponse {
                                        applied: true,
                                        failure_reason: None,
                                        failed_change: None,
                                    };

                                    if let Some(id) = request_id {
                                        if let Some(server) = self.servers.get(server_id) {
                                            match serde_json::to_value(response) {
                                                Ok(value) => {
                                                    let response_msg =
                                                        JsonRpcMessage::response(id, value);
                                                    if let Err(e) =
                                                        server.send_response(response_msg).await
                                                    {
                                                        lsp_error!(
                                                            "LSP-SERVER-REQUEST",
                                                            "Failed to send workspace/applyEdit response: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    lsp_error!(
                                                        "LSP-SERVER-REQUEST",
                                                        "Failed to serialize workspace/applyEdit response: {}",
                                                        e
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Channel send failed
                                    lsp_error!(
                                        "LSP-SERVER-REQUEST",
                                        "Failed to queue workspace edit: {}",
                                        e
                                    );

                                    if let Some(id) = request_id {
                                        if let Some(server) = self.servers.get(server_id) {
                                            let error_response = protocol::ResponseError {
                                                code: -32603, // Internal error
                                                message: format!("Failed to queue edit: {}", e),
                                                data: None,
                                            };

                                            let response_msg =
                                                JsonRpcMessage::error_response(id, error_response);

                                            if let Err(e) = server.send_response(response_msg).await
                                            {
                                                lsp_error!(
                                                    "LSP-SERVER-REQUEST",
                                                    "Failed to send error response: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to parse workspace/applyEdit params: {}",
                                e
                            );

                            // Send error response for parse failure
                            if let Some(id) = request_id {
                                if let Some(server) = self.servers.get(server_id) {
                                    let error_response = protocol::ResponseError {
                                        code: -32700, // Parse error
                                        message: format!("Failed to parse parameters: {}", e),
                                        data: None,
                                    };

                                    let response_msg =
                                        JsonRpcMessage::error_response(id, error_response);

                                    if let Err(e) = server.send_response(response_msg).await {
                                        lsp_error!(
                                            "LSP-SERVER-REQUEST",
                                            "Failed to send error response: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "client/registerCapability" => {
                // Server wants to dynamically register capabilities
                if let Some(params) = request.params {
                    match serde_json::from_value::<lsp_types::RegistrationParams>(params) {
                        Ok(reg_params) => {
                            // Update cached capability flags for each registration
                            if let Some(server) = self.servers.get(server_id) {
                                for reg in &reg_params.registrations {
                                    lsp_info!(
                                        "LSP-SERVER-REQUEST",
                                        "Dynamic registration: {} (id: {})",
                                        reg.method,
                                        reg.id
                                    );
                                    server.set_capability_by_method(&reg.method, true);
                                }
                            }
                        }
                        Err(e) => {
                            lsp_warn!(
                                "LSP-SERVER-REQUEST",
                                "Failed to parse client/registerCapability params: {}",
                                e
                            );
                        }
                    }
                }

                // Always acknowledge success
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let response_msg = JsonRpcMessage::response(id, serde_json::Value::Null);
                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send client/registerCapability response: {}",
                                e
                            );
                        }
                    }
                }
            }
            "client/unregisterCapability" => {
                // Server wants to dynamically unregister capabilities
                if let Some(params) = request.params {
                    match serde_json::from_value::<lsp_types::UnregistrationParams>(params) {
                        Ok(unreg_params) => {
                            if let Some(server) = self.servers.get(server_id) {
                                for unreg in &unreg_params.unregisterations {
                                    lsp_info!(
                                        "LSP-SERVER-REQUEST",
                                        "Dynamic unregistration: {} (id: {})",
                                        unreg.method,
                                        unreg.id
                                    );
                                    server.set_capability_by_method(&unreg.method, false);
                                }
                            }
                        }
                        Err(e) => {
                            lsp_warn!(
                                "LSP-SERVER-REQUEST",
                                "Failed to parse client/unregisterCapability params: {}",
                                e
                            );
                        }
                    }
                }

                // Always acknowledge success
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let response_msg = JsonRpcMessage::response(id, serde_json::Value::Null);
                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send client/unregisterCapability response: {}",
                                e
                            );
                        }
                    }
                }
            }
            "workspace/configuration" => {
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let response_array = workspace_configuration_values(
                            server.language(),
                            request.params.as_ref(),
                        );
                        let response_msg =
                            JsonRpcMessage::response(id, serde_json::Value::Array(response_array));
                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send workspace/configuration response: {}",
                                e
                            );
                        }
                    }
                }
            }
            "window/showMessageRequest" => {
                // Server wants to show a message with action buttons.
                // Respond with null (no action selected) to unblock the server.
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let response_msg = JsonRpcMessage::response(id, serde_json::Value::Null);
                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send showMessageRequest response: {}",
                                e
                            );
                        }
                    }
                }
            }
            "window/workDoneProgress/create" => {
                // Server wants to create a progress token — acknowledge with success
                // Responding with an error crashes some LSP servers (e.g. typescript-language-server)
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let response_msg = JsonRpcMessage::response(id, serde_json::Value::Null);
                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send workDoneProgress/create response: {}",
                                e
                            );
                        }
                    }
                }
            }
            _ => {
                lsp_warn!(
                    "LSP-SERVER-REQUEST",
                    "Unsupported server request: {}",
                    method
                );

                // Send "method not found" error response
                if let Some(id) = request_id {
                    if let Some(server) = self.servers.get(server_id) {
                        let error_response = protocol::ResponseError {
                            code: -32601, // Method not found
                            message: format!("Method not supported: {}", method),
                            data: None,
                        };

                        let response_msg = JsonRpcMessage::error_response(id, error_response);

                        if let Err(e) = server.send_response(response_msg).await {
                            lsp_error!(
                                "LSP-SERVER-REQUEST",
                                "Failed to send error response: {}",
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    /// Handles incoming notifications and requests from language servers
    /// `server_id` is the DashMap key: language_id for primaries, "language_id:companion_id" for companions
    pub async fn handle_notification(&self, server_id: &str, message: JsonRpcMessage) {
        // Check if this is a request from the server (needs a response)
        if message.is_request() {
            self.handle_server_request(server_id, message).await;
            return;
        }

        // Handle notifications (no response needed)
        if let Some(method) = &message.method {
            match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    if let Some(params) = message.params {
                        // Clone params for error message before moving
                        let params_clone = params.clone();
                        match serde_json::from_value::<PublishDiagnosticsParams>(params) {
                            Ok(diag_params) => {
                                self.set_diagnostics(
                                    diag_params.uri,
                                    server_id,
                                    diag_params.diagnostics,
                                    diag_params.version,
                                )
                                .await;
                            }
                            Err(e) => {
                                // ERROR: Failed to parse publishDiagnostics - this is critical for user feedback
                                lsp_error!(
                                    &format!("LSP:{}", server_id),
                                    "Failed to parse publishDiagnostics notification: {}",
                                    e
                                );
                                // Show params preview for debugging
                                let params_str = format!("{:?}", params_clone);
                                let preview = if params_str.len() > 500 {
                                    format!(
                                        "{}...",
                                        crate::unicode::truncate_bytes(&params_str, 500)
                                    )
                                } else {
                                    params_str
                                };
                                lsp_error!(
                                    &format!("LSP:{}", server_id),
                                    "Malformed diagnostics params: {}",
                                    preview
                                );
                            }
                        }
                    }
                }
                "window/showMessage" => {
                    // Only show messages if OVIM_LSP_DEBUG is set to avoid cluttering the terminal
                    if std::env::var("OVIM_LSP_DEBUG").is_ok() {
                        if let Some(params) = message.params {
                            if let Ok(msg_params) =
                                serde_json::from_value::<lsp_types::ShowMessageParams>(params)
                            {
                                // Format message with severity prefix
                                let prefix = match msg_params.typ {
                                    lsp_types::MessageType::ERROR => "LSP Error",
                                    lsp_types::MessageType::WARNING => "LSP Warning",
                                    lsp_types::MessageType::INFO => "LSP Info",
                                    lsp_types::MessageType::LOG => "LSP Log",
                                    _ => "LSP",
                                };
                                let type_str = match msg_params.typ {
                                    lsp_types::MessageType::ERROR => "ERROR",
                                    lsp_types::MessageType::WARNING => "WARN",
                                    lsp_types::MessageType::INFO => "INFO",
                                    lsp_types::MessageType::LOG => "LOG",
                                    _ => "UNKNOWN",
                                };
                                let log_level = match msg_params.typ {
                                    lsp_types::MessageType::ERROR => {
                                        crate::lsp::logger::LogLevel::Error
                                    }
                                    lsp_types::MessageType::WARNING => {
                                        crate::lsp::logger::LogLevel::Warning
                                    }
                                    lsp_types::MessageType::INFO => {
                                        crate::lsp::logger::LogLevel::Info
                                    }
                                    _ => crate::lsp::logger::LogLevel::Info,
                                };
                                crate::lsp::logger::log_message(
                                    log_level,
                                    &format!("{}:{}", server_id, prefix),
                                    &format!("{}: {}", type_str, msg_params.message),
                                );
                            }
                        }
                    }
                }
                "window/logMessage" => {
                    if let Some(params) = message.params {
                        if let Ok(log_params) =
                            serde_json::from_value::<lsp_types::LogMessageParams>(params)
                        {
                            // Only log if OVIM_LSP_DEBUG is set
                            let log_level = match log_params.typ {
                                lsp_types::MessageType::ERROR => {
                                    crate::lsp::logger::LogLevel::Error
                                }
                                lsp_types::MessageType::WARNING => {
                                    crate::lsp::logger::LogLevel::Warning
                                }
                                lsp_types::MessageType::INFO => crate::lsp::logger::LogLevel::Info,
                                _ => crate::lsp::logger::LogLevel::Debug,
                            };
                            let prefix = match log_params.typ {
                                lsp_types::MessageType::ERROR => "ERROR",
                                lsp_types::MessageType::WARNING => "WARN",
                                lsp_types::MessageType::INFO => "INFO",
                                lsp_types::MessageType::LOG => "LOG",
                                _ => "UNKNOWN",
                            };
                            crate::lsp::logger::log_message(
                                log_level,
                                &format!("LSP:{}:{}", server_id, prefix),
                                &log_params.message,
                            );
                        }
                    }
                }
                "$/progress" => {
                    // Progress notifications from LSP server (e.g., jdtls indexing)
                    // These provide real-time feedback about long-running operations
                    if let Some(params) = &message.params {
                        // Try to parse as ProgressParams
                        if let Ok(progress) =
                            serde_json::from_value::<lsp_types::ProgressParams>(params.clone())
                        {
                            // Extract meaningful message from progress
                            let message_opt = match &progress.value {
                                lsp_types::ProgressParamsValue::WorkDone(work_done) => {
                                    match work_done {
                                        lsp_types::WorkDoneProgress::Begin(begin) => {
                                            Some(format!("{}: {}", server_id, begin.title,))
                                        }
                                        lsp_types::WorkDoneProgress::Report(report) => {
                                            if let Some(msg) = &report.message {
                                                Some(format!("{}: {}", server_id, msg))
                                            } else {
                                                report.percentage.map(|percentage| {
                                                    format!("{}: {}%", server_id, percentage)
                                                })
                                            }
                                        }
                                        lsp_types::WorkDoneProgress::End(end) => {
                                            if let Some(msg) = &end.message {
                                                Some(format!("{}: {}", server_id, msg))
                                            } else {
                                                Some(format!("{}: Complete", server_id))
                                            }
                                        }
                                    }
                                }
                            };

                            // Store and log progress messages for UI display
                            if let Some(message) = message_opt {
                                lsp_info!("Progress", "{}", message);
                                // Store latest progress message (will be cleared on End)
                                let mut current_progress = self.current_progress.lock().await;
                                let key = (server_id.to_string(), progress.token.clone());
                                match &progress.value {
                                    lsp_types::ProgressParamsValue::WorkDone(
                                        lsp_types::WorkDoneProgress::End(_),
                                    ) => {
                                        current_progress.remove(&key);
                                    }
                                    _ => {
                                        current_progress.insert(key, message);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {
                    // Silently ignore unknown notifications
                    lsp_debug!(
                        &format!("LSP:{}", server_id),
                        "Unknown notification: {}",
                        method
                    );
                }
            }
        }
    }

    /// Processes pending notifications from language servers
    /// Should be called regularly from the main event loop
    pub async fn process_notifications(self: &Arc<Self>) -> usize {
        let mut rx = self.notification_rx.lock().await;
        let mut notifications = Vec::new();

        // Process all pending notifications (non-blocking)
        while let Ok(notification) = rx.try_recv() {
            notifications.push(notification);
        }
        drop(rx);

        let count = notifications.len();
        for notification in notifications {
            if notification.message.is_request() {
                let manager = Arc::clone(self);
                tokio::spawn(async move {
                    manager
                        .handle_server_request(&notification.server_id, notification.message)
                        .await;
                });
            } else {
                self.handle_notification(&notification.server_id, notification.message)
                    .await;
            }
        }

        count
    }

    /// Processes pending flush requests from debounce timers
    /// Should be called regularly from the main event loop
    /// Returns the number of flush requests processed
    pub async fn process_flush_requests(self: &Arc<Self>) -> usize {
        let mut rx_opt = self.flush_rx.lock().await;
        let mut uris = Vec::new();
        if let Some(rx) = rx_opt.as_mut() {
            // Process all pending flush requests (non-blocking)
            while let Ok(uri) = rx.try_recv() {
                uris.push(uri);
            }
        }
        drop(rx_opt);

        let count = uris.len();
        for uri in uris {
            let manager = Arc::clone(self);
            tokio::spawn(async move {
                manager.process_flush_request(uri).await;
            });
        }

        count
    }

    async fn process_flush_request(self: Arc<Self>, uri: Uri) {
        // Clone the Arc out of the DashMap so we release the shard read-lock
        // before awaiting the debouncer mutex. This avoids the old try_lock()
        // fallback that silently degraded to single-server flush (OV-00149).
        let debouncer_arc = self
            .change_debouncers
            .get(&uri)
            .map(|entry| entry.value().clone());

        if let Some(debouncer_arc) = debouncer_arc {
            // DashMap shard released — safe to await.
            let language_id = {
                let debouncer = debouncer_arc.lock().await;
                debouncer.language_id.clone()
            };
            if let Err(e) = self
                .flush_pending_changes_broadcast(&uri, &language_id)
                .await
                .map(|_| ())
            {
                lsp_error!(
                    "Debounce",
                    "Error flushing changes for {}: {}",
                    uri.as_str(),
                    e
                );
            }
        } else if let Err(e) = self.flush_pending_changes(&uri).await {
            // Debouncer already removed (e.g., did_close raced) — single flush.
            lsp_error!(
                "Debounce",
                "Error flushing changes for {}: {}",
                uri.as_str(),
                e
            );
        }
    }

    /// Polls for pending workspace edits that need to be applied by the Editor
    /// Returns a Vec of workspace edits that should be applied (in order)
    /// This is called from the main event loop which has access to the Editor
    pub async fn poll_pending_workspace_edits(&self) -> Vec<lsp_types::WorkspaceEdit> {
        let mut rx = self.workspace_edit_rx.lock().await;
        let mut edits = Vec::new();

        // Drain all pending workspace edits (non-blocking)
        while let Ok(edit) = rx.try_recv() {
            edits.push(edit);
        }

        edits
    }

    /// Starts a background task to listen for notifications and requests from a language server.
    /// `server_id` is the DashMap key: language_id for primaries, "language_id:companion_id" for companions.
    pub async fn start_notification_listener(&self, server_id: String) {
        let server = self
            .servers
            .get(&server_id)
            .map(|entry| entry.value().clone());

        if let Some(server) = server {
            let tx = self.notification_tx.clone();
            let sid = server_id.clone();
            let dropped_counter = self.dropped_notifications.clone();

            let handle = tokio::spawn(async move {
                while let Some(msg) = server.receive().await {
                    // Handle both notifications (no id) and requests from server (has id)
                    if msg.is_notification() || msg.is_request() {
                        // Send to manager for processing
                        let notification = LspNotification {
                            server_id: sid.clone(),
                            message: msg,
                        };

                        // BUG FIX: Use try_send instead of send to avoid blocking
                        // If channel is full, drop the notification and increment counter
                        // This prevents deadlocks when the receiver is slow
                        match tx.try_send(notification) {
                            Ok(()) => {
                                // Successfully sent
                            }
                            Err(mpsc::error::TrySendError::Full(dropped)) => {
                                let count = dropped_counter.fetch_add(1, Ordering::Relaxed);
                                // Always log when dropping server-initiated requests (they expect a response)
                                if dropped.message.is_request() {
                                    lsp_error!(
                                        "Listener",
                                        "Dropped server-initiated request (channel full): method={:?}",
                                        dropped.message.method
                                    );
                                } else if count.is_multiple_of(100) {
                                    // Log every 100 dropped notifications to avoid spam
                                    lsp_error!(
                                        "Listener",
                                        "Notification channel full, dropped {} notifications so far",
                                        count + 1
                                    );
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                // Manager dropped, stop listening
                                lsp_error!(
                                    "Listener",
                                    "Notification channel closed, stopping listener"
                                );
                                break;
                            }
                        }
                    }
                }
            });

            // Store the handle so we can abort it on server stop
            self.listener_handles.insert(server_id, handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::protocol::RequestId;
    use std::str::FromStr;

    fn progress_notification(token: serde_json::Value, value: serde_json::Value) -> JsonRpcMessage {
        JsonRpcMessage::notification(
            "$/progress".to_string(),
            serde_json::json!({ "token": token, "value": value }),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn progress_end_only_clears_its_own_token() {
        let manager = LspManager::new();

        manager
            .handle_notification(
                "java",
                progress_notification(
                    serde_json::json!("index"),
                    serde_json::json!({ "kind": "begin", "title": "Indexing" }),
                ),
            )
            .await;
        manager
            .handle_notification(
                "java",
                progress_notification(
                    serde_json::json!(2),
                    serde_json::json!({ "kind": "begin", "title": "Building" }),
                ),
            )
            .await;

        assert_eq!(manager.current_progress.lock().await.len(), 2);

        manager
            .handle_notification(
                "java",
                progress_notification(
                    serde_json::json!("index"),
                    serde_json::json!({ "kind": "end" }),
                ),
            )
            .await;

        let progress = manager.current_progress.lock().await;
        assert_eq!(progress.len(), 1);
        assert_eq!(
            progress.get(&("java".to_string(), lsp_types::ProgressToken::Number(2))),
            Some(&"java: Building".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_notifications_does_not_block_on_server_requests() {
        let manager = Arc::new(LspManager::new());

        for _ in 0..100 {
            manager
                .workspace_edit_tx
                .try_send(lsp_types::WorkspaceEdit::default())
                .expect("fill workspace edit queue");
        }

        let request = JsonRpcMessage::request(
            RequestId::Number(1),
            "workspace/applyEdit".to_string(),
            serde_json::to_value(lsp_types::ApplyWorkspaceEditParams {
                label: None,
                edit: lsp_types::WorkspaceEdit::default(),
            })
            .unwrap(),
        );

        manager
            .notification_tx
            .send(LspNotification {
                server_id: "java".to_string(),
                message: request,
            })
            .await
            .expect("queue server request");

        let processed =
            tokio::time::timeout(Duration::from_millis(100), manager.process_notifications())
                .await
                .expect("notification pump should stay non-blocking");

        assert_eq!(processed, 1);

        let mut workspace_rx = manager.workspace_edit_rx.lock().await;
        let _ = workspace_rx.try_recv();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_flush_requests_does_not_wait_for_debouncer_lock() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-flush.rs").expect("uri");
        let debouncer = Arc::new(Mutex::new(ChangeDebouncer::new(
            uri.clone(),
            "rust".to_string(),
            1,
        )));
        manager
            .change_debouncers
            .insert(uri.clone(), debouncer.clone());

        let debouncer_guard = debouncer.lock().await;
        manager
            .flush_tx
            .send(uri)
            .await
            .expect("queue flush request");

        let processed =
            tokio::time::timeout(Duration::from_millis(100), manager.process_flush_requests())
                .await
                .expect("flush pump should stay non-blocking");

        assert_eq!(processed, 1);

        drop(debouncer_guard);
    }

    /// Verifies that the debouncer updates old_text when the caller provides
    /// a fresh baseline (e.g., after undo). Previously, old_text was only
    /// set when None, causing stale baselines after undo.
    #[tokio::test(flavor = "current_thread")]
    async fn debouncer_updates_old_text_on_undo() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-undo-test.rs").expect("uri");

        // Simulate: initial content is "hello\n", flushed to server.
        // Edit 1: type "x" -> "hellox\n"
        manager
            .did_change(
                uri.clone(),
                "rust",
                Arc::from("hellox\n"),
                Some(Arc::from("hello\n")), // baseline: what the server has
            )
            .await
            .unwrap();

        // Verify debouncer state after first edit
        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            assert_eq!(&*debouncer.pending_text, "hellox\n");
            assert_eq!(debouncer.old_text.as_deref(), Some("hello\n"));
        }

        // Edit 2: type "y" -> "helloxy\n" (same baseline — server still has "hello\n")
        manager
            .did_change(
                uri.clone(),
                "rust",
                Arc::from("helloxy\n"),
                Some(Arc::from("hello\n")),
            )
            .await
            .unwrap();

        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            assert_eq!(&*debouncer.pending_text, "helloxy\n");
            // Baseline should still be "hello\n" (server hasn't changed)
            assert_eq!(debouncer.old_text.as_deref(), Some("hello\n"));
        }

        // Now simulate: flush happened, server has "helloxy\n".
        // Then user types "z" -> "helloxyz\n".
        // Caller passes new baseline: "helloxy\n" (what server now has).
        manager
            .did_change(
                uri.clone(),
                "rust",
                Arc::from("helloxyz\n"),
                Some(Arc::from("helloxy\n")), // new baseline after flush
            )
            .await
            .unwrap();

        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            assert_eq!(&*debouncer.pending_text, "helloxyz\n");
            // Baseline MUST be updated to "helloxy\n", not stuck at "hello\n"
            assert_eq!(
                debouncer.old_text.as_deref(),
                Some("helloxy\n"),
                "Debouncer must update old_text when caller provides a new baseline"
            );
        }

        // Simulate undo: buffer goes back to "helloxy\n".
        // Server still has "helloxy\n" (from the last flush).
        // Baseline is "helloxy\n" — same as pending_text.
        manager
            .did_change(
                uri.clone(),
                "rust",
                Arc::from("helloxy\n"),
                Some(Arc::from("helloxy\n")),
            )
            .await
            .unwrap();

        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            assert_eq!(&*debouncer.pending_text, "helloxy\n");
            assert_eq!(debouncer.old_text.as_deref(), Some("helloxy\n"));
            // When pending_text == old_text, compute_simple_diff returns None
            // (no diff to send). This is correct — server already has this content.
        }
    }

    /// OV-00210: did_open's contains_key check and the subsequent insert
    /// must be atomic. If a previous caller has already populated
    /// document_versions for this URI, the second caller must return
    /// Ok(()) without attempting to send a duplicate didOpen and without
    /// looking up servers (which would panic in this test fixture).
    #[tokio::test(flavor = "current_thread")]
    async fn did_open_skips_when_uri_already_claimed() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-already-open.rs").expect("uri");

        // Pre-populate document_versions to simulate a prior didOpen.
        manager
            .document_versions
            .lock()
            .await
            .insert(uri.clone(), 7);

        // Even though no server is registered, this must succeed because
        // the duplicate guard returns early before touching servers.
        let result = manager
            .did_open(uri.clone(), "rust", 1, "fn main() {}".to_string())
            .await;
        assert!(
            result.is_ok(),
            "second did_open should be a no-op, got {:?}",
            result
        );

        // Version must be unchanged (the second call must not overwrite).
        let versions = manager.document_versions.lock().await;
        assert_eq!(versions.get(&uri), Some(&7));
    }

    /// OV-00210: did_open_broadcast must guard against duplicate
    /// notifications the same way did_open does. Without the guard, two
    /// concurrent broadcasts would each fan out a didOpen to every server
    /// in the group — a protocol violation.
    #[tokio::test(flavor = "current_thread")]
    async fn did_open_broadcast_skips_when_uri_already_claimed() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-already-broadcast.rs").expect("uri");

        manager
            .document_versions
            .lock()
            .await
            .insert(uri.clone(), 3);

        // No server group registered. Without the guard this would attempt
        // to look up servers_for_document_uri and return an error. With the
        // guard it returns Ok early.
        let result = manager
            .did_open_broadcast(uri.clone(), "rust", 1, "fn main() {}".to_string())
            .await;
        assert!(
            result.is_ok(),
            "second did_open_broadcast should be a no-op, got {:?}",
            result
        );

        let versions = manager.document_versions.lock().await;
        assert_eq!(versions.get(&uri), Some(&3));
    }

    /// OV-00210: when did_open fails to find a server, it must NOT leave
    /// a stale entry in document_versions. Otherwise the next attempt
    /// (with a properly-registered server) would be silently skipped.
    #[tokio::test(flavor = "current_thread")]
    async fn did_open_rolls_back_claim_on_missing_server() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-rollback.rs").expect("uri");

        let result = manager
            .did_open(uri.clone(), "no-such-language", 1, "x".to_string())
            .await;
        assert!(result.is_err(), "expected missing-server error");

        let versions = manager.document_versions.lock().await;
        assert!(
            !versions.contains_key(&uri),
            "claim must be rolled back on failure"
        );
    }
}

#[cfg(test)]
mod workspace_configuration_tests {
    use super::workspace_configuration_values;

    #[test]
    fn lua_configuration_declares_only_the_host_vim_global() {
        let params = serde_json::json!({
            "items": [
                {"section": "Lua"},
                {"section": "Lua.diagnostics"},
                {"section": "unmanaged"}
            ]
        });
        let values = workspace_configuration_values("lua", Some(&params));

        assert_eq!(values[0]["runtime"]["version"], "Lua 5.4");
        assert_eq!(
            values[0]["diagnostics"]["globals"],
            serde_json::json!(["vim"])
        );
        assert_eq!(values[1]["globals"], serde_json::json!(["vim"]));
        assert!(values[2].is_null());
    }

    #[test]
    fn unconfigured_languages_retain_null_configuration_responses() {
        let params = serde_json::json!({"items": [{"section": "rust"}]});
        assert_eq!(
            workspace_configuration_values("rust", Some(&params)),
            vec![serde_json::Value::Null]
        );
    }
}
