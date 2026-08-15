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

/// Hard ceiling for lifecycle notifications (didOpen/didSave/didClose).
/// A wedged-but-alive server (stdin backpressure with a full outgoing
/// channel) must degrade LSP, not freeze the editor tick that awaits these
/// broadcasts inline (OV-00333). didChange flushes already carry their own
/// per-server timeout in `flush_pending_changes_broadcast`.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// Sends a notification with `NOTIFY_TIMEOUT` applied (OV-00333).
async fn notify_with_timeout(
    server: &super::LanguageServer,
    method: &str,
    params: serde_json::Value,
) -> Result<()> {
    notify_with_deadline(server, method, params, NOTIFY_TIMEOUT).await
}

async fn notify_with_deadline(
    server: &super::LanguageServer,
    method: &str,
    params: serde_json::Value,
    deadline: Duration,
) -> Result<()> {
    match tokio::time::timeout(deadline, server.notify(method, params)).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "{} notification timed out after {:?} (server not reading stdin?)",
            method,
            deadline
        )),
    }
}

/// Projects workspace settings onto the sections a `workspace/configuration`
/// request asks for. `settings` is the full settings tree for the requesting
/// server (or `None` when ovim has no settings for it), resolved by the
/// caller from the server's language and workspace root.
fn workspace_configuration_values(
    settings: Option<&serde_json::Value>,
    params: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    let items = params
        .and_then(|params| params.get("items"))
        .and_then(serde_json::Value::as_array);
    let Some(items) = items else {
        return Vec::new();
    };
    let Some(settings) = settings else {
        return vec![serde_json::Value::Null; items.len()];
    };

    items
        .iter()
        .map(|item| {
            let Some(section) = item.get("section").and_then(serde_json::Value::as_str) else {
                return settings.clone();
            };
            section
                .split('.')
                .try_fold(settings, |value, key| value.get(key))
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

        let opened_text: Arc<str> = Arc::from(text.as_str());
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version,
                text,
            },
        };

        if let Err(e) = notify_with_timeout(
            &server,
            "textDocument/didOpen",
            serde_json::to_value(params)?,
        )
        .await
        {
            // Roll back so the next attempt isn't silently masked by the claim.
            self.document_versions.lock().await.remove(&uri);
            return Err(e);
        }

        lsp_debug!("LSP-NOTIFY", "textDocument/didOpen sent successfully");

        // Record the exact text the server now holds — the baseline for all
        // future incremental diffs to this server (OV-00326).
        self.server_texts
            .insert((language_id.to_string(), uri.clone()), opened_text);

        let mut sent = self.last_sent_versions.lock().await;
        sent.insert(uri, version);

        Ok(())
    }

    /// Sends textDocument/didChange to one server, diffing against the
    /// authoritative per-server baseline in `server_texts` (the exact text
    /// that server last received). Editor-side snapshots are never used as
    /// a diff baseline — they can lag or be poisoned by flush races, and an
    /// incremental edit computed against anything other than the server's
    /// real content corrupts the server's copy of the document (OV-00326).
    ///
    /// Returns `Ok(true)` if the notification was actually sent, or
    /// `Ok(false)` if the baseline already matched (nothing to send).
    async fn send_did_change_to_server(
        &self,
        uri: &Uri,
        server_id: &str,
        text: &Arc<str>,
        version: i32,
    ) -> Result<bool> {
        // Get server reference
        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("No server for language: {}", server_id))?;

        let supports_incremental = server.supports_incremental_sync().await;
        let baseline = self
            .server_texts
            .get(&(server_id.to_string(), uri.clone()))
            .map(|entry| entry.value().clone());

        let full_doc_size = text.len();
        let content_changes = match (supports_incremental, &baseline) {
            (true, Some(old)) => {
                if let Some((range, new_text)) = compute_simple_diff(old, text) {
                    vec![TextDocumentContentChangeEvent {
                        range: Some(range),
                        range_length: None,
                        text: new_text,
                    }]
                } else {
                    // Content identical to what this server already has.
                    return Ok(false);
                }
            }
            _ => {
                if std::env::var("OVIM_LSP_DEBUG").is_ok() {
                    let reason = if !supports_incremental {
                        "server doesn't support incremental"
                    } else {
                        "no recorded server baseline"
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
                    text: text.to_string(),
                }]
            }
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

        // The notification is in the server's ordered outgoing queue: record
        // `text` as this server's content so the next diff builds on it.
        self.server_texts
            .insert((server_id.to_string(), uri.clone()), text.clone());

        Ok(true)
    }

    /// Flushes pending changes for a document (sends immediately).
    /// Delegates to the broadcast flush — all servers in the document's
    /// group must receive the update or per-server baselines drift.
    pub async fn flush_pending_changes(&self, uri: &Uri) -> Result<()> {
        let language_id = {
            let Some(debouncer_arc) = self
                .change_debouncers
                .get(uri)
                .map(|entry| entry.value().clone())
            else {
                return Ok(());
            };
            let debouncer = debouncer_arc.lock().await;
            debouncer.language_id.clone()
        };
        self.flush_pending_changes_broadcast(uri, &language_id)
            .await
            .map(|_| ())
    }

    /// (Re)starts the debounce timer on a locked debouncer.
    fn restart_debounce_timer(&self, debouncer: &mut ChangeDebouncer, uri: Uri) {
        debouncer.cancel_timer();
        let flush_tx = self.flush_tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(CHANGE_DEBOUNCE_MS)).await;
            // Timer expired - request flush via channel
            if let Err(e) = flush_tx.send(uri).await {
                lsp_error!("Debounce", "Error sending flush request: {}", e);
            }
        });
        debouncer.timer_handle = Some(handle);
    }

    /// Sends textDocument/didChange notification with debouncing.
    /// Coalesces rapid changes to reduce LSP traffic by ~1000x.
    ///
    /// **Version is bumped immediately** (not on flush) so that stale
    /// `publishDiagnostics` arriving during the debounce window are correctly
    /// rejected by `set_diagnostics()`.  The assigned version is stored in the
    /// debouncer and used when the flush finally sends the content (OV-00163).
    ///
    /// `old_text` semantics: `None` declares that the server-side content is
    /// not trustworthy (e.g. reload after an external write) and drops the
    /// recorded per-server baselines, forcing the next flush to send a full
    /// document update. `Some(_)` is accepted for API compatibility but the
    /// value itself is ignored — incremental diffs are always computed at
    /// flush time against `server_texts`, the text each server actually
    /// received (OV-00326).
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

        if old_text.is_none() {
            // No trustworthy baseline: force a full-document update on the
            // next flush for every server in this document's group.
            self.server_texts.retain(|(_, u), _| u != &uri);
        }

        // Get or create the document's debouncer. The entry persists until
        // didClose — flushing takes the payload but never removes the entry,
        // so a concurrently queued edit can never be orphaned.
        let debouncer_arc = self
            .change_debouncers
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ChangeDebouncer::new(language_id.to_string()))))
            .clone();

        let mut debouncer = debouncer_arc.lock().await;
        debouncer.pending = Some(super::PendingChange {
            text,
            version: assigned_version,
        });
        self.restart_debounce_timer(&mut debouncer, uri);

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

        notify_with_timeout(
            &server,
            "textDocument/didSave",
            serde_json::to_value(params)?,
        )
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

        notify_with_timeout(
            &server,
            "textDocument/didClose",
            serde_json::to_value(params)?,
        )
        .await?;

        // Clean up internal state
        let mut versions = self.document_versions.lock().await;
        versions.remove(&uri);
        drop(versions);
        self.last_sent_versions.lock().await.remove(&uri);

        // Remove debouncer for this document
        self.change_debouncers.remove(&uri);
        self.flush_gates.remove(&uri);
        self.server_texts.retain(|(_, u), _| u != &uri);
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

        let opened_text: Arc<str> = Arc::from(text.as_str());
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
                if let Err(e) = notify_with_timeout(
                    &server,
                    "textDocument/didOpen",
                    serde_json::to_value(params)?,
                )
                .await
                {
                    lsp_warn!("LSP-BROADCAST", "didOpen failed for server {}: {}", sid, e);
                } else {
                    // Record the exact text this server now holds — the
                    // baseline for future incremental diffs (OV-00326).
                    self.server_texts
                        .insert((sid.clone(), uri.clone()), opened_text.clone());
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
    ///
    /// Flushes are serialized per URI (`flush_gates`) so two flushes can
    /// never interleave their sends and deliver versions out of order. The
    /// debouncer entry itself is never removed here — `did_change()` may
    /// already hold a clone of its Arc, and removing the entry orphaned any
    /// edit queued concurrently with the flush (OV-00326).
    ///
    /// Returns the `(content, version)` that was actually sent to the LSP
    /// server, or `None` if there was nothing to flush.  Callers that record
    /// `synced_content` (e.g. inlay-hint / completion tasks) **must** use the
    /// returned content — the debouncer may have been updated by another thread
    /// since the caller captured its snapshot. On partial or total send
    /// failure the pending change is re-armed for a timer-driven retry
    /// (unless a newer edit superseded it) and an error is returned; the
    /// change is never silently discarded.
    pub async fn flush_pending_changes_broadcast(
        &self,
        uri: &Uri,
        language_id: &str,
    ) -> Result<Option<(String, i32)>> {
        let Some(debouncer_arc) = self
            .change_debouncers
            .get(uri)
            .map(|entry| entry.value().clone())
        else {
            return Ok(None);
        };

        let gate = self
            .flush_gates
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _flush_permit = gate.lock().await;

        // Take the pending payload; the map entry stays so concurrent
        // did_change() calls keep landing in the same debouncer.
        let (text, version) = {
            let mut debouncer = debouncer_arc.lock().await;
            debouncer.cancel_timer();
            match debouncer.pending.take() {
                Some(pending) => (pending.text, pending.version),
                None => return Ok(None),
            }
        };

        let text_string = text.to_string();
        let server_ids = self.servers_for_document_uri(language_id, uri);
        let mut any_sent = false;
        let mut all_synced = !server_ids.is_empty();
        for sid in &server_ids {
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.send_did_change_to_server(uri, sid, &text, version),
            )
            .await
            {
                Ok(Ok(actually_sent)) => {
                    any_sent = any_sent || actually_sent;
                    // Even no-diff (Ok(false)) means this server is in sync
                    // (OV-00211).
                }
                Ok(Err(e)) => {
                    all_synced = false;
                    lsp_warn!("LSP-BROADCAST", "Flush failed for server {}: {}", sid, e);
                }
                Err(_) => {
                    all_synced = false;
                    lsp_warn!(
                        "LSP-BROADCAST",
                        "Timeout flushing changes for server {} (5s)",
                        sid
                    );
                }
            }
        }
        if all_synced {
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

        // At least one server did not receive the update (or no server is
        // registered yet). Re-arm the pending change for a timer-driven
        // retry unless a newer edit superseded it while we were sending —
        // the newer edit re-diffs against each server's recorded baseline,
        // so dropping this payload in that case loses nothing.
        {
            let mut debouncer = debouncer_arc.lock().await;
            if debouncer.pending.is_none() {
                debouncer.pending = Some(super::PendingChange { text, version });
                self.restart_debounce_timer(&mut debouncer, uri.clone());
            }
        }
        Err(anyhow!(
            "didChange flush incomplete for {} (queued for retry)",
            uri.as_str()
        ))
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
                if let Err(e) = notify_with_timeout(
                    &server,
                    "textDocument/didSave",
                    serde_json::to_value(params)?,
                )
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
                if let Err(e) = notify_with_timeout(
                    &server,
                    "textDocument/didClose",
                    serde_json::to_value(params)?,
                )
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
        self.flush_gates.remove(&uri);
        self.server_texts.retain(|(_, u), _| u != &uri);
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
                        let root = self.server_roots.get(server_id).map(|root| root.clone());
                        let settings = root.and_then(|root| {
                            super::server::workspace_settings_for_root(server.language(), &root)
                        });
                        let response_array = workspace_configuration_values(
                            settings.as_ref(),
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
        let debouncer = Arc::new(Mutex::new(ChangeDebouncer::new("rust".to_string())));
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
    /// OV-00326: coalescing keeps the latest text/version, and a flush must
    /// never remove the debouncer map entry — `did_change()` may already
    /// hold a clone of the Arc, and removing the entry orphaned edits
    /// queued concurrently with the flush (their timer then flushed
    /// nothing and the change was silently lost).
    #[tokio::test(flavor = "current_thread")]
    async fn flush_keeps_debouncer_entry_and_pending_coalesces() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-orphan-test.rs").expect("uri");

        manager
            .did_change(uri.clone(), "rust", Arc::from("v1\n"), None)
            .await
            .unwrap();
        manager
            .did_change(uri.clone(), "rust", Arc::from("v2\n"), None)
            .await
            .unwrap();

        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            let pending = debouncer.pending.as_ref().expect("pending change");
            assert_eq!(&*pending.text, "v2\n");
            assert_eq!(pending.version, 2);
        }

        // Flush with no servers registered: the payload cannot be delivered.
        let result = manager.flush_pending_changes_broadcast(&uri, "rust").await;
        assert!(
            result.is_err(),
            "undeliverable flush must surface an error, not silently drop the edit"
        );

        // The entry must survive the flush attempt...
        assert!(
            manager.change_debouncers.contains_key(&uri),
            "flush must not remove the debouncer entry (orphaned-edit race, OV-00326)"
        );
        // ...and the pending change must be re-armed for retry, not discarded.
        {
            let entry = manager.change_debouncers.get(&uri).unwrap();
            let debouncer = entry.lock().await;
            let pending = debouncer
                .pending
                .as_ref()
                .expect("failed flush must retain the pending change for retry (OV-00326)");
            assert_eq!(&*pending.text, "v2\n");
            assert_eq!(pending.version, 2);
        }
        // last_sent must NOT claim the undelivered version.
        assert_eq!(manager.get_last_sent_version(&uri).await, 0);
    }

    /// OV-00326: passing `old_text: None` declares the server-side content
    /// untrustworthy (external reload) — recorded per-server baselines must
    /// be dropped so the next flush sends a full-document update instead of
    /// an incremental diff against a baseline the server may not have.
    #[tokio::test(flavor = "current_thread")]
    async fn did_change_without_baseline_drops_server_texts() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-force-full.rs").expect("uri");
        let other_uri = Uri::from_str("file:///tmp/ovim-other.rs").expect("uri");

        manager
            .server_texts
            .insert(("rust".to_string(), uri.clone()), Arc::from("stale\n"));
        manager
            .server_texts
            .insert(("rust".to_string(), other_uri.clone()), Arc::from("keep\n"));

        manager
            .did_change(uri.clone(), "rust", Arc::from("fresh\n"), None)
            .await
            .unwrap();

        assert!(
            !manager
                .server_texts
                .contains_key(&("rust".to_string(), uri.clone())),
            "force-full resend must drop the recorded baseline for the document"
        );
        assert!(
            manager
                .server_texts
                .contains_key(&("rust".to_string(), other_uri.clone())),
            "baselines for other documents must be untouched"
        );
    }

    /// Applies one LSP TextDocumentContentChangeEvent to a mirror string the
    /// way a server would (UTF-16 positions, lines split on '\n').
    fn apply_content_change(mirror: &str, change: &serde_json::Value) -> String {
        let text = change["text"].as_str().expect("change text").to_string();
        let Some(range) = change.get("range").filter(|r| !r.is_null()) else {
            // Full-document sync
            return text;
        };
        let pos = |p: &serde_json::Value| -> (usize, u32) {
            (
                p["line"].as_u64().expect("line") as usize,
                p["character"].as_u64().expect("character") as u32,
            )
        };
        let (sl, sc) = pos(&range["start"]);
        let (el, ec) = pos(&range["end"]);
        let lines: Vec<&str> = mirror.split('\n').collect();
        let offset_of = |line: usize, utf16_col: u32| -> usize {
            let mut offset = 0;
            for l in lines.iter().take(line) {
                offset += l.len() + 1;
            }
            let line_text = lines.get(line).copied().unwrap_or("");
            let char_col = crate::lsp::position::utf16_to_char_col(line_text, utf16_col);
            offset
                + line_text
                    .chars()
                    .take(char_col)
                    .map(|c| c.len_utf8())
                    .sum::<usize>()
        };
        let start = offset_of(sl, sc).min(mirror.len());
        let end = offset_of(el, ec).min(mirror.len());
        format!("{}{}{}", &mirror[..start], text, &mirror[end..])
    }

    /// OV-00326 end-to-end: a server's copy of the document must track the
    /// editor exactly when incremental flushes interleave with new edits and
    /// with baselines the editor got wrong. The fake server records every
    /// frame ovim sends; replaying didOpen + the didChange stream must
    /// reproduce the final text, and versions must be strictly increasing.
    /// Under the old scheme, diffing against caller-supplied `old_text`
    /// (instead of what the server was actually sent) corrupted the mirror.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn incremental_flush_stream_reconstructs_editor_content() {
        let dir = std::env::temp_dir().join(format!(
            "ovim-sync-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create capture dir");
        let capture = dir.join("frames.bin");
        let capture_str = capture.to_string_lossy().to_string();

        let manager = Arc::new(LspManager::new());
        let server = super::super::server::LanguageServer::spawn(
            "rust",
            "sh",
            vec!["-c".to_string(), format!("exec cat > '{}'", capture_str)],
        )
        .await
        .expect("spawn fake server");
        server.force_incremental_sync_for_test(true);
        manager.servers.insert("rust".to_string(), server);
        manager
            .language_server_index
            .insert("rust".to_string(), vec!["rust".to_string()]);

        let uri = Uri::from_str("file:///tmp/ovim-sync-capture.rs").expect("uri");
        let v0 = "fn compute(x: u32) -> u32 {\n    x * 2\n}\n";
        manager
            .did_open(uri.clone(), "rust", 1, v0.to_string())
            .await
            .expect("didOpen");

        // Edit 1: normal keystroke; editor supplies the correct baseline
        // (which the manager must ignore in favor of its own records).
        let v1 = "fn compute(x: u32) -> u32 {\n    x * 2 + 1\n}\n";
        manager
            .did_change(uri.clone(), "rust", Arc::from(v1), Some(Arc::from(v0)))
            .await
            .unwrap();
        manager
            .flush_pending_changes_broadcast(&uri, "rust")
            .await
            .expect("flush v1");

        // Edit 2: the editor's snapshot lags a flush (the OV-00326 race) and
        // it passes a STALE baseline. The server's copy must still converge.
        let v2 = "fn compute(x: u32) -> u32 {\n    x * \"oops\"\n}\n";
        manager
            .did_change(
                uri.clone(),
                "rust",
                Arc::from(v2),
                Some(Arc::from(v0)), // stale: server was already sent v1
            )
            .await
            .unwrap();
        manager
            .flush_pending_changes_broadcast(&uri, "rust")
            .await
            .expect("flush v2");

        // Edit 3: coalesced pair — only the latest needs to reach the server.
        let v3a = "fn compute(x: u32) -> u32 {\n    x * \"oops\"\n}\n\nfn main() {}\n";
        let v3 = "fn compute(x: u32) -> u32 {\n    x + 40\n}\n\nfn main() {}\n";
        manager
            .did_change(uri.clone(), "rust", Arc::from(v3a), Some(Arc::from(v2)))
            .await
            .unwrap();
        manager
            .did_change(uri.clone(), "rust", Arc::from(v3), Some(Arc::from(v3a)))
            .await
            .unwrap();
        manager
            .flush_pending_changes_broadcast(&uri, "rust")
            .await
            .expect("flush v3");

        // Wait for the fake server process to have received all frames:
        // 1 didOpen + 3 didChange.
        let mut frames: Vec<serde_json::Value> = Vec::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let Ok(raw) = std::fs::read(&capture) else {
                continue;
            };
            frames = parse_lsp_frames(&raw);
            if frames
                .iter()
                .filter(|f| {
                    matches!(
                        f["method"].as_str(),
                        Some("textDocument/didOpen") | Some("textDocument/didChange")
                    )
                })
                .count()
                >= 4
            {
                break;
            }
        }

        let mut mirror = String::new();
        let mut last_version = 0i64;
        let mut did_change_count = 0;
        for frame in &frames {
            match frame["method"].as_str() {
                Some("textDocument/didOpen") => {
                    mirror = frame["params"]["textDocument"]["text"]
                        .as_str()
                        .expect("didOpen text")
                        .to_string();
                }
                Some("textDocument/didChange") => {
                    did_change_count += 1;
                    let version = frame["params"]["textDocument"]["version"]
                        .as_i64()
                        .expect("version");
                    assert!(
                        version > last_version,
                        "didChange versions must be strictly increasing (got {} after {})",
                        version,
                        last_version
                    );
                    last_version = version;
                    for change in frame["params"]["contentChanges"]
                        .as_array()
                        .expect("contentChanges")
                    {
                        mirror = apply_content_change(&mirror, change);
                    }
                }
                _ => {}
            }
        }

        assert_eq!(did_change_count, 3, "frames: {:#?}", frames);
        assert_eq!(
            mirror, v3,
            "server-side reconstruction diverged from the editor's buffer"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// OV-00333: a wedged-but-alive server (not reading stdin, outgoing
    /// channel full) must make lifecycle notifications error out instead of
    /// blocking the caller forever — before the fix, `:w`/open/close on a
    /// stopped server froze the editor tick indefinitely.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notify_times_out_when_server_stops_reading() {
        // A server process that never reads stdin.
        let server = super::super::server::LanguageServer::spawn(
            "rust",
            "sh",
            vec!["-c".to_string(), "exec sleep 300".to_string()],
        )
        .await
        .expect("spawn wedged server");

        // Fill the OS pipe buffer and the bounded outgoing channel so the
        // next notify would block forever without a deadline. A ~1MB params
        // payload saturates the pipe on the first write; the rest queue up.
        let big = serde_json::json!({ "pad": "x".repeat(1024 * 1024) });
        for _ in 0..110 {
            let send = server.notify("test/pad", big.clone());
            if tokio::time::timeout(Duration::from_millis(200), send)
                .await
                .is_err()
            {
                // Channel is full — notify itself now blocks. That's the
                // wedged state the deadline must break out of.
                break;
            }
        }

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            notify_with_deadline(
                &server,
                "textDocument/didSave",
                serde_json::json!({}),
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("notify_with_deadline must return promptly, not hang");
        assert!(
            result.is_err(),
            "wedged server must produce an error, not a silent success"
        );
    }

    /// OV-00336 (scope narrowed per external review): with unsent local
    /// edits, a stale NON-EMPTY unversioned publication is dropped (it must
    /// not later masquerade as describing the newer document), while an
    /// EMPTY publication — possibly the server's only retraction — is
    /// deferred for post-flush re-evaluation instead of being lost.
    #[tokio::test(flavor = "current_thread")]
    async fn unsent_edits_drop_stale_sets_but_defer_clears() {
        let manager = Arc::new(LspManager::new());
        let uri = Uri::from_str("file:///tmp/ovim-clear-defer.rs").expect("uri");

        // Unsent edits: version bumped, nothing flushed.
        manager
            .did_change(uri.clone(), "rust", Arc::from("v1\n"), None)
            .await
            .unwrap();

        let stale_diag = lsp_types::Diagnostic {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, 1),
            ),
            message: "stale".to_string(),
            ..Default::default()
        };
        manager
            .set_diagnostics(uri.clone(), "rust", vec![stale_diag], None)
            .await;
        assert!(
            manager.get_diagnostics(&uri).await.is_empty(),
            "stale non-empty publication must not be stored"
        );
        assert!(
            !manager
                .deferred_diagnostics
                .lock()
                .await
                .contains_key(&(uri.clone(), "rust".to_string())),
            "stale non-empty publication must not be deferred either"
        );

        manager
            .set_diagnostics(uri.clone(), "rust", Vec::new(), None)
            .await;
        assert!(
            manager
                .deferred_diagnostics
                .lock()
                .await
                .contains_key(&(uri.clone(), "rust".to_string())),
            "an empty publication (clear) must be deferred, not dropped"
        );
    }

    /// Splits raw captured bytes into parsed JSON-RPC message bodies.
    fn parse_lsp_frames(mut raw: &[u8]) -> Vec<serde_json::Value> {
        let mut frames = Vec::new();
        while let Some(header_end) = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4) {
            let headers = String::from_utf8_lossy(&raw[..header_end]);
            let Some(len) = headers
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
            else {
                break;
            };
            if raw.len() < header_end + len {
                break;
            }
            if let Ok(value) =
                serde_json::from_slice::<serde_json::Value>(&raw[header_end..header_end + len])
            {
                frames.push(value);
            }
            raw = &raw[header_end + len..];
        }
        frames
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

    fn lua_host_settings() -> serde_json::Value {
        serde_json::json!({
            "Lua": {
                "runtime": { "version": "Lua 5.4" },
                "diagnostics": { "globals": ["vim", "ovim"] }
            }
        })
    }

    #[test]
    fn settings_project_onto_requested_sections() {
        let settings = lua_host_settings();
        let params = serde_json::json!({
            "items": [
                {"section": "Lua"},
                {"section": "Lua.diagnostics"},
                {"section": "unmanaged"}
            ]
        });
        let values = workspace_configuration_values(Some(&settings), Some(&params));

        assert_eq!(values[0]["runtime"]["version"], "Lua 5.4");
        assert_eq!(
            values[0]["diagnostics"]["globals"],
            serde_json::json!(["vim", "ovim"])
        );
        assert_eq!(values[1]["globals"], serde_json::json!(["vim", "ovim"]));
        assert!(values[2].is_null());
    }

    #[test]
    fn sectionless_items_receive_the_full_settings_tree() {
        let settings = lua_host_settings();
        let params = serde_json::json!({"items": [{}]});
        assert_eq!(
            workspace_configuration_values(Some(&settings), Some(&params)),
            vec![settings]
        );
    }

    #[test]
    fn servers_without_settings_retain_null_configuration_responses() {
        let params = serde_json::json!({"items": [{"section": "rust"}]});
        assert_eq!(
            workspace_configuration_values(None, Some(&params)),
            vec![serde_json::Value::Null]
        );
    }
}
