use crate::lsp::LspManager;
use ropey::Rope;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Content type for hover window - distinguishes LSP hover from diagnostic popups
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HoverContentType {
    #[default]
    LspHover,
    Diagnostic,
    BlameInfo,
    AiReasoning,
}

/// Per-document synchronisation state, keyed by canonical file path.
///
/// Debouncing is handled entirely by `LspManager::ChangeDebouncer` (single
/// owner, 150 ms).  The editor side just tracks "dirty" / "sent" so it
/// forwards content to the debouncer on the next tick.
#[derive(Debug, Clone, Default)]
pub struct DocumentSyncState {
    pub buffer_modified: bool,
    pub buffer_saved: bool,
    pub last_flushed_content: Option<Arc<str>>,
    pub last_queued_content: Option<Arc<str>>,
    pub target_lsp_version: Option<i32>,
    /// Track whether we've sent didOpen for this document
    pub did_open_sent: bool,
    /// The buffer content changed without the server hearing about it (e.g.
    /// reload after an external write). The next sync MUST send a full
    /// document update: reconcile seeding and the content-equality no-op
    /// guard are both bypassed, because they assume "server text == buffer
    /// text", which is exactly what is broken here. (OV-00324)
    pub force_full_resend: bool,
}

impl DocumentSyncState {
    pub fn mark_modified(&mut self) {
        self.buffer_modified = true;
    }

    pub fn mark_saved(&mut self) {
        self.buffer_saved = true;
    }

    pub fn is_modified(&self) -> bool {
        self.buffer_modified
    }

    pub fn should_send_save(&self) -> bool {
        self.buffer_saved
    }

    pub fn flushed_content(&self) -> Option<&str> {
        self.last_flushed_content.as_deref()
    }

    pub fn queued_content(&self) -> Option<&str> {
        self.last_queued_content.as_deref()
    }

    pub fn mark_change_queued(&mut self, queued_content: Arc<str>, target_lsp_version: i32) {
        self.buffer_modified = true;
        self.last_queued_content = Some(queued_content);
        self.target_lsp_version = Some(target_lsp_version);
    }

    pub fn mark_change_flushed(
        &mut self,
        flushed_content: Arc<str>,
        flushed_version: i32,
        current_content: Option<&str>,
    ) {
        self.last_flushed_content = Some(flushed_content.clone());

        if self
            .target_lsp_version
            .is_some_and(|target| target <= flushed_version)
        {
            self.target_lsp_version = None;
            if self.last_queued_content.as_deref() == Some(&*flushed_content) {
                self.last_queued_content = None;
            }
        }

        self.buffer_modified = current_content.is_some_and(|current| {
            current != &*flushed_content || self.target_lsp_version.is_some()
        });
    }

    pub fn mark_save_sent(&mut self) {
        self.buffer_saved = false;
    }
}

/// Fingerprint of the most recent viewport-scoped inlay hint request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintRequestKey {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub lsp_version: i32,
}

/// Cache for LSP hover results to avoid redundant requests
#[derive(Debug, Clone)]
pub struct HoverCache {
    pub file_path: String,
    pub line: usize,
    pub col: usize,
    pub buffer_version: usize,
    pub hover_text: String,
    pub cached_at: std::time::Instant,
}

impl HoverCache {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

    pub fn is_valid(
        &self,
        file_path: &str,
        line: usize,
        col: usize,
        buffer_version: usize,
    ) -> bool {
        self.file_path == file_path
            && self.line == line
            && self.col == col
            && self.buffer_version == buffer_version
            && self.cached_at.elapsed() < Self::MAX_AGE
    }

    pub fn new(
        file_path: String,
        line: usize,
        col: usize,
        buffer_version: usize,
        hover_text: String,
    ) -> Self {
        Self {
            file_path,
            line,
            col,
            buffer_version,
            hover_text,
            cached_at: std::time::Instant::now(),
        }
    }
}

/// Rope-anchored char offsets for one cached diagnostic, computed against the
/// buffer at placement time. `line_start` is the exact anchor
/// `decorations_from_diagnostics` gives the diagnostic's EOL decoration, so
/// projecting both through the edit log keeps every consumer (squiggle,
/// gutter sign, echo, float) on the same line as the virtual text. (OV-00328)
#[derive(Debug, Clone)]
pub struct DiagnosticAnchor {
    pub line_start: usize,
    pub start: usize,
    pub end: usize,
}

/// Anchors for `current_file_diagnostics`, parallel by index.
#[derive(Debug, Clone)]
pub struct DiagnosticAnchors {
    pub anchors: Vec<DiagnosticAnchor>,
    /// Buffer version the anchors were computed against.
    pub source_version: u64,
}

/// Diagnostics projected through the edit log, grouped by their projected
/// line. Mirrors `ProjectedDecorations`: built once per render pass so the
/// per-visible-line lookups don't re-project every diagnostic. (OV-00328)
#[derive(Debug, Default, Clone)]
pub struct ProjectedDiagnostics {
    by_line: BTreeMap<usize, Vec<lsp_types::Diagnostic>>,
}

impl ProjectedDiagnostics {
    pub(crate) fn new(by_line: BTreeMap<usize, Vec<lsp_types::Diagnostic>>) -> Self {
        Self { by_line }
    }

    /// Diagnostics whose projected start line equals `line`.
    pub fn for_line(&self, line: usize) -> &[lsp_types::Diagnostic] {
        self.by_line.get(&line).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Consume the snapshot, returning the given line's diagnostics.
    pub fn take_line(mut self, line: usize) -> Vec<lsp_types::Diagnostic> {
        self.by_line.remove(&line).unwrap_or_default()
    }

    /// 64-bit fingerprint of the line's full diagnostic set (projected
    /// ranges and severities), for render cache invalidation: the underline
    /// squiggle is baked into cached rows and the set can change without a
    /// buffer edit (save → republish). Lines with no diagnostics always hash
    /// to the same value. (OV-00329)
    pub fn line_hash(&self, line: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for diag in self.for_line(line) {
            diag.range.start.line.hash(&mut hasher);
            diag.range.start.character.hash(&mut hasher);
            diag.range.end.line.hash(&mut hasher);
            diag.range.end.character.hash(&mut hasher);
            severity_rank(diag.severity).hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Distinct value per severity for hashing. Missing severity is its own
/// bucket: it renders as ERROR today, but folding it into ERROR here would
/// mask a republish that only flips between the two.
fn severity_rank(severity: Option<lsp_types::DiagnosticSeverity>) -> u8 {
    match severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => 1,
        Some(lsp_types::DiagnosticSeverity::WARNING) => 2,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => 3,
        Some(lsp_types::DiagnosticSeverity::HINT) => 4,
        None => 0,
        _ => 5,
    }
}

/// Convert an LSP position to an absolute char offset in the rope.
fn position_to_char_offset(rope: &Rope, pos: lsp_types::Position) -> usize {
    let line = pos.line as usize;
    if line >= rope.len_lines() {
        return rope.len_chars();
    }
    let line_text = crate::display::line_content(rope, line);
    let char_idx = crate::lsp::utf16_to_char_col(&line_text, pos.character);
    (rope.line_to_char(line) + char_idx).min(rope.len_chars())
}

#[derive(Debug, Clone)]
pub struct AvailableCodeAction {
    /// LSP server ID that produced this action (language ID for primary server).
    pub server_id: String,
    /// The code action payload as returned by the server.
    pub action: lsp_types::CodeActionOrCommand,
    /// Whether this action has been resolved via `codeAction/resolve`.
    pub resolved: bool,
}

/// LSP-related state for the editor
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LspResultType {
    References,
    DocumentSymbols,
    WorkspaceSymbols,
    CallHierarchy,
    TypeHierarchy,
}

/// Per-feature intent flags for LSP actions.
///
/// Multiple intents can be set simultaneously (unlike the old single-slot
/// `Option<LspAction>` which lost actions when two were queued in the same
/// frame). Each flag is checked and cleared independently by
/// `dispatch_pending_intents()`.
#[derive(Default)]
pub struct LspIntents {
    pub goto_definition: bool,
    pub goto_definition_new_tab: bool,
    pub goto_implementation: bool,
    pub goto_implementation_new_tab: bool,
    pub goto_type: bool,
    pub hover: bool,
    pub completion: bool,
    pub format_document: bool,
    pub code_actions: bool,
    pub call_hierarchy_incoming: bool,
    pub call_hierarchy_outgoing: bool,
    pub type_hierarchy: bool,
    pub find_references: bool,
    pub document_symbols: bool,
    pub workspace_symbols: bool,
    pub organize_imports: bool,
    pub rename: Option<String>,
    pub semantic_tokens: bool,
}

impl LspIntents {
    /// Clear all intent flags.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Container for all LSP-related state in the editor
pub struct LspState {
    /// LSP manager (optional, only if LSP is enabled)
    pub lsp_manager: Option<Arc<LspManager>>,
    /// Cached diagnostic count (errors, warnings, info, hints) for status line display
    pub diagnostic_count: (usize, usize, usize, usize),
    /// Hover information to display (from LSP)
    pub hover_info: Option<String>,
    /// Scroll offset for hover window (line number)
    pub hover_scroll: usize,
    /// Horizontal scroll offset for hover window (columns)
    pub hover_h_scroll: usize,
    /// Cursor position when hover was triggered (line, col) - for positioning popup
    pub hover_position: Option<(usize, usize)>,
    /// Per-document sync state (tracked by canonical file path)
    pub document_sync: HashMap<String, DocumentSyncState>,
    /// Latest status published by the LSP subsystem.
    pub status: String,
    /// Active LSP servers (language_id -> server_name)
    pub active_lsp_servers: HashMap<String, String>,
    /// Flag to indicate LSP needs initialization for current file
    pub needs_lsp_init: bool,
    /// File path that needs didClose notification (set when switching files)
    pub pending_did_close_file: Option<String>,
    /// Available code actions at current cursor position
    pub available_code_actions: Vec<AvailableCodeAction>,
    /// Available completion items at current cursor position
    pub available_completions: Vec<lsp_types::CompletionItem>,
    /// Available LSP references at current cursor position
    pub available_references: Vec<lsp_types::Location>,
    /// Available document symbols for current file
    pub available_document_symbols: Vec<lsp_types::DocumentSymbol>,
    /// Available workspace symbols
    pub available_workspace_symbols: Vec<lsp_types::SymbolInformation>,
    /// Available call hierarchy items (incoming or outgoing)
    pub available_call_hierarchy: Vec<(String, lsp_types::Location)>,
    /// Available type hierarchy items (supertypes and subtypes)
    pub available_type_hierarchy: Vec<(String, lsp_types::Location)>,
    /// Inlay hints for the visible region
    pub inlay_hints: Vec<lsp_types::InlayHint>,
    /// Currently active LSP result type (for picker navigation)
    pub active_lsp_result_type: Option<LspResultType>,
    /// Cached diagnostics for current file (for inline display)
    pub current_file_diagnostics: Vec<lsp_types::Diagnostic>,
    /// Line-indexed view of `current_file_diagnostics`. Values are indices
    /// into the flat Vec, so per-line lookup is O(log L) without cloning.
    /// Kept in sync via `set_current_file_diagnostics` / `clear_current_file_diagnostics`.
    pub diagnostics_by_line: BTreeMap<usize, Vec<usize>>,
    /// Rope-anchored offsets for `current_file_diagnostics`, set by
    /// `anchor_current_file_diagnostics` when diagnostics are placed against a
    /// known rope/version. `None` (e.g. diagnostics stored without a rope)
    /// disables projection — lookups fall back to the raw LSP ranges.
    pub diagnostic_anchors: Option<DiagnosticAnchors>,
    /// File path when diagnostics were last cached.
    /// Prevents showing diagnostics from a previous file after save-as/path swaps.
    pub diagnostics_file_path: Option<String>,
    /// Current LSP document version for the active file.
    /// Updated in `send_lsp_changes_if_modified` and diagnostic refresh.
    pub current_file_lsp_version: i32,
    /// Last LSP document version definitely seen by the server for the active
    /// file (didOpen/didChange flushed, not merely queued locally).
    pub current_file_lsp_sent_version: i32,
    /// Cached hover result to avoid redundant LSP requests
    pub hover_cache: Option<HoverCache>,
    /// Content type for hover window (LSP hover vs diagnostic)
    pub hover_content_type: HoverContentType,
}

impl LspState {
    /// Creates a new LspState with default values
    pub fn new() -> Self {
        Self {
            lsp_manager: None,
            diagnostic_count: (0, 0, 0, 0),
            hover_info: None,
            hover_scroll: 0,
            hover_h_scroll: 0,
            hover_position: None,
            document_sync: HashMap::new(),
            status: String::new(),
            active_lsp_servers: HashMap::new(),
            needs_lsp_init: false,
            pending_did_close_file: None,
            available_code_actions: Vec::new(),
            available_completions: Vec::new(),
            available_references: Vec::new(),
            available_document_symbols: Vec::new(),
            available_workspace_symbols: Vec::new(),
            available_call_hierarchy: Vec::new(),
            available_type_hierarchy: Vec::new(),
            inlay_hints: Vec::new(),
            active_lsp_result_type: None,
            current_file_diagnostics: Vec::new(),
            diagnostics_by_line: BTreeMap::new(),
            diagnostic_anchors: None,
            diagnostics_file_path: None,
            current_file_lsp_version: 0,
            current_file_lsp_sent_version: 0,
            hover_cache: None,
            hover_content_type: HoverContentType::default(),
        }
    }

    /// Get language IDs of currently active/running LSP servers
    pub fn running_server_languages(&self) -> Vec<String> {
        self.active_lsp_servers.keys().cloned().collect()
    }

    /// Replace the cached diagnostics and rebuild the line index.
    pub fn set_current_file_diagnostics(&mut self, diagnostics: Vec<lsp_types::Diagnostic>) {
        self.diagnostics_by_line.clear();
        self.diagnostic_anchors = None;
        for (idx, diag) in diagnostics.iter().enumerate() {
            self.diagnostics_by_line
                .entry(diag.range.start.line as usize)
                .or_default()
                .push(idx);
        }
        self.current_file_diagnostics = diagnostics;
    }

    /// Anchor the cached diagnostics to rope char offsets so lookups can be
    /// projected through the edit log. Must be called with the same
    /// rope/version pair the diagnostics' EOL decorations are placed against
    /// (see `poll_pending_diagnostic_refresh_response`). (OV-00328)
    pub fn anchor_current_file_diagnostics(&mut self, rope: &Rope, source_version: u64) {
        let anchors = self
            .current_file_diagnostics
            .iter()
            .map(|diag| {
                let line = diag.range.start.line as usize;
                // Same line-start anchor decorations_from_diagnostics uses.
                let line_start = if line < rope.len_lines() {
                    rope.line_to_char(line)
                } else {
                    rope.len_chars()
                };
                let start = position_to_char_offset(rope, diag.range.start);
                let end = position_to_char_offset(rope, diag.range.end).max(start);
                DiagnosticAnchor {
                    line_start,
                    start,
                    end,
                }
            })
            .collect();
        self.diagnostic_anchors = Some(DiagnosticAnchors {
            anchors,
            source_version,
        });
    }

    /// Clear cached diagnostics and the line index together.
    pub fn clear_current_file_diagnostics(&mut self) {
        self.current_file_diagnostics.clear();
        self.diagnostics_by_line.clear();
        self.diagnostic_anchors = None;
    }
}

impl Default for LspState {
    fn default() -> Self {
        Self::new()
    }
}
