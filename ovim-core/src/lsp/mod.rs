//! LSP (Language Server Protocol) client implementation
//!
//! This module provides LSP support for ovim, enabling IDE-like features such as:
//! - Diagnostics (errors and warnings)
//! - Go to definition
//! - Hover information
//! - Code completion
//! - Code actions
//! - Formatting
//!
//! # Architecture
//!
//! - `LspManager`: Central coordinator managing multiple language servers
//! - `LanguageServer`: Individual language server process management
//! - `protocol`: JSON-RPC message handling
//! - `types`: Type conversions and helpers

#[macro_use]
pub mod logger;
mod notifications;
pub mod position;
mod protocol;
mod requests;
mod server;
mod supervisor;
mod trigger_chars;
mod types;
mod utils;

pub use logger::{get_log_path, init_lsp_logging};
pub use position::{char_col_to_utf16, utf16_to_char_col};

pub use protocol::{JsonRpcMessage, RequestId};
pub use server::{LanguageServer, LanguageServerHealth};
pub use supervisor::{RestartPolicy, TaskSupervisor};
pub use trigger_chars::fallback_completion_trigger_characters;
pub use types::{uri_from_file_path, uri_to_file_path, LspPosition, LspRange};
pub use utils::compute_simple_diff;

use anyhow::Result;
use dashmap::{DashMap, DashSet};
use lsp_types::{Diagnostic, Uri};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Default)]
struct StoredDiagnostics {
    version: Option<i32>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
struct DeferredDiagnostics {
    diagnostics: Vec<Diagnostic>,
    document_version: i32,
    last_edit: Instant,
    apply_after: Instant,
}

/// Maximum document size in bytes (10MB)
/// Protects against OOM when opening/syncing large files
const MAX_DOCUMENT_SIZE: usize = 10 * 1024 * 1024;

/// Unversioned diagnostics can arrive out-of-date during rapid edits/saves.
/// Keep them suppressed briefly after local edits until LSP has a chance to
/// compute diagnostics for the latest content.
const UNVERSIONED_DIAGNOSTICS_SETTLE_MS: u64 = 150;

/// Debounce duration for textDocument/didChange notifications (milliseconds)
/// Coalesces rapid changes to reduce LSP traffic by ~1000x
/// Reduced to 150ms for faster diagnostics feedback (was 300ms)
const CHANGE_DEBOUNCE_MS: u64 = 150;

/// Notification message from a language server
#[derive(Clone)]
pub struct LspNotification {
    /// The server_id that sent this notification.
    /// For primary servers this equals the language_id (e.g., "rust").
    /// For companion servers this is "language_id:companion_id" (e.g., "typescript:tailwindcss").
    pub server_id: String,
    pub message: JsonRpcMessage,
}

/// Information about an active LSP server for introspection
#[derive(Clone, Debug, serde::Serialize)]
pub struct LspServerInfo {
    pub language: String,
    pub command: String,
    pub state: String,
    pub pending_requests: usize,
    pub has_capabilities: bool,
}

/// A coalesced didChange payload waiting for the debounce timer.
pub(crate) struct PendingChange {
    /// Full text of the pending change
    pub(crate) text: Arc<str>,

    /// LSP document version assigned when the change was received.
    /// Bumped immediately in `did_change()` so stale diagnostics can be
    /// rejected before the debounce timer fires.  Used by flush instead of
    /// re-incrementing.
    pub(crate) version: i32,
}

/// Debouncer for textDocument/didChange notifications.
/// Coalesces rapid changes to reduce LSP traffic.
///
/// One entry lives in `change_debouncers` per open document for the whole
/// didOpen..didClose lifetime. Flushing takes the pending payload under the
/// mutex but NEVER removes the map entry — the old remove-on-flush scheme
/// raced `did_change()` (which had already cloned the Arc out of the map)
/// and orphaned freshly queued edits in a debouncer whose timer flushed
/// nothing (OV-00326).
pub(crate) struct ChangeDebouncer {
    /// Language ID (e.g., "rust", "python")
    language_id: String,

    /// The coalesced change waiting to be flushed, if any.
    pending: Option<PendingChange>,

    /// Timer handle for the debounce delay
    timer_handle: Option<JoinHandle<()>>,
}

impl ChangeDebouncer {
    fn new(language_id: String) -> Self {
        Self {
            language_id,
            pending: None,
            timer_handle: None,
        }
    }

    /// Cancels the pending timer if any
    fn cancel_timer(&mut self) {
        if let Some(handle) = self.timer_handle.take() {
            handle.abort();
        }
    }
}

impl Drop for ChangeDebouncer {
    fn drop(&mut self) {
        self.cancel_timer();
    }
}

/// Central LSP manager coordinating all language servers
pub struct LspManager {
    /// Active language servers (one per language)
    /// Using DashMap for lock-free concurrent access
    servers: DashMap<String, LanguageServer>,

    /// Diagnostics per file URI, per server_id
    /// Outer key: URI, inner key: server_id, value: diagnostics from that server
    diagnostics: Mutex<HashMap<Uri, HashMap<String, StoredDiagnostics>>>,

    /// Cached merged diagnostics per URI (OV-00151)
    /// Invalidated when set_diagnostics() stores new data for a URI.
    /// Avoids calling merge_diagnostics() 2-3x per tick under lock.
    merged_diagnostics_cache: Mutex<HashMap<Uri, Vec<Diagnostic>>>,

    /// Document versions for change tracking (bumped immediately in did_change)
    document_versions: Mutex<HashMap<Uri, i32>>,

    /// Last version that was actually *sent* to the server via didChange.
    /// Used to detect when unversioned diagnostics are stale: if
    /// `last_sent < document_versions[uri]`, there are unsent edits and any
    /// unversioned diagnostics must have been computed against old content.
    last_sent_versions: Mutex<HashMap<Uri, i32>>,

    /// Last local edit time per document (for unversioned diagnostics staleness).
    /// If an unversioned publishDiagnostics arrives right after this timestamp,
    /// it may refer to pre-change content and should be ignored.
    last_local_edit: Mutex<HashMap<Uri, Instant>>,

    /// Latest unversioned diagnostics received during the post-edit settle window.
    /// These are deferred rather than discarded so an empty publication can still
    /// clear diagnostics from the previous document version.
    deferred_diagnostics: Mutex<HashMap<(Uri, String), DeferredDiagnostics>>,

    /// Channel for receiving notifications from language servers (bounded to prevent memory issues)
    notification_tx: mpsc::Sender<LspNotification>,
    notification_rx: Mutex<mpsc::Receiver<LspNotification>>,

    /// Pending changes being debounced per document
    /// Coalesces rapid changes to reduce LSP traffic by ~1000x
    change_debouncers: DashMap<Uri, Arc<Mutex<ChangeDebouncer>>>,

    /// Per-document flush serialization. Held across the actual didChange
    /// sends so two flushes for the same URI can never interleave and
    /// deliver versions out of order. Never held while queueing changes,
    /// so `did_change()` (called from the editor tick) never blocks on a
    /// slow server.
    flush_gates: DashMap<Uri, Arc<Mutex<()>>>,

    /// The exact document text each server last received (via didOpen or a
    /// successful didChange), keyed by (server_id, uri). This is the ONLY
    /// trustworthy baseline for incremental diffs: editor-side snapshots
    /// can lag or be poisoned by flush races, and diffing against anything
    /// other than what the server actually holds corrupts the server's
    /// copy of the document (OV-00326).
    server_texts: DashMap<(String, Uri), Arc<str>>,

    /// Channel for debounce flush requests (URI to flush)
    flush_tx: mpsc::Sender<Uri>,
    flush_rx: Mutex<Option<mpsc::Receiver<Uri>>>,

    /// Flag indicating diagnostics have changed and cache needs update
    diagnostics_changed: AtomicBool,

    /// Current progress messages keyed by server and LSP progress token.
    /// A server may run multiple operations concurrently, so server identity
    /// alone is not sufficient to track their independent lifetimes.
    current_progress: Mutex<HashMap<(String, lsp_types::ProgressToken), String>>,

    /// Channel for workspace edits that need to be applied by the Editor
    /// These come from server-initiated workspace/applyEdit requests
    workspace_edit_tx: mpsc::Sender<lsp_types::WorkspaceEdit>,
    workspace_edit_rx: Mutex<mpsc::Receiver<lsp_types::WorkspaceEdit>>,

    /// BUG FIX: Counter for dropped notifications when channel is full
    /// Prevents blocking when notification receiver is slow
    dropped_notifications: Arc<AtomicU64>,

    /// Tracks servers currently being started (prevents concurrent duplicate starts)
    starting_servers: DashSet<String>,

    /// Handles to notification listener tasks (for cleanup on server stop)
    listener_handles: DashMap<String, tokio::task::JoinHandle<()>>,

    /// Reverse index: language_id → server_ids serving that language.
    /// Maintained by start_server/start_companion_server/stop_server.
    /// Avoids O(n) DashMap scan in servers_for_language().
    language_server_index: DashMap<String, Vec<String>>,

    /// Maps server_id → root_path for root-based dedup
    server_roots: DashMap<String, std::path::PathBuf>,
}

/// Builds a composite server ID for companion LSP servers.
/// Primary servers use just `language_id`, companion servers use `language_id:companion_id`.
pub fn companion_server_id(language_id: &str, companion_id: &str) -> String {
    format!("{}:{}", language_id, companion_id)
}

/// Builds a composite server ID for root-scoped LSP servers.
/// First server for a language uses bare `language_id`; subsequent ones with different
/// roots use `language_id@<8-char hash of root>`.
fn root_server_id(language: &str, root_path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root_path.hash(&mut hasher);
    format!("{}@{:08x}", language, hasher.finish() as u32)
}

impl LspManager {
    /// Creates a new LSP manager
    pub fn new() -> Self {
        // Use bounded channel to prevent unbounded memory growth from notifications
        let (notification_tx, notification_rx) = mpsc::channel(1000);
        let (flush_tx, flush_rx) = mpsc::channel(100);
        let (workspace_edit_tx, workspace_edit_rx) = mpsc::channel(100);
        Self {
            servers: DashMap::new(),
            diagnostics: Mutex::new(HashMap::new()),
            merged_diagnostics_cache: Mutex::new(HashMap::new()),
            document_versions: Mutex::new(HashMap::new()),
            last_sent_versions: Mutex::new(HashMap::new()),
            last_local_edit: Mutex::new(HashMap::new()),
            deferred_diagnostics: Mutex::new(HashMap::new()),
            notification_tx,
            notification_rx: Mutex::new(notification_rx),
            change_debouncers: DashMap::new(),
            flush_gates: DashMap::new(),
            server_texts: DashMap::new(),
            flush_tx,
            flush_rx: Mutex::new(Some(flush_rx)),
            diagnostics_changed: AtomicBool::new(false),
            current_progress: Mutex::new(HashMap::new()),
            workspace_edit_tx,
            workspace_edit_rx: Mutex::new(workspace_edit_rx),
            dropped_notifications: Arc::new(AtomicU64::new(0)),
            starting_servers: DashSet::new(),
            listener_handles: DashMap::new(),
            language_server_index: DashMap::new(),
            server_roots: DashMap::new(),
        }
    }

    /// Checks if diagnostics have changed and resets the flag
    pub fn diagnostics_changed(&self) -> bool {
        self.diagnostics_changed.swap(false, Ordering::SeqCst)
    }

    /// Gets the number of dropped notifications (when channel was full)
    /// BUG FIX: Added to track notification backpressure
    pub fn get_dropped_notification_count(&self) -> u64 {
        self.dropped_notifications.load(Ordering::Relaxed)
    }

    /// Gets current progress message (non-blocking)
    pub fn get_progress_message(&self) -> Option<String> {
        if let Ok(progress) = self.current_progress.try_lock() {
            if !progress.is_empty() {
                // Return the first progress message (usually only one active)
                progress.values().next().cloned()
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Starts a language server for the given language and root path.
    /// Returns the server_id used (may be `language` or `language@<hash>` if
    /// a server already exists for the same language with a different root).
    pub async fn start_server(
        &self,
        language: &str,
        command: &str,
        args: Vec<String>,
        root_path: &Path,
    ) -> Result<String> {
        lsp_debug!(
            "LspManager",
            "start_server called for language={} root={}",
            language,
            root_path.display()
        );

        // Check existing servers for this language — if any shares the same root, reuse it
        let existing_ids = self.servers_for_language(language);
        for sid in &existing_ids {
            if let Some(existing_root) = self.server_roots.get(sid.as_str()) {
                if existing_root.value() == root_path {
                    lsp_debug!(
                        "LspManager",
                        "Server {} already running for root {}",
                        sid,
                        root_path.display()
                    );
                    return Ok(sid.clone());
                }
            }
        }

        // Determine server_id: bare language for the first server, composite for subsequent roots
        let server_id = if existing_ids.is_empty() {
            language.to_string()
        } else {
            root_server_id(language, root_path)
        };

        // Already running with this exact id (e.g., race between two buffers with same root)
        if self.servers.contains_key(&server_id) {
            lsp_debug!("LspManager", "Server already running: {}", server_id);
            return Ok(server_id);
        }

        // Prevent concurrent duplicate starts
        if !self.starting_servers.insert(server_id.clone()) {
            lsp_debug!(
                "LspManager",
                "Server start already in progress for {}",
                server_id
            );
            return Ok(server_id);
        }

        let result = async {
            lsp_debug!("LspManager", "Spawning server: {} {:?}", command, args);
            let mut server = LanguageServer::spawn(language, command, args).await?;
            lsp_debug!("LspManager", "Server spawned successfully");

            let root_uri = uri_from_file_path(root_path)
                .ok_or_else(|| anyhow::anyhow!("Invalid root path"))?;
            lsp_debug!("LspManager", "Root URI: {}", root_uri.as_str());

            lsp_debug!("LspManager", "Calling initialize...");
            server.initialize(root_uri).await?;
            lsp_debug!("LspManager", "Initialize completed successfully");

            // Insert into servers map
            if let Some(mut existing) = self.servers.insert(server_id.clone(), server) {
                // The fresh process has no documents open — drop baselines
                // recorded for the replaced instance (OV-00326).
                self.server_texts.retain(|(sid, _), _| sid != &server_id);
                if let Err(e) = existing.shutdown().await {
                    lsp_warn!(
                        "LspManager",
                        "Failed to shut down redundant server for {}: {}",
                        server_id,
                        e
                    );
                }
            }

            // Track root path for this server
            self.server_roots
                .insert(server_id.clone(), root_path.to_path_buf());

            // Update reverse index (deduplicate for restart safety)
            let mut ids = self
                .language_server_index
                .entry(language.to_string())
                .or_default();
            if !ids.contains(&server_id) {
                ids.push(server_id.clone());
            }

            Ok(server_id.clone())
        }
        .await;

        self.starting_servers.remove(&server_id);
        result
    }

    /// Starts a companion language server with an explicit server_id
    /// The server_id should be built with `companion_server_id(language_id, companion_id)`
    pub async fn start_companion_server(
        &self,
        server_id: &str,
        command: &str,
        args: Vec<String>,
        root_path: &Path,
    ) -> Result<()> {
        lsp_debug!(
            "LspManager",
            "start_companion_server called for server_id={}",
            server_id
        );
        // Check if already running
        if self.servers.contains_key(server_id) {
            lsp_debug!(
                "LspManager",
                "Companion server already running for {}",
                server_id
            );
            return Ok(());
        }

        // Prevent concurrent duplicate starts
        if !self.starting_servers.insert(server_id.to_string()) {
            lsp_debug!(
                "LspManager",
                "Companion server start already in progress for {}",
                server_id
            );
            return Ok(());
        }

        let result = async {
            lsp_debug!(
                "LspManager",
                "Spawning companion server: {} {:?}",
                command,
                args
            );
            // Extract language part for the server's language field
            let language = server_id.split(':').next().unwrap_or(server_id);
            let mut server = LanguageServer::spawn(language, command, args).await?;

            let root_uri = uri_from_file_path(root_path)
                .ok_or_else(|| anyhow::anyhow!("Invalid root path"))?;

            server.initialize(root_uri).await?;
            lsp_debug!("LspManager", "Companion server {} initialized", server_id);

            if let Some(mut existing) = self.servers.insert(server_id.to_string(), server) {
                // Fresh process, no documents open — drop stale baselines
                // recorded for the replaced instance (OV-00326).
                self.server_texts.retain(|(sid, _), _| sid != server_id);
                if let Err(e) = existing.shutdown().await {
                    lsp_warn!(
                        "LspManager",
                        "Failed to shut down redundant companion server for {}: {}",
                        server_id,
                        e
                    );
                }
            }

            // Track root path for this companion server
            self.server_roots
                .insert(server_id.to_string(), root_path.to_path_buf());

            // Update reverse index (deduplicate for restart safety)
            let mut ids = self
                .language_server_index
                .entry(language.to_string())
                .or_default();
            if !ids.contains(&server_id.to_string()) {
                ids.push(server_id.to_string());
            }

            Ok(())
        }
        .await;

        self.starting_servers.remove(server_id);
        result
    }

    /// Returns all server_ids that serve the given language_id.
    /// This includes the primary server (key == language_id) and any
    /// companion servers (key starts with "language_id:").
    /// O(1) lookup via reverse index maintained by start/stop.
    pub fn servers_for_language(&self, language_id: &str) -> Vec<String> {
        self.language_server_index
            .get(language_id)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Returns the server_ids that should handle a specific document.
    ///
    /// When multiple roots exist for the same language, choose the deepest
    /// matching root and include all servers (primary + companions) registered
    /// for that exact root.
    pub fn servers_for_document(&self, language_id: &str, file_path: &Path) -> Vec<String> {
        let server_ids = self.servers_for_language(language_id);
        if server_ids.is_empty() {
            return Vec::new();
        }

        let mut best_root: Option<(usize, PathBuf)> = None;
        let mut rootless_ids = Vec::new();

        for sid in &server_ids {
            match self.server_roots.get(sid.as_str()) {
                Some(root) if file_path.starts_with(root.value()) => {
                    let depth = root.value().components().count();
                    let root_path = root.value().clone();
                    if best_root
                        .as_ref()
                        .is_none_or(|(best_depth, _)| depth > *best_depth)
                    {
                        best_root = Some((depth, root_path));
                    }
                }
                Some(_) => {}
                None => rootless_ids.push(sid.clone()),
            }
        }

        if let Some((_, selected_root)) = best_root {
            return server_ids
                .into_iter()
                .filter(|sid| {
                    self.server_roots
                        .get(sid.as_str())
                        .is_some_and(|root| root.value() == &selected_root)
                })
                .collect();
        }

        if !rootless_ids.is_empty() {
            return rootless_ids;
        }

        Vec::new()
    }

    /// Convenience wrapper for URI-based document routing.
    pub fn servers_for_document_uri(&self, language_id: &str, uri: &Uri) -> Vec<String> {
        uri_to_file_path(uri)
            .map(|path| self.servers_for_document(language_id, &path))
            .unwrap_or_default()
    }

    pub async fn completion_trigger_characters_for_servers(
        &self,
        server_ids: &[String],
    ) -> Vec<char> {
        use std::collections::HashSet;
        let mut set: HashSet<char> = HashSet::new();
        for sid in server_ids {
            if let Some(server) = self.servers.get(sid.as_str()).map(|e| e.value().clone()) {
                for ch in server.completion_trigger_characters().await {
                    set.insert(ch);
                }
            }
        }
        set.into_iter().collect()
    }

    /// Stops a language server
    pub async fn stop_server(&self, language: &str) -> Result<()> {
        // Abort notification listener for this server
        if let Some((_, handle)) = self.listener_handles.remove(language) {
            handle.abort();
        }

        if let Some((_, mut server)) = self.servers.remove(language) {
            server.shutdown().await?;
        }

        // The server process is gone: its recorded document baselines are
        // meaningless. A replacement server gets fresh didOpen full text and
        // re-records them (OV-00326).
        self.server_texts
            .retain(|(server_id, _), _| server_id != language);

        // Clean up root tracking
        self.server_roots.remove(language);

        // Update reverse index: remove this server_id from its language entry
        // For root-scoped servers like "typescript@abcd1234", extract base language
        let language_id = language.split([':', '@']).next().unwrap_or(language);
        if let Some(mut entry) = self.language_server_index.get_mut(language_id) {
            entry.retain(|s| s != language);
            if entry.is_empty() {
                drop(entry);
                self.language_server_index.remove(language_id);
            }
        }

        // Clean up diagnostics from this server
        {
            let mut diags = self.diagnostics.lock().await;
            for server_map in diags.values_mut() {
                server_map.remove(language);
            }
        }
        // Invalidate entire merge cache since we removed a server's diagnostics
        {
            let mut cache = self.merged_diagnostics_cache.lock().await;
            cache.clear();
        }
        // Signal diagnostics changed so UI refreshes
        self.diagnostics_changed.store(true, Ordering::SeqCst);

        // Clear document version tracking so the restarted server gets fresh
        // didOpen notifications. Without this, did_open() sees stale entries
        // in document_versions and skips sending didOpen to the new server.
        {
            let mut versions = self.document_versions.lock().await;
            versions.clear();
        }
        {
            let mut sent = self.last_sent_versions.lock().await;
            sent.clear();
        }
        {
            let mut edits = self.last_local_edit.lock().await;
            edits.clear();
        }

        Ok(())
    }

    /// Merges diagnostics from all servers for a URI, deduplicating by range+message
    fn merge_diagnostics(server_map: &HashMap<String, StoredDiagnostics>) -> Vec<Diagnostic> {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for stored in server_map.values() {
            for diag in &stored.diagnostics {
                // Deduplicate by (range, message) — different servers may report the same issue
                let key = (
                    diag.range.start.line,
                    diag.range.start.character,
                    diag.range.end.line,
                    diag.range.end.character,
                    diag.message.clone(),
                );
                if seen.insert(key) {
                    merged.push(diag.clone());
                }
            }
        }
        merged
    }

    /// Gets diagnostics for a file (merged from all servers, cached)
    pub async fn get_diagnostics(&self, uri: &Uri) -> Vec<Diagnostic> {
        // Check cache first (OV-00151)
        {
            let cache = self.merged_diagnostics_cache.lock().await;
            if let Some(cached) = cache.get(uri) {
                return cached.clone();
            }
        }

        // Cache miss: merge and store
        let merged = {
            let diagnostics = self.diagnostics.lock().await;
            let result = diagnostics
                .get(uri)
                .map(Self::merge_diagnostics)
                .unwrap_or_default();
            crate::lsp_debug!(
                "DIAGNOSTICS",
                "get_diagnostics: uri={} found={} stored_uris={:?}",
                uri.as_str(),
                result.len(),
                diagnostics.keys().map(|u| u.as_str()).collect::<Vec<_>>()
            );
            result
        };

        {
            let mut cache = self.merged_diagnostics_cache.lock().await;
            cache.insert(uri.clone(), merged.clone());
        }

        merged
    }

    /// Gets diagnostics for a specific line in a file (merged from all servers, cached)
    pub async fn get_diagnostics_for_line(&self, uri: &Uri, line: u32) -> Vec<Diagnostic> {
        self.get_diagnostics(uri)
            .await
            .into_iter()
            .filter(|d| d.range.start.line <= line && d.range.end.line >= line)
            .collect()
    }

    /// Counts diagnostics by severity (merged from all servers, cached)
    pub async fn count_diagnostics(&self, uri: &Uri) -> (usize, usize, usize, usize) {
        let merged = self.get_diagnostics(uri).await;
        let mut errors = 0;
        let mut warnings = 0;
        let mut info = 0;
        let mut hints = 0;

        for diag in &merged {
            match diag.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => errors += 1,
                Some(lsp_types::DiagnosticSeverity::WARNING) => warnings += 1,
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => info += 1,
                Some(lsp_types::DiagnosticSeverity::HINT) => hints += 1,
                None => warnings += 1, // Default to warning if no severity
                _ => {}
            }
        }

        (errors, warnings, info, hints)
    }

    /// Gets merged diagnostics and the LSP document versions they were
    /// published for across all tracked URIs. An empty version list means all
    /// contributing servers published unversioned diagnostics.
    pub async fn list_all_diagnostics(&self) -> Vec<(Uri, Vec<Diagnostic>, Vec<i32>)> {
        let diagnostics = self.diagnostics.lock().await;
        let mut out = Vec::new();
        for (uri, server_map) in diagnostics.iter() {
            let merged = Self::merge_diagnostics(server_map);
            if !merged.is_empty() {
                let mut versions = server_map
                    .values()
                    .filter_map(|stored| stored.version)
                    .collect::<Vec<_>>();
                versions.sort_unstable();
                versions.dedup();
                out.push((uri.clone(), merged, versions));
            }
        }
        out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        out
    }

    /// Sets diagnostics for a file from a specific server
    /// (called when receiving publishDiagnostics)
    pub async fn set_diagnostics(
        &self,
        uri: Uri,
        server_id: &str,
        diagnostics: Vec<Diagnostic>,
        version: Option<i32>,
    ) {
        crate::lsp_debug!(
            "DIAGNOSTICS",
            "set_diagnostics: uri={} server={} count={} version={:?}",
            uri.as_str(),
            server_id,
            diagnostics.len(),
            version
        );
        crate::metrics::LSP_DIAGNOSTICS_TOTAL.inc();

        // Reject stale diagnostics — two cases:
        //
        // (a) Server sent a version: drop if version < document_versions[uri].
        //     Since did_change() now bumps document_versions *immediately*,
        //     this catches diagnostics arriving during the debounce window.
        //
        // (b) Server omitted version (None): drop if we have unsent edits
        //     (last_sent_versions[uri] < document_versions[uri]).  The server
        //     can only have seen up to last_sent, so its diagnostics cannot
        //     reflect pending content.  (OV-00162)
        {
            let versions = self.document_versions.lock().await;
            if let Some(&current_version) = versions.get(&uri) {
                if let Some(diag_version) = version {
                    if diag_version < current_version {
                        crate::lsp_debug!(
                            "DIAGNOSTICS",
                            "Dropping stale diagnostics: server={} diag_version={} current_doc_version={}",
                            server_id,
                            diag_version,
                            current_version
                        );
                        return;
                    }
                } else {
                    // No version from server — check if we have unsent edits
                    let sent = self.last_sent_versions.lock().await;
                    let last_sent = sent.get(&uri).copied().unwrap_or(0);
                    if last_sent < current_version {
                        crate::lsp_debug!(
                            "DIAGNOSTICS",
                            "Dropping unversioned diagnostics (unsent edits): server={} last_sent={} current={}",
                            server_id,
                            last_sent,
                            current_version
                        );
                        return;
                    }

                    // Unversioned diagnostics arriving too soon after local edits can
                    // still be for older content (server race with newer didChange).
                    // Keep the latest publication and apply it after the settle window;
                    // discarding it can leave old diagnostics cached forever when this
                    // publication is the server's only clearing update.
                    let last_edit = self.last_local_edit.lock().await.get(&uri).copied();
                    if let Some(edit_time) = last_edit {
                        if edit_time.elapsed()
                            < Duration::from_millis(UNVERSIONED_DIAGNOSTICS_SETTLE_MS)
                        {
                            let apply_after = edit_time
                                + Duration::from_millis(UNVERSIONED_DIAGNOSTICS_SETTLE_MS);
                            self.deferred_diagnostics.lock().await.insert(
                                (uri.clone(), server_id.to_string()),
                                DeferredDiagnostics {
                                    diagnostics,
                                    document_version: current_version,
                                    last_edit: edit_time,
                                    apply_after,
                                },
                            );
                            crate::lsp_debug!(
                                "DIAGNOSTICS",
                                "Deferring unversioned diagnostics (recent local edit): server={} elapsed_ms={} uri={}",
                                server_id,
                                edit_time.elapsed().as_millis(),
                                uri.as_str()
                            );
                            return;
                        }
                    }
                }
            }
        }

        self.deferred_diagnostics
            .lock()
            .await
            .remove(&(uri.clone(), server_id.to_string()));

        let mut diags = self.diagnostics.lock().await;
        let uri_for_cache = uri.clone();
        let entry = diags.entry(uri).or_default();

        if let Some(diag_version) = version {
            if let Some(existing) = entry.get(server_id).and_then(|s| s.version) {
                if diag_version < existing {
                    crate::lsp_debug!(
                        "DIAGNOSTICS",
                        "Ignoring out-of-order diagnostics: server={} diag_version={} existing_version={}",
                        server_id,
                        diag_version,
                        existing
                    );
                    return;
                }
            }
        }

        entry.insert(
            server_id.to_string(),
            StoredDiagnostics {
                version,
                diagnostics,
            },
        );
        drop(diags); // Release diagnostics lock before acquiring cache lock

        // Invalidate merged cache for this URI (OV-00151)
        {
            let mut cache = self.merged_diagnostics_cache.lock().await;
            cache.remove(&uri_for_cache);
        }
        self.diagnostics_changed.store(true, Ordering::SeqCst);
    }

    /// Apply unversioned diagnostics held during the post-edit settle window.
    ///
    /// The editor calls this once per tick. A deferred publication is discarded
    /// only when a newer local edit superseded the document state it describes.
    pub async fn apply_deferred_diagnostics(&self) {
        let now = Instant::now();
        let ready = {
            let mut pending = self.deferred_diagnostics.lock().await;
            let ready_keys: Vec<_> = pending
                .iter()
                .filter(|(_, value)| value.apply_after <= now)
                .map(|(key, _)| key.clone())
                .collect();

            ready_keys
                .into_iter()
                .filter_map(|key| pending.remove(&key).map(|value| (key, value)))
                .collect::<Vec<_>>()
        };

        for ((uri, server_id), deferred) in ready {
            let current_version = self
                .document_versions
                .lock()
                .await
                .get(&uri)
                .copied()
                .unwrap_or(0);
            let last_edit = self.last_local_edit.lock().await.get(&uri).copied();

            if current_version != deferred.document_version || last_edit != Some(deferred.last_edit)
            {
                continue;
            }

            self.set_diagnostics(uri, &server_id, deferred.diagnostics, None)
                .await;
        }
    }

    /// Gets health information for all language servers
    pub async fn health_check(&self) -> Vec<LanguageServerHealth> {
        // Collect servers while holding lock (minimal duration)
        // to avoid holding DashMap lock during async health_check() calls
        let servers: Vec<_> = self.servers.iter().map(|r| r.value().clone()).collect();

        // Lock is released after collection; now iterate without contention
        let mut health_infos = Vec::new();
        for server in servers {
            health_infos.push(server.health_check().await);
        }

        health_infos
    }

    /// Get list of active server language IDs (sync, for command execution)
    pub fn active_server_languages(&self) -> Vec<String> {
        self.servers.iter().map(|r| r.key().clone()).collect()
    }

    /// Get command for a language server (sync)
    pub fn server_command(&self, language: &str) -> Option<String> {
        self.servers.get(language).map(|s| s.command().to_string())
    }

    /// Gets the current version of a document
    pub async fn get_document_version(&self, uri: &Uri) -> i32 {
        let versions = self.document_versions.lock().await;
        versions.get(uri).copied().unwrap_or(0)
    }

    /// Gets the last version that was actually sent to the LSP server via didChange.
    /// Returns 0 if no version has been sent yet.
    pub async fn get_last_sent_version(&self, uri: &Uri) -> i32 {
        let sent = self.last_sent_versions.lock().await;
        sent.get(uri).copied().unwrap_or(0)
    }

    /// Increments the version of a document
    pub async fn increment_document_version(&self, uri: &Uri) -> i32 {
        let mut versions = self.document_versions.lock().await;
        let version = versions.entry(uri.clone()).or_insert(0);
        *version += 1;
        *version
    }

    /// Gets a reference to a language server
    pub async fn get_server(&self, language: &str) -> Option<LanguageServer> {
        self.servers
            .get(language)
            .map(|entry| entry.value().clone())
    }

    /// Gets the root path for a server (for debugging/introspection)
    pub fn server_root(&self, server_id: &str) -> Option<std::path::PathBuf> {
        self.server_roots.get(server_id).map(|r| r.clone())
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_diagnostics_storage() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        // Initially no diagnostics
        assert!(manager.get_diagnostics(&uri).await.is_empty());

        // Set diagnostics
        let diags = vec![]; // Empty for now
        manager
            .set_diagnostics(uri.clone(), "rust", diags, Some(1))
            .await;

        // Verify stored
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 0);
    }

    #[tokio::test]
    async fn list_all_diagnostics_preserves_analysis_versions() {
        let manager = LspManager::new();
        let uri: Uri = "file:///versioned.rs".parse().unwrap();
        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], Some(3))
            .await;

        let all = manager.list_all_diagnostics().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, uri);
        assert_eq!(all[0].1.len(), 1);
        assert_eq!(all[0].2, vec![3]);
    }

    #[tokio::test]
    async fn test_document_versioning() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        // Initial version is 0
        assert_eq!(manager.get_document_version(&uri).await, 0);

        // Increment version
        let v1 = manager.increment_document_version(&uri).await;
        assert_eq!(v1, 1);

        let v2 = manager.increment_document_version(&uri).await;
        assert_eq!(v2, 2);

        assert_eq!(manager.get_document_version(&uri).await, 2);
    }

    #[tokio::test]
    async fn test_diagnostics_version_filtering() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        // First publish is accepted (even if it may be behind the editor's current buffer).
        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], Some(2))
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);

        // Newer publish is accepted.
        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], Some(3))
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);

        // Out-of-order older publish should not override newer stored one.
        manager
            .set_diagnostics(uri.clone(), "rust", vec![], Some(2))
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);
    }

    #[tokio::test]
    async fn test_unversioned_diagnostics_dropped_just_after_local_edit() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        {
            let mut versions = manager.document_versions.lock().await;
            versions.insert(uri.clone(), 2);
        }
        {
            let mut sent = manager.last_sent_versions.lock().await;
            sent.insert(uri.clone(), 2);
        }
        {
            let mut local_edit = manager.last_local_edit.lock().await;
            local_edit.insert(uri.clone(), std::time::Instant::now());
        }

        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], None)
            .await;

        assert_eq!(manager.get_diagnostics(&uri).await.len(), 0);
    }

    #[tokio::test]
    async fn test_unversioned_diagnostics_accepted_after_settle() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        {
            let mut versions = manager.document_versions.lock().await;
            versions.insert(uri.clone(), 2);
        }
        {
            let mut sent = manager.last_sent_versions.lock().await;
            sent.insert(uri.clone(), 2);
        }
        {
            let mut local_edit = manager.last_local_edit.lock().await;
            let settle = std::time::Duration::from_millis(UNVERSIONED_DIAGNOSTICS_SETTLE_MS + 1);
            let stable_time = std::time::Instant::now()
                .checked_sub(settle)
                .expect("monotonic clock supports checked_sub");
            local_edit.insert(uri.clone(), stable_time);
        }

        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], None)
            .await;

        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);
    }

    #[tokio::test]
    async fn deferred_empty_diagnostics_clear_previous_errors() {
        let manager = LspManager::new();
        let uri: Uri = "file:///stale.rs".parse().unwrap();

        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], None)
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);

        manager
            .document_versions
            .lock()
            .await
            .insert(uri.clone(), 2);
        manager
            .last_sent_versions
            .lock()
            .await
            .insert(uri.clone(), 2);
        manager
            .last_local_edit
            .lock()
            .await
            .insert(uri.clone(), Instant::now());

        manager
            .set_diagnostics(uri.clone(), "rust", Vec::new(), None)
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);

        tokio::time::sleep(Duration::from_millis(
            UNVERSIONED_DIAGNOSTICS_SETTLE_MS + 10,
        ))
        .await;
        manager.apply_deferred_diagnostics().await;

        assert!(manager.get_diagnostics(&uri).await.is_empty());
    }

    #[tokio::test]
    async fn test_unversioned_diagnostics_accepted_after_local_changes_flushed() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        {
            let mut versions = manager.document_versions.lock().await;
            versions.insert(uri.clone(), 2);
        }
        {
            let mut sent = manager.last_sent_versions.lock().await;
            sent.insert(uri.clone(), 1);
        }
        {
            let mut local_edit = manager.last_local_edit.lock().await;
            let stable_time = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(2))
                .expect("monotonic clock supports checked_sub");
            local_edit.insert(uri.clone(), stable_time);
        }

        // Still drops before the latest version is marked as sent.
        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], None)
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 0);

        {
            let mut sent = manager.last_sent_versions.lock().await;
            sent.insert(uri.clone(), 2);
        }

        // Now that the document state is up to date, unversioned diagnostics should apply.
        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], None)
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);
    }

    #[tokio::test]
    async fn test_versioned_diagnostics_stale_when_before_current_document_version() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        {
            let mut versions = manager.document_versions.lock().await;
            versions.insert(uri.clone(), 4);
        }
        {
            let mut sent = manager.last_sent_versions.lock().await;
            sent.insert(uri.clone(), 4);
        }

        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], Some(2))
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 0);

        manager
            .set_diagnostics(uri.clone(), "rust", vec![Diagnostic::default()], Some(4))
            .await;
        assert_eq!(manager.get_diagnostics(&uri).await.len(), 1);
    }

    #[tokio::test]
    async fn test_last_local_edit_cleanup_on_did_close_broadcast() {
        let manager = LspManager::new();
        let uri: Uri = "file:///test.rs".parse().unwrap();

        manager
            .last_local_edit
            .lock()
            .await
            .insert(uri.clone(), std::time::Instant::now());

        let _ = manager.did_close_broadcast(uri.clone(), "rust").await;

        assert!(manager.last_local_edit.lock().await.get(&uri).is_none());
        assert!(manager.get_diagnostics(&uri).await.is_empty());
    }

    #[test]
    fn test_servers_for_document_prefers_deepest_matching_root_and_keeps_companions() {
        let manager = LspManager::new();
        let repo_root = PathBuf::from("/workspace");
        let nested_root = repo_root.join("nested");
        let repo_id = root_server_id("java", &repo_root);
        let nested_id = root_server_id("java", &nested_root);
        let nested_companion = companion_server_id("java", "formatter");

        manager.language_server_index.insert(
            "java".to_string(),
            vec![repo_id.clone(), nested_id.clone(), nested_companion.clone()],
        );
        manager.server_roots.insert(repo_id, repo_root);
        manager
            .server_roots
            .insert(nested_id.clone(), nested_root.clone());
        manager
            .server_roots
            .insert(nested_companion.clone(), nested_root);

        let routed =
            manager.servers_for_document("java", Path::new("/workspace/nested/src/Test.java"));

        assert_eq!(routed, vec![nested_id, nested_companion]);
    }

    #[test]
    fn test_servers_for_document_returns_empty_when_no_root_matches() {
        let manager = LspManager::new();
        let root = PathBuf::from("/workspace/project");
        let server_id = root_server_id("java", &root);

        manager
            .language_server_index
            .insert("java".to_string(), vec![server_id.clone()]);
        manager.server_roots.insert(server_id, root);

        assert!(manager
            .servers_for_document("java", Path::new("/other/project/Test.java"))
            .is_empty());
    }
}
