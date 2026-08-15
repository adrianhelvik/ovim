//! Language server process management
//!
//! Manages the lifecycle of a single language server process, including:
//! - Process spawning and monitoring
//! - stdio communication
//! - Request/response matching
//! - Initialization handshake

use super::protocol::{write_message, JsonRpcMessage, RequestId};
use super::supervisor::{RestartPolicy, TaskHealth, TaskSupervisor};
use anyhow::{anyhow, Context, Result};
use lsp_types::{
    InitializeParams, InitializeResult, InitializedParams, ServerCapabilities, Uri, WorkspaceFolder,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Maximum LSP message size in bytes (50MB)
/// Must match MAX_MESSAGE_SIZE in mod.rs
const MAX_MESSAGE_SIZE: usize = 50 * 1024 * 1024;

bitflags::bitflags! {
    /// Cached LSP server capability flags, stored as a single atomic u32.
    ///
    /// Set once during initialization (from `ServerCapabilities`) or
    /// updated via dynamic registration (`client/registerCapability`).
    /// Queried lock-free via the `supports_*()` accessors.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct LspCapFlags: u32 {
        const GOTO_DEFINITION    = 1 << 0;
        const GOTO_DECLARATION   = 1 << 1;
        const GOTO_IMPLEMENTATION = 1 << 2;
        const GOTO_TYPE_DEFINITION = 1 << 3;
        const HOVER              = 1 << 4;
        const COMPLETION         = 1 << 5;
        const FORMATTING         = 1 << 6;
        const RANGE_FORMATTING   = 1 << 7;
        const CODE_ACTIONS       = 1 << 8;
        const REFERENCES         = 1 << 9;
        const RENAME             = 1 << 10;
        const PREPARE_RENAME     = 1 << 11;
        const SIGNATURE_HELP     = 1 << 12;
        const DOCUMENT_SYMBOL    = 1 << 13;
        const SELECTION_RANGE    = 1 << 14;
        const WORKSPACE_SYMBOL   = 1 << 15;
        const DOCUMENT_HIGHLIGHT = 1 << 16;
        const INCREMENTAL_SYNC   = 1 << 17;
        const FOLDING_RANGE      = 1 << 18;
        const CALL_HIERARCHY     = 1 << 19;
        const TYPE_HIERARCHY     = 1 << 20;
        const EXECUTE_COMMAND    = 1 << 21;
        const INLAY_HINT         = 1 << 22;
        const SEMANTIC_TOKENS    = 1 << 23;
    }
}

pub(crate) fn workspace_settings_for_language(language: &str) -> Option<serde_json::Value> {
    match language {
        // Ovim exposes these host tables to config/plugins: `vim` is the
        // Neovim-compatible surface, `ovim` is the editor's own API.
        // Keep the declaration narrow so LuaLS still catches misspelled globals.
        "lua" => Some(json!({
            "Lua": {
                "runtime": { "version": "Lua 5.4" },
                "diagnostics": { "globals": ["vim", "ovim"] }
            }
        })),
        _ => None,
    }
}

/// Maximum number of pending requests to prevent OOM
const MAX_PENDING_REQUESTS: usize = 1000;

/// Maximum time a request can remain pending before cleanup (10 minutes)
/// This prevents stale requests from accumulating when LSP servers hang
/// Initialize requests need much longer (up to 5 minutes for Java)
const REQUEST_STALE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Interval for cleanup task (10 seconds)
/// More frequent cleanup prevents memory buildup from stale requests
const CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Pending request metadata
struct PendingRequest {
    sender: oneshot::Sender<Result<Value>>,
    sent_at: Instant,
    method: String,
}

/// Server state for explicit state machine
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ServerState {
    /// Server process is spawning
    Spawning,

    /// Server is initializing (sent initialize request, waiting for response)
    Initializing { started_at: Instant },

    /// Server is ready to accept requests
    Ready {
        initialized_at: Instant,
        capabilities: ServerCapabilities,
    },

    /// Server has failed and cannot recover
    Failed { error: String, at: Instant },

    /// Server is shutting down
    ShuttingDown,

    /// Server process has terminated
    Terminated,
}

/// Health information for a language server
#[derive(Debug, Clone)]
pub struct LanguageServerHealth {
    /// Language identifier (e.g., "rust", "python")
    pub language: String,

    /// Command used to spawn the server
    pub command: String,

    /// Current server state
    pub state: String,

    /// Time since server was spawned
    pub uptime: Duration,

    /// Number of pending requests
    pub pending_requests: usize,

    /// Whether the server has capabilities
    pub has_capabilities: bool,

    /// Health of supervised background tasks
    pub tasks: Vec<TaskHealth>,

    /// Whether the server process is still alive
    pub is_alive: bool,
}

/// A language server process
#[derive(Clone)]
pub struct LanguageServer {
    inner: Arc<LanguageServerInner>,
}

struct LanguageServerInner {
    /// Language identifier (e.g., "rust", "python") for logging context
    language: String,

    /// Command used to spawn the server (e.g., "rust-analyzer") for logging
    command: String,

    /// Child process handle
    process: Mutex<Option<Child>>,

    /// Current server state (explicit state machine)
    state: Arc<Mutex<ServerState>>,

    /// Server capabilities after initialization (kept for backwards compat)
    capabilities: Mutex<Option<ServerCapabilities>>,

    /// Pending requests awaiting responses with metadata for cleanup
    pending_requests: Mutex<HashMap<RequestId, PendingRequest>>,

    /// Next request ID
    next_request_id: AtomicU64,

    /// Channel to send outgoing messages
    outgoing_tx: mpsc::Sender<JsonRpcMessage>,

    /// Channel to receive incoming messages
    incoming_rx: Mutex<Option<mpsc::Receiver<JsonRpcMessage>>>,

    /// Task supervisor for managing background tasks
    supervisor: TaskSupervisor,

    /// Cached capability flags — lock-free, set once during initialization.
    /// Use `LspCapFlags` bitflags for the individual capabilities.
    cap_flags: AtomicU32,
}

impl LanguageServerInner {
    /// Returns a log prefix with language and command context
    fn log_prefix(&self) -> String {
        format!("[LSP:{}:{}]", self.language, self.command)
    }

    /// Returns the current capability flags.
    fn cap_flags(&self) -> LspCapFlags {
        LspCapFlags::from_bits_truncate(self.cap_flags.load(Ordering::Relaxed))
    }

    /// Checks whether a specific capability flag is set.
    fn has_cap(&self, flag: LspCapFlags) -> bool {
        self.cap_flags().contains(flag)
    }

    /// Atomically sets or clears a single capability flag.
    fn set_cap(&self, flag: LspCapFlags, enabled: bool) {
        if enabled {
            self.cap_flags.fetch_or(flag.bits(), Ordering::Relaxed);
        } else {
            self.cap_flags.fetch_and(!flag.bits(), Ordering::Relaxed);
        }
    }
}

impl LanguageServer {
    pub(crate) fn language(&self) -> &str {
        &self.inner.language
    }

    /// Returns a log prefix with language and command context
    fn log_prefix(&self) -> String {
        self.inner.log_prefix()
    }

    /// Spawns a new language server process
    pub async fn spawn(language: &str, command: &str, args: Vec<String>) -> Result<Self> {
        crate::lsp_debug!(
            "Server",
            "Spawning {} with command: {} args: {:?}",
            language,
            command,
            args
        );
        let mut child = Command::new(command)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()) // Capture stderr for debugging
            .spawn()
            .context(format!("Failed to spawn language server: {}", command))?;
        crate::lsp_debug!("Server", "Process spawned, PID: {:?}", child.id());

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to open stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to open stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to open stderr"))?;

        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<JsonRpcMessage>(100);
        let (incoming_tx, incoming_rx) = mpsc::channel::<JsonRpcMessage>(100);

        // Wrap stdin in Arc so writer task can clone it
        let stdin = Arc::new(Mutex::new(stdin));

        // Create supervisor with auto-restart on failure
        let supervisor = TaskSupervisor::new(RestartPolicy::OnFailure {
            max_retries: 3,
            initial_backoff: Duration::from_secs(1),
        });

        let inner = Arc::new(LanguageServerInner {
            language: language.to_string(),
            command: command.to_string(),
            process: Mutex::new(Some(child)),
            state: Arc::new(Mutex::new(ServerState::Spawning)),
            capabilities: Mutex::new(None),
            pending_requests: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            outgoing_tx,
            incoming_rx: Mutex::new(Some(incoming_rx)),
            supervisor,
            cap_flags: AtomicU32::new(0),
        });

        let server = Self {
            inner: inner.clone(),
        };

        // Spawn writer task to write messages to stdin
        // Note: This task is NOT supervised because:
        // 1. It owns the unique receiver for the outgoing channel
        // 2. If this task fails, the entire LSP communication is broken
        // 3. Restarting would require unsafe code or complex channel recreation
        // 4. Better to let the server process fail and restart cleanly
        let stdin_clone = stdin.clone();
        let writer_state = inner.state.clone();
        let writer_lang = inner.language.clone();
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                let mut stdin_guard = stdin_clone.lock().await;
                if let Err(e) = write_message(&mut *stdin_guard, &msg).await {
                    crate::lsp_error!(
                        "Writer",
                        "[{}] Error writing to language server: {}",
                        writer_lang,
                        e
                    );
                    let mut state = writer_state.lock().await;
                    *state = ServerState::Failed {
                        error: format!("Writer failed: {}", e),
                        at: Instant::now(),
                    };
                    break;
                }
            }
        });

        // Spawn supervised stale request cleanup task
        let inner_cleanup = inner.clone();
        let state_clone_cleanup = inner.state.clone();
        inner
            .supervisor
            .spawn_supervised("lsp_cleanup".to_string(), move || {
                let inner = inner_cleanup.clone();
                let state_ref = state_clone_cleanup.clone();
                async move {
                    loop {
                        tokio::time::sleep(CLEANUP_INTERVAL).await;

                        // BUG FIX #3: Check server state first - fail all requests if server is dead
                        let state = state_ref.lock().await;
                        let is_failed = matches!(*state, ServerState::Failed { .. });
                        drop(state);

                        let mut pending = inner.pending_requests.lock().await;
                        let now = Instant::now();
                        let count_before = pending.len();

                        if is_failed && count_before > 0 {
                            // Server failed - immediately fail ALL pending requests
                            crate::lsp_warn!(
                                "Cleanup",
                                "Server failed - clearing all {} pending requests",
                                count_before
                            );
                            for (id, req) in pending.drain() {
                                crate::lsp_warn!(
                                    "Cleanup",
                                    "Failing request {:?} for method '{}' due to server failure",
                                    id,
                                    req.method
                                );
                                let _ = req.sender.send(Err(anyhow!(
                                    "Request '{}' failed because LSP server is in Failed state",
                                    req.method
                                )));
                            }
                        } else {
                            // Normal cleanup: remove stale requests
                            // Collect stale request IDs first
                            let stale_ids: Vec<RequestId> = pending
                                .iter()
                                .filter_map(|(id, req)| {
                                    let age = now.duration_since(req.sent_at);
                                    if age > REQUEST_STALE_TIMEOUT {
                                        Some(id.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();

                            // Remove and notify each stale request
                            for id in stale_ids {
                                if let Some(req) = pending.remove(&id) {
                                    let age = now.duration_since(req.sent_at);
                                    crate::lsp_warn!(
                                        "Cleanup",
                                        "Removing stale request {:?} for method '{}' (age: {:?})",
                                        id,
                                        req.method,
                                        age
                                    );
                                    let _ = req.sender.send(Err(anyhow!(
                                        "Request '{}' timed out and was cleaned up after {:?}",
                                        req.method,
                                        age
                                    )));
                                }
                            }
                        }

                        let count_after = pending.len();
                        if count_before > count_after {
                            crate::lsp_info!(
                                "Cleanup",
                                "Cleaned up {} stale requests ({} remaining)",
                                count_before - count_after,
                                count_after
                            );
                        }

                        // Warn at 80% of maximum capacity
                        let warning_threshold = (MAX_PENDING_REQUESTS as f64 * 0.8) as usize;
                        if pending.len() > warning_threshold {
                            crate::lsp_warn!(
                                "Cleanup",
                                "Warning: {} pending requests (max: {})",
                                pending.len(),
                                MAX_PENDING_REQUESTS
                            );
                        }
                    }
                }
            })
            .await?;

        // Spawn task to read messages from stdout
        // Note: Not supervised because stdout is unique to the process - if this fails, server is dead
        let inner_clone = server.inner.clone();
        let state_clone = inner.state.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                // Read headers (LSP spec allows multiple: Content-Length, Content-Type, etc.)
                // Headers end with an empty line (\r\n)
                let mut content_length: Option<usize> = None;
                let mut got_eof = false;
                let mut got_error = false;

                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header).await {
                        Err(e) => {
                            crate::lsp_error!(
                                &inner_clone.log_prefix(),
                                "CRITICAL: Reader task failed while reading header: {}",
                                e
                            );
                            let mut state = state_clone.lock().await;
                            *state = ServerState::Failed {
                                error: format!("Reader task failed: {}", e),
                                at: Instant::now(),
                            };
                            got_error = true;
                            break;
                        }
                        Ok(0) => {
                            // EOF
                            crate::lsp_error!(
                                &inner_clone.log_prefix(),
                                "Reader EOF: LSP server closed output (process likely exited)"
                            );
                            let mut state = state_clone.lock().await;
                            *state = ServerState::Failed {
                                error: "LSP server process exited".to_string(),
                                at: Instant::now(),
                            };
                            got_eof = true;
                            break;
                        }
                        Ok(_) => {}
                    }

                    let trimmed = header.trim();
                    if trimmed.is_empty() {
                        // Empty line = end of headers
                        break;
                    }

                    if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                        content_length = len_str.trim().parse().ok();
                    }
                    // Skip other headers (Content-Type, etc.)
                }

                if got_eof || got_error {
                    break;
                }

                let content_length = match content_length {
                    Some(len) => len,
                    None => {
                        // No Content-Length found in headers, skip
                        continue;
                    }
                };

                // Validate Content-Length before allocation
                if content_length > MAX_MESSAGE_SIZE {
                    crate::lsp_warn!(
                        &inner_clone.log_prefix(),
                        "Message too large: {} bytes (max {}MB), skipping",
                        content_length,
                        MAX_MESSAGE_SIZE / (1024 * 1024)
                    );
                    // Drain the oversized message body to keep framing in sync.
                    // Read in chunks to avoid allocating the full oversized buffer.
                    let mut remaining = content_length;
                    let mut drain_buf = vec![0u8; 64 * 1024]; // 64KB chunks
                    while remaining > 0 {
                        let to_read = remaining.min(drain_buf.len());
                        if tokio::io::AsyncReadExt::read_exact(
                            &mut reader,
                            &mut drain_buf[..to_read],
                        )
                        .await
                        .is_err()
                        {
                            got_error = true;
                            break;
                        }
                        remaining -= to_read;
                    }
                    if got_error {
                        break;
                    }
                    continue;
                }

                // Read content
                let mut content = vec![0u8; content_length];
                if let Err(_e) =
                    tokio::io::AsyncReadExt::read_exact(&mut reader, &mut content).await
                {
                    // Error reading message body, LSP server may have closed
                    break;
                }

                // Parse JSON
                match serde_json::from_slice::<JsonRpcMessage>(&content) {
                    Ok(msg) => {
                        if msg.is_response() {
                            // Handle response
                            if let Some(id) = msg.id {
                                // Extract the PendingRequest from the map without holding lock during send
                                let pending_req = {
                                    let mut pending = inner_clone.pending_requests.lock().await;
                                    pending.remove(&id)
                                }; // Lock released immediately

                                // Send response outside the lock scope to reduce contention
                                if let Some(req) = pending_req {
                                    // CRITICAL FIX: Propagate errors instead of just logging
                                    if let Some(error) = msg.error {
                                        // LSP Error Codes for expected/benign responses:
                                        // -32800: Request Cancelled (client or server cancelled)
                                        // -32801: Content Modified (document changed, results stale)
                                        const LSP_ERROR_REQUEST_CANCELLED: i32 = -32800;
                                        const LSP_ERROR_CONTENT_MODIFIED: i32 = -32801;

                                        if error.code == LSP_ERROR_REQUEST_CANCELLED
                                            || error.code == LSP_ERROR_CONTENT_MODIFIED
                                        {
                                            // Request was cancelled or invalidated - expected behavior
                                            // Log at debug level, not error level
                                            crate::lsp_debug!(
                                                &inner_clone.log_prefix(),
                                                "Request ID {:?} invalidated by server (code {}): {}",
                                                id,
                                                error.code,
                                                error.message
                                            );

                                            // Send cancellation error to caller
                                            // Caller can distinguish this from actual LSP errors
                                            let _ = req.sender.send(Err(anyhow!(
                                                "Request cancelled: {}",
                                                error.message
                                            )));
                                        } else {
                                            // Real LSP error - propagate to caller
                                            // Errors will be shown in status line by the editor
                                            let error_msg =
                                                format!("{} (code {})", error.message, error.code);
                                            // Removed eprintln - leaks into TUI display
                                            let _ = req
                                                .sender
                                                .send(Err(anyhow!("LSP error: {}", error_msg)));
                                        }
                                    } else if let Some(result) = msg.result {
                                        let _ = req.sender.send(Ok(result));
                                    } else {
                                        // Response with no result and no error - treat as null result
                                        // This can happen for valid responses like hover with no info
                                        let _ = req.sender.send(Ok(Value::Null));
                                    }
                                } else {
                                    // Response for unknown request — likely timed out and
                                    // was already cleaned up. Log so silent drops are visible.
                                    crate::lsp_warn!(
                                        &inner_clone.log_prefix(),
                                        "Response for unknown request ID {:?} (timed out or already handled)",
                                        id
                                    );
                                }
                            }
                        } else {
                            // Handle notification or request
                            if let Err(_e) = incoming_tx.send(msg).await {
                                // Channel closed, exit silently
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        // ERROR: Failed to parse LSP message - log for visibility
                        crate::lsp_error!(
                            &inner_clone.log_prefix(),
                            "Failed to parse LSP message (size: {} bytes): {}",
                            content.len(),
                            e
                        );
                        // Show a truncated preview of the malformed content for debugging
                        let lossy = String::from_utf8_lossy(&content);
                        let preview = if lossy.len() > 200 {
                            format!("{}...", crate::unicode::truncate_bytes(&lossy, 200))
                        } else {
                            lossy.into_owned()
                        };
                        crate::lsp_error!(
                            &inner_clone.log_prefix(),
                            "Malformed message preview: {}",
                            preview
                        );
                        // Continue processing other messages (don't break the loop)
                    }
                }
            }
            // Reader task exiting silently
        });

        // Spawn task to capture stderr and log it for debugging
        // Note: Not supervised because stderr is unique to the process
        tokio::spawn(async move {
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = stderr_reader.read_line(&mut line).await {
                if n == 0 {
                    break; // EOF
                }
                // Log stderr output from LSP server
                crate::lsp_debug!("stderr", "{}", line.trim_end());
                line.clear();
            }
            crate::lsp_debug!("stderr", "LSP stderr task exiting");
        });

        // BUG FIX: Verify the process is actually running before returning
        // Give it a small delay to fail fast if the command doesn't exist or crashes immediately
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check if process is still alive
        let process_guard = server.inner.process.lock().await;
        if let Some(ref child) = *process_guard {
            // try_wait returns Ok(Some(status)) if exited, Ok(None) if still running, Err on error
            match child.id() {
                Some(_pid) => {
                    // Process has a valid PID, likely running
                    drop(process_guard);
                }
                None => {
                    drop(process_guard);
                    // Process has no PID - it failed to start or already exited
                    return Err(anyhow!(
                        "Language server process failed to start or exited immediately: {} {:?}",
                        command,
                        args
                    ));
                }
            }
        } else {
            drop(process_guard);
            return Err(anyhow!("Language server process handle is missing"));
        }

        Ok(server)
    }

    /// Initializes the language server
    pub async fn initialize(&mut self, root_uri: Uri) -> Result<()> {
        // Use language-specific timeout (Java needs much longer due to indexing)
        // Java/jdtls: 5 minutes (300s) for large projects with many dependencies
        // Other languages: 2 minutes (120s) should be plenty
        let init_timeout = if self.inner.language == "java" {
            Duration::from_secs(300) // 5 minutes for Java
        } else {
            Duration::from_secs(120) // 2 minutes for other languages
        };

        match tokio::time::timeout(init_timeout, self.initialize_internal(root_uri)).await {
            Ok(result) => result,
            Err(_) => {
                // OV-00155: Timeout cancels initialize_internal mid-execution,
                // so its error handler never runs. Transition to Failed here to
                // trigger cleanup of orphaned pending requests.
                let msg = format!("LSP initialization timed out after {:?}", init_timeout);
                crate::lsp_error!(&self.log_prefix(), "{}", msg);
                self.transition_to(ServerState::Failed {
                    error: msg.clone(),
                    at: Instant::now(),
                })
                .await;
                Err(anyhow::anyhow!(
                    "{msg}. For large projects, this may take several minutes on first run."
                ))
            }
        }
    }

    /// Internal initialization implementation (wrapped by timeout)
    /// BUG FIX: Improved error handling - sets Failed state on errors
    async fn initialize_internal(&mut self, root_uri: Uri) -> Result<()> {
        // Transition to Initializing state
        self.transition_to(ServerState::Initializing {
            started_at: Instant::now(),
        })
        .await;

        // BUG FIX: Wrap initialization logic to catch errors and set Failed state
        let init_result = self.do_initialize(root_uri).await;

        if let Err(ref e) = init_result {
            // Initialization failed - set server to Failed state
            crate::lsp_error!(&self.log_prefix(), "Initialization failed: {}", e);
            self.transition_to(ServerState::Failed {
                error: format!("Initialization failed: {}", e),
                at: Instant::now(),
            })
            .await;
        }

        init_result
    }

    /// Performs the actual initialization protocol exchange
    async fn do_initialize(&mut self, root_uri: Uri) -> Result<()> {
        // Build comprehensive client capabilities to advertise supported features
        // This tells the LSP server what features the client can handle
        let client_capabilities = lsp_types::ClientCapabilities {
            // Window capabilities
            window: Some(lsp_types::WindowClientCapabilities {
                work_done_progress: Some(true),
                show_message: Some(lsp_types::ShowMessageRequestClientCapabilities {
                    message_action_item: Some(lsp_types::MessageActionItemCapabilities {
                        additional_properties_support: Some(false),
                    }),
                }),
                ..Default::default()
            }),

            // Text document capabilities - advertise support for common LSP features
            text_document: Some(lsp_types::TextDocumentClientCapabilities {
                completion: Some(Default::default()),
                hover: Some(hover_client_capabilities()),
                signature_help: Some(Default::default()),
                declaration: Some(Default::default()),
                definition: Some(Default::default()),
                references: Some(Default::default()),
                document_highlight: Some(Default::default()),
                document_symbol: Some(Default::default()),
                code_action: Some(lsp_types::CodeActionClientCapabilities {
                    dynamic_registration: Some(true),
                    code_action_literal_support: Some(lsp_types::CodeActionLiteralSupport {
                        code_action_kind: lsp_types::CodeActionKindLiteralSupport {
                            value_set: vec![
                                lsp_types::CodeActionKind::QUICKFIX.as_str().to_string(),
                                lsp_types::CodeActionKind::REFACTOR.as_str().to_string(),
                                lsp_types::CodeActionKind::REFACTOR_EXTRACT
                                    .as_str()
                                    .to_string(),
                                lsp_types::CodeActionKind::REFACTOR_INLINE
                                    .as_str()
                                    .to_string(),
                                lsp_types::CodeActionKind::REFACTOR_REWRITE
                                    .as_str()
                                    .to_string(),
                                lsp_types::CodeActionKind::SOURCE.as_str().to_string(),
                                lsp_types::CodeActionKind::SOURCE_ORGANIZE_IMPORTS
                                    .as_str()
                                    .to_string(),
                                lsp_types::CodeActionKind::SOURCE_FIX_ALL
                                    .as_str()
                                    .to_string(),
                            ],
                        },
                    }),
                    is_preferred_support: Some(true),
                    disabled_support: Some(true),
                    data_support: Some(true),
                    resolve_support: Some(lsp_types::CodeActionCapabilityResolveSupport {
                        properties: vec!["edit".to_string(), "command".to_string()],
                    }),
                    honors_change_annotations: Some(true),
                }),
                rename: Some(Default::default()),
                formatting: Some(Default::default()),
                range_formatting: Some(Default::default()),
                publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
                    related_information: Some(true),
                    tag_support: Some(lsp_types::TagSupport {
                        value_set: vec![
                            lsp_types::DiagnosticTag::UNNECESSARY,
                            lsp_types::DiagnosticTag::DEPRECATED,
                        ],
                    }),
                    version_support: Some(true),
                    ..Default::default()
                }),
                // LSP 3.17 servers that gate on this capability (same pattern
                // that bit publish_diagnostics for typescript-language-server)
                // won't emit hints unless we advertise it.
                inlay_hint: Some(lsp_types::InlayHintClientCapabilities {
                    dynamic_registration: Some(true),
                    resolve_support: None,
                }),
                ..Default::default()
            }),

            // Workspace capabilities
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                apply_edit: Some(true),
                configuration: Some(true),
                ..Default::default()
            }),

            ..Default::default()
        };

        #[allow(deprecated)]
        // Language-specific initialization options
        // Each language server has different requirements for optimal behavior
        let initialization_options = match self.inner.language.as_str() {
            "rust" => {
                // rust-analyzer specific configuration.
                //
                // `checkOnSave` is a plain boolean in current rust-analyzer;
                // the command moved to the `check` table. The old
                // `checkOnSave: { command, extraArgs }` shape is rejected with
                // "invalid config value" and silently falls back to defaults.
                // Deliberately no `-D warnings`: promoting every clippy
                // warning to an error made the editor disagree with plain
                // `cargo build`/`cargo test`, which users read as bogus
                // errors. (OV-00325)
                Some(json!({
                    "checkOnSave": true,
                    "check": {
                        "command": "clippy"
                    },
                    "hover": {
                        "documentation": true,
                        "relatedInformation": true
                    },
                    "cargo": {
                        "buildScripts": { "enable": true }
                    },
                    "procMacro": { "enable": true },
                    "inlayHints": {
                        "bindingModeHints": {
                            "enable": false
                        },
                        "chainingHints": {
                            "enable": true
                        },
                        "closureReturnTypeHints": {
                            "enable": "never"
                        },
                        "closureStyle": "impl_fn",
                        "discriminantHints": {
                            "enable": "never"
                        },
                        "expressionAdjustmentHints": {
                            "enable": "never"
                        },
                        "genericPlaceholderHints": {
                            "enable": true
                        },
                        "implicitDrops": {
                            "enable": false
                        },
                        "lifetimeElisionHints": {
                            "enable": "never"
                        },
                        "maxLength": null,
                        "parameterHints": {
                            "enable": true
                        },
                        "rangeHints": {
                            "enable": false
                        },
                        "renderColons": true,
                        "typeHints": {
                            "enable": true,
                            "hideClosureInitialization": false,
                            "hideNamedConstructor": false
                        }
                    },
                    "typing": {
                        "autoClosingAngleBrackets": {
                            "enable": true
                        }
                    }
                }))
            }
            "javascript" | "typescript" | "typescriptreact" | "javascriptreact" => {
                // TypeScript language server configuration
                Some(json!({
                    "preferences": {
                        "includeInlayParameterNameHints": "all",
                        "includeInlayFunctionParameterTypeHints": true,
                        "includeInlayVariableTypeHints": true,
                        "includeInlayPropertyDeclarationTypeHints": true,
                        "includeInlayEnumMemberValueHints": true,
                        "quotePreference": "auto",
                        "importModuleSpecifierPreference": "relative"
                    },
                    "hostInfo": "ovim"
                }))
            }
            "python" => {
                // pyright/pylsp configuration
                Some(json!({
                    "python": {
                        "analysis": {
                            "typeCheckingMode": "basic",
                            "autoSearchPaths": true,
                            "diagnosticMode": "workspace"
                        }
                    }
                }))
            }
            "astro" => {
                // astro-ls refuses to initialize without `typescript.tsdk`
                // pointing at a TypeScript lib directory. Resolve it the way
                // nvim-lspconfig/mason do: workspace TypeScript first, then
                // one installed near the server binary.
                match resolve_typescript_tsdk(&root_uri, &self.inner.command) {
                    Some(tsdk) => Some(json!({
                        "typescript": { "tsdk": tsdk }
                    })),
                    None => {
                        crate::lsp_warn!(
                            &self.log_prefix(),
                            "No TypeScript SDK found for astro-ls (checked workspace \
                             node_modules and near the server binary). Install one with: \
                             npm install -g typescript@6"
                        );
                        None
                    }
                }
            }
            _ => {
                // No specific initialization options for other languages
                None
            }
        };

        #[allow(deprecated)] // root_uri/root_path deprecated but needed for LSP backwards compat
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            root_path: None,
            initialization_options: initialization_options.clone(),
            capabilities: client_capabilities,
            trace: None,
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri.clone(),
                name: "workspace".to_string(),
            }]),
            client_info: Some(lsp_types::ClientInfo {
                name: "ovim".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            locale: None,
            work_done_progress_params: Default::default(),
        };

        crate::lsp_info!(
            &self.log_prefix(),
            "LSP Initialize | Language: {} | Root: {} | InitOptions: {}",
            self.inner.language,
            root_uri.as_str(),
            initialization_options
                .as_ref()
                .map(|opts| opts.to_string())
                .unwrap_or_else(|| "None".to_string())
        );

        let result = self
            .request("initialize", serde_json::to_value(params)?)
            .await
            .context("Failed to send initialize request")?;

        let init_result: InitializeResult =
            serde_json::from_value(result).context("Failed to parse initialize response")?;

        // Store capabilities
        let mut caps = self.inner.capabilities.lock().await;
        *caps = Some(init_result.capabilities.clone());
        drop(caps); // Release lock

        // Cache capability flags for lock-free access
        self.cache_capabilities(&init_result.capabilities);

        // Send initialized notification
        self.notify("initialized", serde_json::to_value(InitializedParams {})?)
            .await
            .context("Failed to send initialized notification")?;
        if let Some(settings) = workspace_settings_for_language(&self.inner.language) {
            self.notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": settings }),
            )
            .await
            .context("Failed to send workspace configuration")?;
        }

        // Transition to Ready state and replay pending operations
        self.transition_to(ServerState::Ready {
            initialized_at: Instant::now(),
            capabilities: init_result.capabilities,
        })
        .await;

        Ok(())
    }

    /// Transitions to a new state, atomically updating the server state field.
    async fn transition_to(&self, new_state: ServerState) {
        let mut state = self.inner.state.lock().await;
        *state = new_state;
    }

    /// Gets the current server state
    pub async fn state(&self) -> ServerState {
        self.inner.state.lock().await.clone()
    }

    /// Checks if server is ready to accept requests
    pub async fn is_ready(&self) -> bool {
        matches!(self.state().await, ServerState::Ready { .. })
    }

    /// Sends a request and waits for the response
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        // Track LSP request metrics
        let _timer = crate::metrics::LSP_REQUEST_DURATION.start_timer();
        crate::metrics::LSP_REQUESTS_TOTAL.inc();

        let request_id =
            RequestId::Number(self.inner.next_request_id.fetch_add(1, Ordering::SeqCst));

        crate::lsp_debug!(
            &self.log_prefix(),
            "LSP-REQUEST: {} | ID: {:?} | Params: {}",
            method,
            request_id,
            params
        );

        let (tx, rx) = oneshot::channel();

        // Check pending count without inserting (insert after send to avoid orphaned entries)
        {
            let pending = self.inner.pending_requests.lock().await;
            if pending.len() >= MAX_PENDING_REQUESTS {
                return Err(anyhow!(
                    "Too many pending LSP requests ({}/{}) - server may be slow or hanging",
                    pending.len(),
                    MAX_PENDING_REQUESTS
                ));
            }
        }

        // Build request message
        let msg = JsonRpcMessage::request(request_id.clone(), method.to_string(), params);

        // Check message size by serializing to bytes (avoids double serialization)
        // The write_message function will serialize again, but we need size check first
        let serialized_size = serde_json::to_vec(&msg)
            .context("Failed to estimate request size")?
            .len();
        if serialized_size > MAX_MESSAGE_SIZE {
            return Err(anyhow!(
                "Request '{}' too large: {} bytes (max {} bytes / {:.1} MB)",
                method,
                serialized_size,
                MAX_MESSAGE_SIZE,
                MAX_MESSAGE_SIZE as f64 / (1024.0 * 1024.0)
            ));
        }

        // Check if server is still alive before sending
        {
            let state = self.inner.state.lock().await;
            match &*state {
                ServerState::Failed { error, .. } => {
                    return Err(anyhow!("LSP server failed: {} (method: {})", error, method));
                }
                ServerState::Terminated => {
                    return Err(anyhow!("LSP server terminated (method: {})", method));
                }
                // OV-00156: Reject requests during shutdown to avoid queuing work
                // that will never be answered — except for the "shutdown" request
                // itself, which must be sent during ShuttingDown state (OV-00225)
                ServerState::ShuttingDown if method != "shutdown" => {
                    return Err(anyhow!("LSP server shutting down (method: {})", method));
                }
                _ => {} // Ready or Initializing — proceed
            }
        }

        // Register FIRST — the entry must exist before the message hits the wire.
        // A fast LSP server can respond before an insert-after-send would execute,
        // causing the reader task to silently drop the response.
        {
            let mut pending = self.inner.pending_requests.lock().await;
            pending.insert(
                request_id.clone(),
                PendingRequest {
                    sender: tx,
                    sent_at: Instant::now(),
                    method: method.to_string(),
                },
            );
        }

        // THEN send — if send fails, clean up the registration.
        if self.inner.outgoing_tx.send(msg).await.is_err() {
            let mut pending = self.inner.pending_requests.lock().await;
            pending.remove(&request_id);
            return Err(anyhow!(
                "LSP server not responding — channel closed (method: {})",
                method,
            ));
        }

        // Wait for response with timeout
        // Use longer timeout for initialize request (jdtls can be very slow)
        let timeout_duration = if method == "initialize" {
            // Java LSP can take 5+ minutes to index large projects on first run
            std::time::Duration::from_secs(300) // 5 minutes for initialize
        } else {
            std::time::Duration::from_secs(10) // 10s for other requests
        };

        // eprintln!("[LSP-REQUEST] Waiting for response (timeout: {:?})", timeout_duration);
        let start_time = Instant::now();

        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(result)) => {
                let elapsed = start_time.elapsed();
                let result_preview = match &result {
                    Ok(value) if value.is_null() => "null".to_string(),
                    Ok(value) => {
                        let json_str =
                            serde_json::to_string(value).unwrap_or_else(|_| "?".to_string());
                        if json_str.len() > 200 {
                            format!("{}...", crate::unicode::truncate_bytes(&json_str, 200))
                        } else {
                            json_str
                        }
                    }
                    Err(e) => {
                        crate::metrics::LSP_ERRORS_TOTAL.inc();
                        format!("Error: {}", e)
                    }
                };
                crate::lsp_info!(
                    &self.log_prefix(),
                    "LSP-RESPONSE: {} | {:?} | ID: {:?} | Result: {}",
                    method,
                    elapsed,
                    request_id,
                    result_preview
                );
                result.context(format!("LSP request '{}' failed", method))
            }
            Ok(Err(_)) => {
                let _elapsed = start_time.elapsed();
                crate::metrics::LSP_ERRORS_TOTAL.inc();
                // eprintln!("[LSP-ERROR] Channel closed: {} | After: {:?}", method, elapsed);
                Err(anyhow!("Response channel closed for method '{}'", method))
            }
            Err(_) => {
                crate::metrics::LSP_ERRORS_TOTAL.inc();
                // eprintln!("[LSP-ERROR] Timeout: {} | After: {:?}", method, timeout_duration);
                // Timeout - remove pending request
                let mut pending = self.inner.pending_requests.lock().await;
                pending.remove(&request_id);
                Err(anyhow!(
                    "Request '{}' timed out after {:?}",
                    method,
                    timeout_duration
                ))
            }
        }
    }

    /// Sends a notification (no response expected)
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = JsonRpcMessage::notification(method.to_string(), params);

        // Check message size by serializing to bytes (avoids double serialization)
        // The write_message function will serialize again, but we need size check first
        let serialized_size = serde_json::to_vec(&msg)
            .context("Failed to estimate notification size")?
            .len();
        if serialized_size > MAX_MESSAGE_SIZE {
            return Err(anyhow!(
                "Notification '{}' too large: {} bytes (max {} bytes / {:.1} MB)",
                method,
                serialized_size,
                MAX_MESSAGE_SIZE,
                MAX_MESSAGE_SIZE as f64 / (1024.0 * 1024.0)
            ));
        }

        self.inner.outgoing_tx.send(msg).await.map_err(|_| {
            anyhow!(
                "LSP server not responding — channel closed (notification: {})",
                method
            )
        })?;
        Ok(())
    }

    /// Sends a response to a request from the server
    pub async fn send_response(&self, response: JsonRpcMessage) -> Result<()> {
        // Check message size
        let serialized_size = serde_json::to_vec(&response)
            .context("Failed to estimate response size")?
            .len();
        if serialized_size > MAX_MESSAGE_SIZE {
            return Err(anyhow!(
                "Response too large: {} bytes (max {} bytes / {:.1} MB)",
                serialized_size,
                MAX_MESSAGE_SIZE,
                MAX_MESSAGE_SIZE as f64 / (1024.0 * 1024.0)
            ));
        }

        self.inner
            .outgoing_tx
            .send(response)
            .await
            .map_err(|_| anyhow!("Failed to send response"))?;
        Ok(())
    }

    /// Receives the next incoming notification/request
    pub async fn receive(&self) -> Option<JsonRpcMessage> {
        let mut rx = self.inner.incoming_rx.lock().await;
        if let Some(ref mut rx) = *rx {
            rx.recv().await
        } else {
            None
        }
    }

    /// Shuts down the language server gracefully
    /// Follows proper shutdown sequence to avoid zombie processes:
    /// 1. LSP shutdown request
    /// 2. LSP exit notification
    /// 3. Wait for graceful exit (5s)
    /// 4. SIGTERM (if Unix, 3s wait)
    /// 5. SIGKILL (last resort)
    pub async fn shutdown(&mut self) -> Result<()> {
        let prefix = self.log_prefix();

        // Transition to ShuttingDown state
        self.transition_to(ServerState::ShuttingDown).await;

        // Drain pending requests before shutdown — callers waiting on responses
        // get a clear error instead of hanging until timeout
        {
            let mut pending = self.inner.pending_requests.lock().await;
            for (_id, req) in pending.drain() {
                let _ = req.sender.send(Err(anyhow!("LSP server shutting down")));
            }
        }

        // Step 1: Send LSP shutdown request
        let shutdown_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.request("shutdown", Value::Null),
        )
        .await;

        if shutdown_result.is_ok() {
            // Step 2: Send exit notification
            let _ = self.notify("exit", Value::Null).await;

            // Step 3: Wait for graceful exit
            let mut process = self.inner.process.lock().await;
            if let Some(ref mut child) = *process {
                match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                    Ok(Ok(_status)) => {
                        return Ok(());
                    }
                    Ok(Err(e)) => {
                        crate::lsp_warn!(&prefix, "Shutdown: Error waiting for exit: {}", e);
                    }
                    Err(_) => {
                        // Graceful exit timeout, trying SIGTERM
                    }
                }
            } else {
                return Ok(()); // No process to shutdown
            }
        }

        // Step 4: Try SIGTERM (Unix only)
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            let mut process = self.inner.process.lock().await;
            if let Some(ref mut child) = *process {
                if let Some(pid) = child.id() {
                    if let Ok(()) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                        // Wait 3 seconds for SIGTERM to take effect
                        match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
                            .await
                        {
                            Ok(Ok(_status)) => {
                                return Ok(());
                            }
                            Ok(Err(e)) => {
                                crate::lsp_warn!(&prefix, "Shutdown: Error after SIGTERM: {}", e);
                            }
                            Err(_) => {
                                // SIGTERM timeout, using SIGKILL
                            }
                        }
                    }
                }
            }
        }

        // Step 5: Last resort - SIGKILL
        let mut process = self.inner.process.lock().await;
        if let Some(ref mut child) = *process {
            if let Err(e) = child.kill().await {
                crate::lsp_error!(&prefix, "Shutdown: SIGKILL failed: {}", e);
            }

            // Wait to reap zombie
            if let Err(e) = child.wait().await {
                crate::lsp_error!(&prefix, "Shutdown: Error reaping process: {}", e);
            }
        }

        // Shutdown all supervised tasks
        if let Err(e) = self.inner.supervisor.shutdown_all().await {
            crate::lsp_error!(&prefix, "Shutdown: Error shutting down tasks: {}", e);
        }

        // Final transition to Terminated
        self.transition_to(ServerState::Terminated).await;

        Ok(())
    }

    /// Gets health information for this language server
    pub async fn health_check(&self) -> LanguageServerHealth {
        let state = self.state().await;
        let pending_count = self.inner.pending_requests.lock().await.len();
        let has_caps = self.inner.capabilities.lock().await.is_some();
        let tasks = self.inner.supervisor.health_check().await;

        // Determine uptime based on state
        let uptime = match &state {
            ServerState::Spawning => Duration::from_secs(0),
            ServerState::Initializing { started_at, .. } => started_at.elapsed(),
            ServerState::Ready { initialized_at, .. } => initialized_at.elapsed(),
            ServerState::Failed { at, .. } => at.elapsed(),
            ServerState::ShuttingDown | ServerState::Terminated => Duration::from_secs(0),
        };

        // Check if process is alive (OV-00154: use try_wait for accurate check)
        let is_alive = {
            let mut process = self.inner.process.lock().await;
            if let Some(ref mut child) = *process {
                match child.try_wait() {
                    Ok(None) => true,     // Still running
                    Ok(Some(_)) => false, // Exited
                    Err(_) => false,      // Error checking
                }
            } else {
                false
            }
        };

        // Convert state to string for easier debugging
        let state_str = match state {
            ServerState::Spawning => "Spawning".to_string(),
            ServerState::Initializing { .. } => "Initializing".to_string(),
            ServerState::Ready { .. } => "Ready".to_string(),
            ServerState::Failed { ref error, .. } => format!("Failed: {}", error),
            ServerState::ShuttingDown => "ShuttingDown".to_string(),
            ServerState::Terminated => "Terminated".to_string(),
        };

        LanguageServerHealth {
            language: self.inner.language.clone(),
            command: self.inner.command.clone(),
            state: state_str,
            uptime,
            pending_requests: pending_count,
            has_capabilities: has_caps,
            tasks,
            is_alive,
        }
    }

    /// Caches capability flags from ServerCapabilities for lock-free access.
    /// Called once during initialization.
    fn cache_capabilities(&self, caps: &ServerCapabilities) {
        use LspCapFlags as F;

        let mut flags = F::empty();

        // Simple is_some() checks
        if caps.definition_provider.is_some() {
            flags |= F::GOTO_DEFINITION;
        }
        if caps.declaration_provider.is_some() {
            flags |= F::GOTO_DECLARATION;
        }
        if caps.implementation_provider.is_some() {
            flags |= F::GOTO_IMPLEMENTATION;
        }
        if caps.type_definition_provider.is_some() {
            flags |= F::GOTO_TYPE_DEFINITION;
        }
        if caps.hover_provider.is_some() {
            flags |= F::HOVER;
        }
        if caps.completion_provider.is_some() {
            flags |= F::COMPLETION;
        }
        if caps.document_formatting_provider.is_some() {
            flags |= F::FORMATTING;
        }
        if caps.document_range_formatting_provider.is_some() {
            flags |= F::RANGE_FORMATTING;
        }
        if caps.code_action_provider.is_some() {
            flags |= F::CODE_ACTIONS;
        }
        if caps.references_provider.is_some() {
            flags |= F::REFERENCES;
        }
        if caps.rename_provider.is_some() {
            flags |= F::RENAME;
        }
        if caps.signature_help_provider.is_some() {
            flags |= F::SIGNATURE_HELP;
        }
        if caps.document_symbol_provider.is_some() {
            flags |= F::DOCUMENT_SYMBOL;
        }
        if caps.selection_range_provider.is_some() {
            flags |= F::SELECTION_RANGE;
        }
        if caps.workspace_symbol_provider.is_some() {
            flags |= F::WORKSPACE_SYMBOL;
        }
        if caps.document_highlight_provider.is_some() {
            flags |= F::DOCUMENT_HIGHLIGHT;
        }
        if caps.folding_range_provider.is_some() {
            flags |= F::FOLDING_RANGE;
        }
        if caps.call_hierarchy_provider.is_some() {
            flags |= F::CALL_HIERARCHY;
        }
        if caps.execute_command_provider.is_some() {
            flags |= F::EXECUTE_COMMAND;
        }
        if caps.inlay_hint_provider.is_some() {
            flags |= F::INLAY_HINT;
        }
        if caps.semantic_tokens_provider.is_some() {
            flags |= F::SEMANTIC_TOKENS;
        }

        // Prepare rename: needs deeper inspection
        if let Some(lsp_types::OneOf::Right(options)) = &caps.rename_provider {
            if options.prepare_provider.unwrap_or(false) {
                flags |= F::PREPARE_RENAME;
            }
        }

        // Incremental sync: needs enum matching
        let incremental = match &caps.text_document_sync {
            Some(lsp_types::TextDocumentSyncCapability::Kind(kind)) => {
                *kind == lsp_types::TextDocumentSyncKind::INCREMENTAL
            }
            Some(lsp_types::TextDocumentSyncCapability::Options(opts)) => {
                opts.change == Some(lsp_types::TextDocumentSyncKind::INCREMENTAL)
            }
            None => false,
        };
        if incremental {
            flags |= F::INCREMENTAL_SYNC;
        }

        // TODO(OV-00132): type_hierarchy_provider requires lsp-types 0.96+
        // Hardcoded off until we upgrade from 0.95.
        // flags |= F::TYPE_HIERARCHY;

        self.inner.cap_flags.store(flags.bits(), Ordering::Relaxed);
    }

    /// Sets a cached capability flag based on an LSP method name from dynamic registration.
    /// Called when the server sends `client/registerCapability` or `client/unregisterCapability`.
    pub fn set_capability_by_method(&self, method: &str, enabled: bool) {
        use LspCapFlags as F;
        let flag = match method {
            "textDocument/definition" => F::GOTO_DEFINITION,
            "textDocument/declaration" => F::GOTO_DECLARATION,
            "textDocument/implementation" => F::GOTO_IMPLEMENTATION,
            "textDocument/typeDefinition" => F::GOTO_TYPE_DEFINITION,
            "textDocument/hover" => F::HOVER,
            "textDocument/completion" => F::COMPLETION,
            "textDocument/formatting" => F::FORMATTING,
            "textDocument/rangeFormatting" => F::RANGE_FORMATTING,
            "textDocument/codeAction" => F::CODE_ACTIONS,
            "textDocument/references" => F::REFERENCES,
            "textDocument/rename" => F::RENAME,
            "textDocument/signatureHelp" => F::SIGNATURE_HELP,
            "textDocument/documentSymbol" => F::DOCUMENT_SYMBOL,
            "textDocument/selectionRange" => F::SELECTION_RANGE,
            "workspace/symbol" => F::WORKSPACE_SYMBOL,
            "textDocument/documentHighlight" => F::DOCUMENT_HIGHLIGHT,
            "textDocument/foldingRange" => F::FOLDING_RANGE,
            "textDocument/prepareCallHierarchy" => F::CALL_HIERARCHY,
            "workspace/executeCommand" => F::EXECUTE_COMMAND,
            "textDocument/inlayHint" => F::INLAY_HINT,
            "textDocument/semanticTokens"
            | "textDocument/semanticTokens/full"
            | "textDocument/semanticTokens/range" => F::SEMANTIC_TOKENS,
            _ => return,
        };
        self.inner.set_cap(flag, enabled);
    }

    /// Gets the server capabilities
    pub async fn capabilities(&self) -> Option<ServerCapabilities> {
        let caps = self.inner.capabilities.lock().await;
        caps.clone()
    }

    /// Checks if the server supports goto definition (lock-free)
    pub async fn supports_goto_definition(&self) -> bool {
        self.inner.has_cap(LspCapFlags::GOTO_DEFINITION)
    }

    /// Checks if the server supports goto declaration (lock-free)
    pub async fn supports_goto_declaration(&self) -> bool {
        self.inner.has_cap(LspCapFlags::GOTO_DECLARATION)
    }

    /// Checks if the server supports goto implementation (lock-free)
    pub async fn supports_goto_implementation(&self) -> bool {
        self.inner.has_cap(LspCapFlags::GOTO_IMPLEMENTATION)
    }

    /// Checks if the server supports goto type definition (lock-free)
    pub async fn supports_goto_type_definition(&self) -> bool {
        self.inner.has_cap(LspCapFlags::GOTO_TYPE_DEFINITION)
    }

    /// Checks if the server supports hover (lock-free)
    pub async fn supports_hover(&self) -> bool {
        self.inner.has_cap(LspCapFlags::HOVER)
    }

    /// Checks if the server supports completion (lock-free)
    pub async fn supports_completion(&self) -> bool {
        self.inner.has_cap(LspCapFlags::COMPLETION)
    }

    /// Best-effort: return completion trigger characters advertised by the server.
    ///
    /// LSP uses `String` for trigger characters; we currently treat these as single
    /// graphemes and take the first `char` of each entry.
    pub async fn completion_trigger_characters(&self) -> Vec<char> {
        let Some(caps) = self.capabilities().await else {
            return Vec::new();
        };
        let Some(provider) = caps.completion_provider else {
            return Vec::new();
        };
        provider
            .trigger_characters
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.chars().next())
            .collect()
    }

    /// Checks if the server supports formatting (lock-free)
    pub async fn supports_formatting(&self) -> bool {
        self.inner.has_cap(LspCapFlags::FORMATTING)
    }

    /// Checks if the server supports range formatting (lock-free)
    pub async fn supports_range_formatting(&self) -> bool {
        self.inner.has_cap(LspCapFlags::RANGE_FORMATTING)
    }

    /// Checks if the server supports code actions (lock-free)
    pub async fn supports_code_actions(&self) -> bool {
        self.inner.has_cap(LspCapFlags::CODE_ACTIONS)
    }

    /// Checks if the server supports references (lock-free)
    pub async fn supports_references(&self) -> bool {
        self.inner.has_cap(LspCapFlags::REFERENCES)
    }

    /// Checks if the server supports rename (lock-free)
    pub async fn supports_rename(&self) -> bool {
        self.inner.has_cap(LspCapFlags::RENAME)
    }

    /// Checks if the server supports prepare rename (lock-free)
    pub async fn supports_prepare_rename(&self) -> bool {
        self.inner.has_cap(LspCapFlags::PREPARE_RENAME)
    }

    /// Checks if the server supports signature help (lock-free)
    pub async fn supports_signature_help(&self) -> bool {
        self.inner.has_cap(LspCapFlags::SIGNATURE_HELP)
    }

    /// Checks if the server supports document symbols (lock-free)
    pub async fn supports_document_symbol(&self) -> bool {
        self.inner.has_cap(LspCapFlags::DOCUMENT_SYMBOL)
    }

    /// Checks if the server supports selection range (lock-free)
    pub async fn supports_selection_range(&self) -> bool {
        self.inner.has_cap(LspCapFlags::SELECTION_RANGE)
    }

    /// Checks if the server supports workspace symbols (lock-free)
    pub async fn supports_workspace_symbol(&self) -> bool {
        self.inner.has_cap(LspCapFlags::WORKSPACE_SYMBOL)
    }

    /// Checks if the server supports document highlight (lock-free)
    pub async fn supports_document_highlight(&self) -> bool {
        self.inner.has_cap(LspCapFlags::DOCUMENT_HIGHLIGHT)
    }

    /// Checks if the server supports incremental sync (lock-free)
    pub async fn supports_incremental_sync(&self) -> bool {
        self.inner.has_cap(LspCapFlags::INCREMENTAL_SYNC)
    }

    /// Test-only: force the incremental-sync capability so sync tests can
    /// exercise the diff path against a fake server that never negotiates
    /// capabilities.
    #[cfg(test)]
    pub(crate) fn force_incremental_sync_for_test(&self, enabled: bool) {
        self.inner.set_cap(LspCapFlags::INCREMENTAL_SYNC, enabled);
    }

    /// Checks if the server supports folding range (lock-free)
    pub async fn supports_folding_range(&self) -> bool {
        self.inner.has_cap(LspCapFlags::FOLDING_RANGE)
    }

    /// Checks if the server supports call hierarchy (lock-free)
    pub async fn supports_call_hierarchy(&self) -> bool {
        self.inner.has_cap(LspCapFlags::CALL_HIERARCHY)
    }

    /// Checks if the server supports type hierarchy (lock-free)
    pub async fn supports_type_hierarchy(&self) -> bool {
        self.inner.has_cap(LspCapFlags::TYPE_HIERARCHY)
    }

    /// Checks if the server supports execute command (lock-free)
    pub async fn supports_execute_command(&self) -> bool {
        self.inner.has_cap(LspCapFlags::EXECUTE_COMMAND)
    }

    /// Checks if the server supports inlay hints (lock-free)
    pub async fn supports_inlay_hints(&self) -> bool {
        self.inner.has_cap(LspCapFlags::INLAY_HINT)
    }

    /// Checks if the server supports semantic tokens (lock-free)
    pub async fn supports_semantic_tokens(&self) -> bool {
        self.inner.has_cap(LspCapFlags::SEMANTIC_TOKENS)
    }

    /// Gets the current server state (alias for introspection)
    pub async fn get_state(&self) -> ServerState {
        self.state().await
    }

    /// Gets the number of pending requests
    pub async fn pending_requests_count(&self) -> usize {
        let pending = self.inner.pending_requests.lock().await;
        pending.len()
    }

    /// Checks if the server has capabilities (is ready)
    pub async fn has_capabilities(&self) -> bool {
        let caps = self.inner.capabilities.lock().await;
        caps.is_some()
    }

    /// Gets the command used to start the server
    pub async fn get_command(&self) -> String {
        self.inner.command.clone()
    }

    /// Gets the command used to start the server (sync version)
    pub fn command(&self) -> &str {
        &self.inner.command
    }

    /// Cancels all pending requests for a specific method
    ///
    /// This is useful for high-frequency, low-priority operations like hover and completion
    /// where only the latest request matters. When a new request is made, previous ones
    /// become stale and can be safely cancelled.
    ///
    /// # LSP Cancellation Protocol
    ///
    /// The LSP protocol supports request cancellation via the `$/cancelRequest` notification:
    /// ```json
    /// {
    ///   "jsonrpc": "2.0",
    ///   "method": "$/cancelRequest",
    ///   "params": { "id": 123 }
    /// }
    /// ```
    ///
    /// # Server Behavior
    ///
    /// According to the LSP spec, servers may respond in three ways to cancellation:
    /// 1. **Stop processing** and respond with error -32800 (Request Cancelled)
    /// 2. **Continue processing** and respond normally if work already done
    /// 3. **Ignore** the cancellation if the request already completed
    ///
    /// # Race Conditions in Async Systems
    ///
    /// Request cancellation introduces interesting race conditions:
    ///
    /// **Race 1: Cancel vs Response**
    /// - Client sends request ID 1
    /// - Server starts processing
    /// - Client sends `$/cancelRequest` for ID 1
    /// - Server finishes and sends response for ID 1
    /// - Result: Client receives response after cancellation (harmless, we ignore it)
    ///
    /// **Race 2: Multiple Cancellations**
    /// - Client sends request ID 1, 2, 3
    /// - Client cancels 1, 2, 3
    /// - Server might respond to 2 before processing cancellation for 1
    /// - Result: Out-of-order responses (we filter by removing from pending_requests)
    ///
    /// # Why Request Ordering Matters
    ///
    /// Consider rapid cursor movement over symbols A → B → C:
    ///
    /// **Without cancellation:**
    /// ```text
    /// t=0ms:  Request hover for A (ID 1)
    /// t=50ms: Request hover for B (ID 2)
    /// t=100ms: Request hover for C (ID 3)
    /// t=200ms: Response for A arrives → UI shows stale hover for A
    /// t=250ms: Response for B arrives → UI shows stale hover for B
    /// t=300ms: Response for C arrives → UI shows correct hover for C
    /// ```
    /// User sees flickering UI with wrong information!
    ///
    /// **With cancellation:**
    /// ```text
    /// t=0ms:  Request hover for A (ID 1)
    /// t=50ms: Cancel ID 1, Request hover for B (ID 2)
    /// t=100ms: Cancel ID 2, Request hover for C (ID 3)
    /// t=150ms: Response for C arrives → UI shows correct hover immediately
    /// ```
    /// Server only processes final request, UI is always correct.
    ///
    /// # Implementation Strategy
    ///
    /// 1. **Find** all pending request IDs matching the method
    /// 2. **Send** `$/cancelRequest` notification for each (don't wait for response)
    /// 3. **Remove** from pending_requests map (fail the oneshot receiver)
    /// 4. **Continue** - caller's await will fail with "Request cancelled"
    ///
    /// # Arguments
    ///
    /// * `method` - LSP method name (e.g., "textDocument/hover", "textDocument/completion")
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Cancellations sent successfully
    /// * `Err` - Failed to send cancellation notification
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // In hover implementation:
    /// server.cancel_requests_by_method("textDocument/hover").await?;
    /// let result = server.request("textDocument/hover", params).await?;
    /// ```
    pub async fn cancel_requests_by_method(&self, method: &str) -> Result<()> {
        // Find all request IDs for this method
        // We need to do this in two steps to avoid holding the lock during async operations
        let to_cancel: Vec<RequestId> = {
            let pending = self.inner.pending_requests.lock().await;
            pending
                .iter()
                .filter(|(_, req)| req.method == method)
                .map(|(id, _)| id.clone())
                .collect()
        }; // Lock released here

        if to_cancel.is_empty() {
            // No requests to cancel - fast path
            return Ok(());
        }

        crate::lsp_debug!(
            &self.log_prefix(),
            "Cancelling {} pending requests for method '{}'",
            to_cancel.len(),
            method
        );

        // Send $/cancelRequest notification for each request ID
        // These are fire-and-forget notifications - we don't wait for acknowledgment
        for id in &to_cancel {
            let params = serde_json::json!({ "id": id });

            // Log the cancellation for debugging
            crate::lsp_debug!(
                &self.log_prefix(),
                "Sending $/cancelRequest for ID {:?} (method: {})",
                id,
                method
            );

            // Send cancellation notification
            // This is a notification, not a request, so we don't expect a response
            if let Err(e) = self.notify("$/cancelRequest", params).await {
                // If we fail to send cancellation, log but continue
                // The server might still respond, but we'll ignore it by removing from pending
                crate::lsp_warn!(
                    &self.log_prefix(),
                    "Failed to send $/cancelRequest for ID {:?}: {}",
                    id,
                    e
                );
            }
        }

        // Remove cancelled requests from pending map and fail their receivers
        // This ensures callers waiting on these requests get an error
        {
            let mut pending = self.inner.pending_requests.lock().await;
            for id in to_cancel {
                if let Some(req) = pending.remove(&id) {
                    // Send cancellation error to the waiting caller
                    // The underscore pattern ignores send errors (receiver might have dropped)
                    let _ = req.sender.send(Err(anyhow!(
                        "Request '{}' cancelled by client (newer request supersedes this one)",
                        method
                    )));

                    crate::lsp_debug!(
                        &self.log_prefix(),
                        "Removed cancelled request ID {:?} from pending map",
                        id
                    );
                }
            }
        } // Lock released here

        Ok(())
    }
}

/// Locate a TypeScript SDK directory for servers that require the
/// `typescript.tsdk` initialization option (astro-ls).
///
/// Resolution order mirrors nvim-lspconfig/mason:
/// 1. `node_modules/typescript/lib` in the workspace root or any ancestor
///    (walks up so pnpm/npm workspaces with hoisted deps are found).
/// 2. A TypeScript install near the resolved server binary — `npm install -g`
///    nests dependencies under the package's own `node_modules`, and the
///    global npm root itself is an ancestor of the server script.
///
/// Candidates are validated because TypeScript 7.x dropped the
/// `typescript.js`/`tsserverlibrary.js` bundles the language server loads;
/// a 7.x install must be skipped in favor of a usable 6.x one.
fn resolve_typescript_tsdk(root_uri: &Uri, command: &str) -> Option<String> {
    if let Some(root) = super::uri_to_file_path(root_uri) {
        let mut dir: Option<&std::path::Path> = Some(root.as_path());
        while let Some(d) = dir {
            let candidate = d.join("node_modules").join("typescript").join("lib");
            if is_valid_tsdk(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
            dir = d.parent();
        }
    }

    let command_path = resolve_command_path(command)?;
    let canonical = std::fs::canonicalize(&command_path).unwrap_or(command_path);
    for ancestor in canonical.ancestors() {
        let candidate = ancestor.join("node_modules").join("typescript").join("lib");
        if is_valid_tsdk(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_valid_tsdk(dir: &std::path::Path) -> bool {
    dir.join("tsserverlibrary.js").exists() || dir.join("typescript.js").exists()
}

/// Resolve a server command to a filesystem path so tsdk lookup can walk
/// its ancestors. Bare names are searched in $PATH, then the well-known
/// package manager locations the LSP finder already trusts.
fn resolve_command_path(command: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(command);
    if path.components().count() > 1 {
        return Some(path.to_path_buf());
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    crate::language_config::find_in_well_known_locations(command).map(std::path::PathBuf::from)
}

/// Hover client capabilities advertised at `initialize`.
///
/// Listing `Markdown` first asks servers (e.g. rust-analyzer) to emit
/// fenced code blocks instead of plaintext, which the hover renderer
/// uses to drive syntax highlighting.
fn hover_client_capabilities() -> lsp_types::HoverClientCapabilities {
    lsp_types::HoverClientCapabilities {
        dynamic_registration: Some(true),
        content_format: Some(vec![
            lsp_types::MarkupKind::Markdown,
            lsp_types::MarkupKind::PlainText,
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_generation() {
        let next_request_id = AtomicU64::new(1);

        assert_eq!(next_request_id.fetch_add(1, Ordering::SeqCst), 1);
        assert_eq!(next_request_id.fetch_add(1, Ordering::SeqCst), 2);
    }

    #[test]
    fn hover_capabilities_advertise_markdown_first() {
        // Without advertising Markdown, servers like rust-analyzer fall back
        // to plaintext and the hover renderer has no code fences to highlight.
        let caps = hover_client_capabilities();
        let formats = caps.content_format.expect("content_format set");
        assert_eq!(formats.first(), Some(&lsp_types::MarkupKind::Markdown));
        assert!(formats.contains(&lsp_types::MarkupKind::PlainText));
    }
}
