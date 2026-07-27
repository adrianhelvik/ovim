mod ai_agent;
mod ai_base_manifest;
mod ai_chat;
mod ai_chat_commands;
mod ai_chat_exa;
mod ai_chat_images;
pub mod ai_chat_input;
mod ai_chat_mutations;
mod ai_chat_presentation;
mod ai_chat_queue;
mod ai_chat_review;
mod ai_chat_scratch;
mod ai_chat_selection;
mod ai_chat_session;
pub(crate) mod ai_chat_state;
mod ai_chat_tools;
mod ai_chat_turn;
mod ai_chat_viewport;
mod ai_code_explanation;
mod ai_codex_auth;
mod ai_comprehension;
mod ai_context;
mod ai_durable_chat;
pub(crate) mod ai_integration;
mod ai_run_events;
mod ai_shell_process;
mod ai_skills;
mod ai_state;
mod ai_subagents;
mod ai_tool_execution;
mod ai_tool_path;
mod ai_tool_streaming;
mod ai_workflow;
mod blame_commands;
mod buffer_manager;
mod build_state;
mod change_tracking;
mod code_explanation;
mod command_context;
mod command_history;
mod completion;
mod debug_integration;
pub mod decoration;
mod editing_state;
mod filetree;
pub mod fuzzy;
pub mod grep;
mod input;
mod input_context;
mod input_state;
mod keymap;
mod lsp_integration;
pub mod lsp_manager_panel;
pub(crate) mod lsp_slot;
mod lsp_state;
mod lsp_subsystem;
mod lsp_ui;
mod lua_integration;
mod macros;
mod mark_jump;
mod marks;
pub(crate) mod motions;
mod navigation_state;
pub mod nucleo_matcher;
mod operators;
pub mod path_completion;
mod performance;
pub mod picker;
mod picker_manager;
pub mod picker_state;
mod quickfix;
mod register;
mod render_cache;
mod search_context;
mod search_manager;
mod single_line_input;
mod tab_manager;
mod tabpage;
mod test_runner;
mod theme;
mod theme_state;
mod toast;
mod ui_features;
mod ui_panels;
mod undo;
mod viewport_state;
mod visual_context;
mod visual_mode;
mod window;
mod window_viewport;
mod wrap_map;
mod yank_flash;

// Re-export sibling modules for backward compatibility
pub use crate::fold;
pub use crate::search;
pub use crate::textobjects;

pub use crate::change::{
    ApplyPos, Change, ChangeBuilder, ChangeManager, CursorPos, InsertEntryMode, Range,
    TextObjectType,
};
pub use ai_chat_commands::AiChatSlashCompletion;
pub use ai_chat_state::{
    AiChatActivity, ComprehensionPolicy, QueuedChatInput, QueuedChatInputKind,
};
pub use ai_shell_process::{ShellInspectorView, ShellProcessPhase};
pub use ai_state::{AiEditRegion, AiRegionStatus, CodexAuthDialogPhase, CodexAuthDialogSummary};
pub use ai_subagents::PreparedHeadlessAgentControl;
pub use build_state::PendingShellCommand;
pub use code_explanation::{
    CodeExplanationCardLayout, CodeExplanationDiscussionView, CodeExplanationPageView,
    CodeExplanationView, ConceptExplanationCardLayout,
};
pub use command_context::CommandContext;
pub use completion::CompletionMenu;
pub use editing_state::{EditingState, PendingChangeRepeat};
pub use filetree::{FileTree, FileTreeAction, FileTreeClipboardKind, TreeNode};
pub use fold::{Fold, FoldManager};
pub use input::mouse::handle_mouse_event;
pub use input::shell_expansion;
pub use input::InputHandler;
pub use input_context::InputContext;
pub use input_state::{CharMotion, InputState, TextObjectPrefix};
pub use keymap::{KeyMapManager, KeyMapping, MapMode};
pub use lsp_manager_panel::LspManagerPanel;
pub use lsp_state::{HoverContentType, LspIntents, LspResultType, LspState};
pub use lsp_ui::LspUi;
pub use macros::MacroManager;
pub use marks::{GlobalMark, JumpList, Mark, MarkManager, TagEntry, TagStack};
pub use motions::Motions;
pub use navigation_state::NavigationState;
pub use operators::Operator;
pub use path_completion::PathCompletionState;
pub use performance::{PerformanceMetrics, MAX_LATENCY_SAMPLES};
pub use picker::{Picker, PickerAction, PickerField, PickerMode, PickerResult};
pub use picker_state::PickerState;
pub use quickfix::{LocationList, QuickfixEntry, QuickfixEntryType, QuickfixList};
pub use register::{RegisterManager, RegisterType};
pub use render_cache::RenderCache;
pub use search::Search;
pub use search_context::{SearchContext, VisualSearchState};
pub use single_line_input::SingleLineInput;
pub use tabpage::{TabPage, TabPageManager};
pub use textobjects::{TextObjectRange, TextObjects};
pub use theme_state::ThemeState;
pub use toast::{Toast, ToastCenter, ToastLevel, ToastRequest, ToastSource};
pub use ui_panels::UiPanels;
pub use undo::UndoManager;
pub use viewport_state::ViewportState;
pub use visual_context::{VisualContext, VisualSelection};
pub use window::{SplitDirection, Window, WindowManager, WindowNode, WindowView, WindowViewNode};
pub use wrap_map::WrapMap;

/// Outcome of [`Editor::refresh_wrap_map`] — what the caller should do with the
/// wrap-map slot it owns (editor-global or per-`Window`).
enum WrapMapRefresh {
    /// Wrap is off — the slot should be cleared.
    Disable,
    /// The existing map (if any) is still valid — leave it as-is.
    UpToDate,
    /// A freshly built map, plus the `DecorationMap::generation` it reflects
    /// (the caller stores both).
    Rebuilt(WrapMap, u64),
}

/// Margin background color for textwidth shading
#[derive(Debug, Clone, PartialEq)]
pub enum MarginColor {
    /// No margin shading (default — preserves terminal transparency)
    None,
    /// Solid RGB color
    Solid(u8, u8, u8),
}

/// Controls LSP auto-install behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoInstallMode {
    /// Show a consent dialog before installing (default)
    #[default]
    Prompt,
    /// Install automatically without asking
    Auto,
    /// Never auto-install, only show install hints
    Off,
}

/// Editor options and settings
#[derive(Debug, Clone)]
pub struct EditorOptions {
    /// Width of tab character (default: 4)
    pub tab_width: usize,
    /// Number of spaces to use for autoindent (default: 4)
    pub shift_width: usize,
    /// Use spaces instead of tabs (default: true)
    pub expand_tab: bool,
    /// Show line numbers (default: false)
    pub number: bool,
    /// Show relative line numbers (default: false)
    pub relative_number: bool,
    /// Number of lines to scroll for half-page movements (default: None = calculate from viewport)
    pub scroll: Option<usize>,
    /// Maximum width of text content (default: None = use full terminal width)
    /// When set, content is centered horizontally with margins on both sides
    pub textwidth: Option<usize>,
    /// Highlight a vertical column at the specified column (default: None)
    /// Useful for keeping lines under a certain width
    pub colorcolumn: Option<usize>,
    /// Ignore case in search patterns (default: false)
    pub ignorecase: bool,
    /// Smart case: case-insensitive if pattern is all lowercase, case-sensitive otherwise (default: false)
    /// Only applies when ignorecase is also set
    pub smartcase: bool,
    /// Highlight the current line (default: false)
    pub cursorline: bool,
    /// Highlight matching brackets (default: true)
    pub showmatch: bool,
    /// Create swap files for crash recovery (default: true)
    pub swapfile: bool,
    /// Create backup files before saving (default: false)
    pub backup: bool,
    /// Minimum number of lines to keep above and below cursor (default: 10)
    pub scrolloff: usize,
    /// Wrap long lines (default: true)
    pub wrap: bool,
    /// Horizontal scroll step size (default: 0 = jump to center cursor)
    pub sidescroll: usize,
    /// Minimum columns to keep left and right of cursor (default: 5)
    pub sidescrolloff: usize,
    /// Clipboard mode: "unnamedplus" (default), "unnamed", or "" (vim-compatible)
    /// When set, yank/delete/paste use the system clipboard by default
    pub clipboard: String,
    /// Whether `-` key auto-reveals current file in the file tree (default: true)
    pub file_tree_reveal: bool,
    /// Show git blame gutter (default: false)
    pub blame: bool,
    /// Conceal markdown constructs (links, images) when rendering (default: true)
    pub markdown_conceal: bool,
    /// Background color for textwidth margins
    pub margin_color: MarginColor,
    /// Extra columns of normal background between text edge and shaded margin area (default: 0)
    pub margin_padding: usize,
    /// Program to run for :make (default: "cargo build")
    pub makeprg: String,
    /// LSP auto-install behavior: Prompt (default), Auto, or Off
    pub lsp_auto_install: AutoInstallMode,
    /// Request and render LSP inlay hints (default: false).
    /// Off by default while OV-00257 (wrap math) and OV-00258 (stale
    /// placement) are open. Toggle on with `:set inlay_hints` to opt in.
    pub inlay_hints: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            tab_width: 4,
            shift_width: 4,
            expand_tab: true,
            number: true,
            relative_number: false,
            scroll: None,
            textwidth: Some(150),
            colorcolumn: None,
            ignorecase: false,
            smartcase: false,
            cursorline: false,
            showmatch: true,
            swapfile: true,
            backup: false,
            scrolloff: 10,
            wrap: true,
            sidescroll: 0,
            sidescrolloff: 5,
            clipboard: "unnamedplus".to_string(),
            file_tree_reveal: true,
            blame: false,
            markdown_conceal: true,
            margin_color: MarginColor::None,
            margin_padding: 0,
            makeprg: "cargo build".to_string(),
            lsp_auto_install: AutoInstallMode::default(),
            inlay_hints: false,
        }
    }
}

use crate::buffer::Buffer;
#[cfg(feature = "lua")]
use crate::lua::LuaContext;
use crate::mode::Mode;
use crate::repeat_action::RepeatAction;
use crate::unicode::{grapheme_to_char_col, GraphemeCol};
use anyhow::Result;
use std::collections::HashMap;

/// Commands sent from background tasks to the LSP manager via channel
#[derive(Debug)]
pub enum LspCommand {
    /// Start a language server
    StartServer {
        language: String,
        command: String,
        args: Vec<String>,
        root_path: std::path::PathBuf,
        response_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Send didOpen notification
    DidOpen {
        uri: lsp_types::Uri,
        language_id: String,
        version: i32,
        text: String,
        response_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Start notification listener
    StartNotificationListener { language_id: String },
}

/// Cached preview highlights: line_idx -> Vec<(range, highlight_group)>
pub type PreviewHighlights =
    HashMap<usize, Vec<(std::ops::Range<usize>, crate::syntax::HighlightGroup)>>;

/// The main editor state
pub struct Editor {
    /// List of open buffers
    pub(crate) buffers: Vec<Buffer>,
    /// Index of the currently active buffer
    current_buffer_index: usize,
    /// Window manager for split windows
    window_manager: Option<WindowManager>,
    /// Current editing mode
    mode: Mode,
    /// Whether the editor should quit
    should_quit: bool,
    /// Exit code to use when quitting (0 = success, non-zero = error, used by :cq))
    exit_code: i32,
    /// Input context (counts, operators, pending commands, registers, input state machine)
    input: InputContext,
    /// Register manager for yank/delete operations
    registers: RegisterManager,
    /// Visual mode context (selection start, block insert state, last selection)
    visual: VisualContext,
    /// Command-line mode context (buffer, history, navigation)
    command: CommandContext,
    /// Search-related state
    pub search: SearchContext,
    /// Navigation state (marks, jump list, tag stack, find repeat)
    pub nav: NavigationState,
    /// Key mapping manager
    keymaps: KeyMapManager,
    /// Macro manager for recording and playback
    macro_manager: MacroManager,
    /// Picker state (picker, preview cache, layout, file list cache, etc.)
    pub picker_state: PickerState,
    /// LSP subsystem (state, commands, UI, install)
    pub(crate) lsp: lsp_subsystem::LspSubsystem,
    /// Lua context for configuration and plugins (optional)
    #[cfg(feature = "lua")]
    lua_context: Option<LuaContext>,
    /// Bridge for Lua-Editor communication (optional)
    #[cfg(feature = "lua")]
    editor_bridge: Option<crate::lua::EditorBridge>,
    /// Editing operation state (insert, replace, substitute, rename)
    pub editing: EditingState,
    /// Completion menu popup (LSP)
    completion_menu: CompletionMenu,
    /// Theme and color scheme state
    theme: ThemeState,
    /// Editor options and settings
    pub options: EditorOptions,
    /// Viewport and scroll state
    pub viewport: ViewportState,
    /// Tab page manager
    tab_page_manager: TabPageManager,
    /// Performance metrics
    metrics: PerformanceMetrics,
    /// Cached rendering state (mouse, layout geometry)
    pub render_cache: RenderCache,
    /// Transient yank flash highlight
    yank_flash: Option<yank_flash::YankFlash>,
    /// UI panels (file tree, quickfix, path completion, dashboard, diagnostic badge)
    pub ui_panels: UiPanels,
    /// DAP (Debug Adapter Protocol) manager for debug sessions
    dap_manager: crate::dap::DapManager,
    /// AI prompt, pending jobs, and in-buffer agent logs
    pub ai_state: Box<ai_state::AiState>,
    /// API server port (set during startup, used by :session start/stop)
    api_port: Option<u16>,
    /// Active session name (set by :session start, cleared by :session stop)
    active_session: Option<String>,
    /// Git branch name for the current file (if in a git repo)
    git_branch: Option<String>,
    /// Build/test subsystem state
    pub(crate) build: build_state::BuildState,
    /// Unified virtual text decorations (inlay hints, diagnostics, etc.)
    pub decorations: decoration::DecorationMap,
    /// Channel for receiving background git refresh results (status + blame)
    git_refresh_rx: tokio::sync::mpsc::Receiver<GitRefreshResult>,
    /// Sender half — cloned into spawn_blocking tasks after save
    pub(crate) git_refresh_tx: tokio::sync::mpsc::Sender<GitRefreshResult>,
}

/// Result of a background git status/blame refresh after save.
pub struct GitRefreshResult {
    pub path: String,
    pub status: Option<crate::git::GitStatus>,
    pub blame: Option<crate::git::GitBlame>,
}

/// Pending LSP server installation awaiting user consent
#[derive(Debug, Clone)]
pub struct PendingLspInstall {
    /// Human-readable language name (e.g. "Python")
    pub language_name: String,
    /// LSP server command (e.g. "pyright-langserver")
    pub server_command: String,
    /// How it will be installed (e.g. "npm install -g pyright")
    pub method_description: String,
    /// File path that triggered the install
    pub file_path: String,
}

/// User's response to the LSP install consent dialog
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspInstallConsent {
    /// Install this one time
    Yes,
    /// Set autoinstall=auto for all future installs
    Always,
    /// Skip this install
    No,
}

/// A background `:make` job waiting for results.
pub struct PendingMake {
    pub receiver: std::sync::mpsc::Receiver<MakeResult>,
    pub command: String,
}

/// Result from a `:make` background job.
pub struct MakeResult {
    pub output: String,
    pub success: bool,
}

/// Cached picker layout rects for mouse hit-testing
#[derive(Debug, Clone)]
pub struct PickerLayout {
    /// Search input area
    pub query_field: crate::Rect,
    /// File filter area (LiveGrep only)
    pub filter_field: Option<crate::Rect>,
    /// Results list area
    pub results_area: crate::Rect,
    /// Scroll offset of results (for mapping row to result index)
    pub results_scroll_offset: usize,
}

/// Tracks mouse interaction state for click and drag
#[derive(Debug, Clone, Default)]
pub struct MouseState {
    /// Whether a drag is in progress
    pub is_dragging: bool,
    /// Buffer position where the drag started (line, col)
    pub drag_origin: Option<(usize, usize)>,
}

/// State for tracking Replace mode for dot-repeat
#[derive(Clone, Debug)]
pub struct ReplaceModeState {
    /// Cursor position when R was pressed (grapheme-space).
    pub start_position: CursorPos,
    /// Characters typed during replace mode
    pub replacements: String,
    /// Original text that was overwritten
    pub old_text: String,
}

/// Cached preview data for the picker
#[derive(Clone)]
pub struct PreviewCache {
    /// File content
    pub content: String,
    /// Cached syntax-highlighted lines (line_idx -> highlights)
    /// Uses RefCell for interior mutability so we can cache highlights even with immutable reference
    pub highlighted_lines: std::cell::RefCell<PreviewHighlights>,
    /// Detected language (if any)
    pub language: Option<crate::syntax::Language>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindType {
    Find, // f/F - cursor lands on character
    Till, // t/T - cursor lands before/after character
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindDirection {
    Forward,
    Backward,
}

impl Editor {
    /// Creates a new editor with an empty buffer
    /// Starts in Dashboard mode when no file is opened
    pub fn new() -> Self {
        let buffer = Buffer::new();
        let (git_tx, git_rx) = tokio::sync::mpsc::channel(4);
        let mut editor = Self {
            buffers: vec![buffer],
            current_buffer_index: 0,
            window_manager: None, // Will be initialized when viewport size is known
            mode: Mode::Dashboard,
            should_quit: false,
            exit_code: 0,
            input: InputContext::new(),
            registers: RegisterManager::new(),
            visual: VisualContext::new(),
            command: CommandContext::new(),
            search: SearchContext::new(),
            nav: NavigationState::default(),
            keymaps: KeyMapManager::new(),
            macro_manager: MacroManager::new(),
            picker_state: PickerState::new(),
            lsp: lsp_subsystem::LspSubsystem::default(),
            #[cfg(feature = "lua")]
            lua_context: None,
            #[cfg(feature = "lua")]
            editor_bridge: None,
            editing: EditingState::default(),
            completion_menu: CompletionMenu::new(),
            theme: ThemeState::default(),
            options: EditorOptions::default(),
            viewport: ViewportState::default(),
            tab_page_manager: TabPageManager::new(),
            metrics: PerformanceMetrics::new(),
            render_cache: RenderCache::default(),
            yank_flash: None,
            ui_panels: UiPanels::default(),
            dap_manager: crate::dap::DapManager::new(),
            ai_state: Box::new(ai_state::AiState::default()),
            api_port: None,
            active_session: None,
            git_branch: None,
            build: build_state::BuildState::default(),
            decorations: decoration::DecorationMap::new(),
            git_refresh_rx: git_rx,
            git_refresh_tx: git_tx,
        };
        editor.ai_state.last_observed_buffer_version = editor.buffer().version();
        editor
    }

    /// Creates an editor with initial content
    pub fn with_content(content: &str) -> Self {
        let buffer = Buffer::new_from_str(content);
        let (git_tx, git_rx) = tokio::sync::mpsc::channel(4);
        let mut editor = Self {
            buffers: vec![buffer],
            current_buffer_index: 0,
            window_manager: None, // Will be initialized when viewport size is known
            mode: Mode::default(),
            should_quit: false,
            exit_code: 0,
            input: InputContext::new(),
            registers: RegisterManager::new(),
            visual: VisualContext::new(),
            command: CommandContext::new(),
            search: SearchContext::new(),
            nav: NavigationState::default(),
            keymaps: KeyMapManager::new(),
            macro_manager: MacroManager::new(),
            picker_state: PickerState::new(),
            lsp: lsp_subsystem::LspSubsystem::default(),
            #[cfg(feature = "lua")]
            lua_context: None,
            #[cfg(feature = "lua")]
            editor_bridge: None,
            editing: EditingState::default(),
            completion_menu: CompletionMenu::new(),
            theme: ThemeState::default(),
            options: EditorOptions::default(),
            viewport: ViewportState::default(),
            tab_page_manager: TabPageManager::new(),
            metrics: PerformanceMetrics::new(),
            render_cache: RenderCache::default(),
            yank_flash: None,
            ui_panels: UiPanels::default(),
            dap_manager: crate::dap::DapManager::new(),
            ai_state: Box::new(ai_state::AiState::default()),
            api_port: None,
            active_session: None,
            git_branch: None,
            build: build_state::BuildState::default(),
            decorations: decoration::DecorationMap::new(),
            git_refresh_rx: git_rx,
            git_refresh_tx: git_tx,
        };
        editor.ai_state.last_observed_buffer_version = editor.buffer().version();
        editor
    }

    // ==================== Rename Input ====================

    pub fn rename_buffer(&self) -> &str {
        self.editing.rename_input.text()
    }

    pub fn rename_cursor(&self) -> usize {
        self.editing.rename_input.cursor()
    }

    pub fn set_rename_buffer(&mut self, s: String) {
        self.editing.rename_input = SingleLineInput::new(s);
    }

    pub(crate) fn rename_input_mut(&mut self) -> &mut SingleLineInput {
        &mut self.editing.rename_input
    }

    // ==================== AI Prompt ====================

    pub fn ai_prompt_input(&self) -> &str {
        &self.ai_state.prompt.input
    }

    pub fn ai_prompt_cursor(&self) -> usize {
        self.ai_state.prompt.cursor
    }

    // ==================== Core Editor Methods ====================

    pub fn start_macro_recording(&mut self, register: char) -> bool {
        self.macro_manager.start_recording(register)
    }

    /// Stops macro recording
    pub fn stop_macro_recording(&mut self) {
        self.macro_manager.stop_recording();
    }

    /// Records a key event in the current macro
    pub fn record_macro_event(&mut self, event: crate::KeyEvent) {
        self.macro_manager.record_event(event);
    }

    /// Returns whether currently recording a macro
    pub fn is_recording_macro(&self) -> bool {
        self.macro_manager.is_recording()
    }

    /// Gets the register being recorded
    pub fn recording_register(&self) -> Option<char> {
        self.macro_manager.recording_register()
    }

    /// Gets a macro by register for playback
    pub fn get_macro(&self, register: char) -> Option<&Vec<crate::KeyEvent>> {
        self.macro_manager.get_macro(register)
    }

    /// Sets the last played macro register (for @@)
    pub fn set_last_played_macro(&mut self, register: char) {
        self.macro_manager.set_last_played(register);
    }

    /// Gets the last played macro register (for @@)
    pub fn last_played_macro(&self) -> Option<char> {
        self.macro_manager.last_played()
    }

    /// Gets the current mode
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Sets the mode
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        // Clear count and pending operator when changing modes
        self.input.count = None;
        self.input.pending_operator = None;
        self.input.pending_command = None;
        self.input.pending_register = None;
        self.input.pending_mapping_sequence.clear();
        self.input.pending_mapping_events.clear();

        // Clear visual selection when leaving visual modes
        if !matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock) {
            self.visual.visual_start = None;
        }
        if mode != Mode::AiPrompt {
            self.ai_state.prompt.model_picker_open = false;
        }
    }

    /// Gets the dashboard selected menu index
    pub fn dashboard_selected(&self) -> usize {
        self.ui_panels.dashboard_selected
    }

    /// Sets the dashboard selected menu index
    pub fn set_dashboard_selected(&mut self, index: usize) {
        self.ui_panels.dashboard_selected = index;
    }

    /// Returns true if the dashboard should be shown
    /// Dashboard is shown when: no file loaded AND buffer is empty/default
    pub fn should_show_dashboard(&self) -> bool {
        self.mode == Mode::Dashboard
    }

    /// Returns a mutable reference to the cat animation (if active).
    pub fn cat_animation_mut(
        &mut self,
    ) -> Option<&mut Box<dyn crate::dashboard::DashboardAnimation>> {
        self.ui_panels.cat_animation.as_mut()
    }

    /// Startle the cat (e.g. on terminal resize while it's on the logo).
    pub fn startle_cat(&mut self) {
        if let Some(ref mut anim) = self.ui_panels.cat_animation {
            anim.startle();
        }
    }

    /// Set a yank flash for a linewise region (e.g. `yy`, `yj`, `yk`).
    pub fn set_yank_flash_lines(&mut self, start_line: usize, end_line: usize) {
        self.yank_flash = Some(yank_flash::YankFlash::lines(start_line, end_line));
    }

    /// Set a yank flash for a character-wise region (e.g. `yw`, `y$`).
    pub fn set_yank_flash_range(
        &mut self,
        start_line: usize,
        start_col: GraphemeCol,
        end_line: usize,
        end_col: GraphemeCol,
    ) {
        self.yank_flash = Some(yank_flash::YankFlash::range(
            start_line,
            start_col.0,
            end_line,
            end_col.0,
        ));
    }

    /// Get a reference to the current yank flash (if any).
    pub fn yank_flash(&self) -> Option<&yank_flash::YankFlash> {
        self.yank_flash.as_ref()
    }

    /// Get the last make/test output (if any).
    pub fn last_make_output(&self) -> Option<&str> {
        self.build.last_make_output.as_deref()
    }

    /// Take a pending shell command (if any) for the event loop to execute.
    pub fn take_pending_shell_command(&mut self) -> Option<build_state::PendingShellCommand> {
        self.build.pending_shell_command.take()
    }

    /// Get the API server port.
    pub fn api_port(&self) -> Option<u16> {
        self.api_port
    }

    /// Set the API server port.
    pub fn set_api_port(&mut self, port: u16) {
        self.api_port = Some(port);
    }

    /// Get the active session name.
    pub fn active_session(&self) -> Option<&str> {
        self.active_session.as_deref()
    }

    /// Set the active session name.
    pub fn set_active_session(&mut self, name: String) {
        self.active_session = Some(name);
    }

    /// Take the active session name, leaving None.
    pub fn take_active_session(&mut self) -> Option<String> {
        self.active_session.take()
    }

    /// Set a pending LSP install awaiting user consent.
    pub fn set_pending_lsp_install(&mut self, install: PendingLspInstall) {
        self.lsp.pending_install = Some(install);
    }

    /// Check if there's an approved LSP install ready for the event loop.
    pub fn has_approved_lsp_install(&self) -> bool {
        self.lsp.approved_install.is_some()
    }

    /// Tick the yank flash. Returns true if it just expired (needs redraw to clear).
    pub fn tick_yank_flash(&mut self) -> bool {
        if let Some(ref flash) = self.yank_flash {
            if flash.is_expired() {
                self.yank_flash = None;
                return true;
            }
        }
        false
    }

    /// Tick transient toasts. Returns true if any expired toast was removed.
    pub fn tick_toasts(&mut self) -> bool {
        self.ui_panels.toast_center.prune_expired()
    }

    /// Tick the cat animation. Returns true if a frame advanced (needs redraw).
    pub fn tick_cat_animation(&mut self) -> bool {
        if let Some(ref mut anim) = self.ui_panels.cat_animation {
            if anim.is_active() {
                return anim.tick();
            }
            // Animation finished — drop it
            self.ui_panels.cat_animation = None;
        }
        false
    }

    /// Advance the AI chat working spinner. Returns true when its visible
    /// frame changed and the TUI needs another render without user input.
    pub fn tick_ai_chat_working_animation(&mut self) -> bool {
        if self.mode() != Mode::AiChat || !self.ai_chat_waiting() {
            return false;
        }
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            / 80;
        if self.render_cache.ai_chat_working_animation_tick == tick {
            return false;
        }
        self.render_cache.ai_chat_working_animation_tick = tick;
        true
    }

    pub fn ai_chat_working_animation_frame(&self) -> usize {
        (self.render_cache.ai_chat_working_animation_tick % 8) as usize
    }

    /// Gets the pending command
    pub fn pending_command(&self) -> Option<char> {
        self.input.pending_command
    }

    /// Sets the pending command
    pub fn set_pending_command(&mut self, cmd: char) {
        self.input.pending_command = Some(cmd);
    }

    /// Gets the current input state (new state machine)
    pub fn input_state(&self) -> &InputState {
        &self.input.input_state
    }

    /// Sets the input state (new state machine)
    pub fn set_input_state(&mut self, state: InputState) {
        self.input.input_state = state;
    }

    /// Resets input state to Normal
    pub fn reset_input_state(&mut self) {
        self.input.input_state = InputState::Normal;
    }

    /// Sets the viewport height (called from UI layer)
    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport.viewport_height = height;
    }

    /// Caches the buffer layout from the last render (for mouse coordinate conversion)
    pub fn set_last_layout(
        &mut self,
        buffer_area: crate::Rect,
        gutter_width: usize,
        text_width: usize,
        blame_width: usize,
    ) {
        self.render_cache.last_buffer_area = Some(buffer_area);
        self.render_cache.last_gutter_width = gutter_width;
        self.render_cache.last_text_width = text_width;
        self.render_cache.last_blame_width = blame_width;
    }

    /// Gets the viewport height
    pub fn viewport_height(&self) -> usize {
        self.viewport.viewport_height
    }

    /// Gets the scroll offset (top visible line)
    pub fn scroll_offset(&self) -> usize {
        // If we have a window manager, use the focused window's scroll offset
        // This allows viewport commands (zz, zt, zb) to control scrolling
        if let Some(wm) = &self.window_manager {
            if let Some(window) = wm.focused_window() {
                return window.scroll_offset();
            }
        }
        // Fall back to editor-level scroll offset for headless/test mode
        self.viewport.scroll_offset
    }

    /// Gets the visual sub-row offset within the top visible logical line.
    ///
    /// Mirrors [`scroll_offset`](Self::scroll_offset): the focused window's value
    /// when there's a window manager, otherwise the editor-global fallback.
    /// Always 0 when soft wrap is off.
    pub fn scroll_subrow(&self) -> usize {
        if let Some(wm) = &self.window_manager {
            if let Some(window) = wm.focused_window() {
                return window.scroll_subrow();
            }
        }
        self.viewport.scroll_subrow
    }

    /// Gets a reference to the wrap map for the active viewport (the focused
    /// window when there's a window manager, otherwise the editor-global slot
    /// used in headless / no-window-manager contexts).
    ///
    /// This is the map the cursor overlay and focused-pane content render
    /// against; non-focused split panes use their own via `Window::wrap_map`.
    pub fn wrap_map(&self) -> Option<&WrapMap> {
        match self
            .window_manager
            .as_ref()
            .and_then(|wm| wm.focused_window())
        {
            Some(window) => window.wrap_map(),
            None => self.viewport.wrap_map.as_ref(),
        }
    }

    /// Ensures the active viewport's wrap map is built and up-to-date for
    /// `text_width` columns — the focused window's map when there's a window
    /// manager, otherwise the editor-global `ViewportState::wrap_map`. Called
    /// from the rendering layer before drawing wrapped lines.
    pub fn ensure_wrap_map(&mut self, text_width: usize) {
        if let Some(focused_idx) = self
            .window_manager
            .as_ref()
            .map(|wm| wm.focused_window_index())
        {
            self.ensure_wrap_map_for_window(focused_idx, text_width);
            return;
        }
        // No window manager (headless / test harness) — editor-global slot.
        match self.refresh_wrap_map(
            text_width,
            self.viewport.wrap_map.as_ref(),
            self.viewport.wrap_decoration_generation,
        ) {
            WrapMapRefresh::Disable => self.viewport.wrap_map = None,
            WrapMapRefresh::UpToDate => {}
            WrapMapRefresh::Rebuilt(map, dec_gen) => {
                self.viewport.wrap_map = Some(map);
                self.viewport.wrap_decoration_generation = dec_gen;
            }
        }
    }

    /// Ensures window `window_idx`'s own wrap map is built and up-to-date for a
    /// viewport of `text_width` columns. No-op if there is no window manager or
    /// no such window. (roadmap 19 — split panes wrap at their own width.)
    pub fn ensure_wrap_map_for_window(&mut self, window_idx: usize, text_width: usize) {
        // The window must exist.
        if self
            .window_manager
            .as_ref()
            .and_then(|wm| wm.get_window(window_idx))
            .is_none()
        {
            return;
        }
        let existing_dec_gen = self
            .window_manager
            .as_ref()
            .and_then(|wm| wm.get_window(window_idx))
            .map(|w| w.wrap_decoration_generation())
            .unwrap_or(0);
        // `refresh_wrap_map` only reads `self` (shared) — passing it a `&WrapMap`
        // borrowed from `self.window_manager` is fine: both are shared borrows.
        let refresh = self.refresh_wrap_map(
            text_width,
            self.window_manager
                .as_ref()
                .and_then(|wm| wm.get_window(window_idx))
                .and_then(|w| w.wrap_map()),
            existing_dec_gen,
        );
        if let Some(window) = self
            .window_manager
            .as_mut()
            .and_then(|wm| wm.get_window_mut(window_idx))
        {
            match refresh {
                WrapMapRefresh::Disable => {
                    window.invalidate_wrap_map();
                }
                WrapMapRefresh::UpToDate => {}
                WrapMapRefresh::Rebuilt(map, dec_gen) => {
                    *window.wrap_map_mut() = Some(map);
                    window.set_wrap_decoration_generation(dec_gen);
                }
            }
        }
    }

    /// Recompute a wrap map for a viewport of `text_width` columns against the
    /// current buffer + decorations, given the caller's existing map (if any)
    /// and the decoration generation it was built at. Pure w.r.t. `self`
    /// (shared borrow only) — the caller decides which slot to store the
    /// result in. Shared by `ensure_wrap_map` (editor-global slot) and
    /// `ensure_wrap_map_for_window` (per-`Window` slot).
    fn refresh_wrap_map(
        &self,
        text_width: usize,
        existing: Option<&WrapMap>,
        existing_dec_gen: u64,
    ) -> WrapMapRefresh {
        if !self.options.wrap {
            return WrapMapRefresh::Disable;
        }
        let width = text_width.max(1);
        let tab_width = self.options.tab_width;
        // Use the rope's raw line count (includes the trailing empty line after
        // a final `\n`) so the wrap map covers every valid cursor position.
        let line_count = self.buffer().rope().len_lines();
        let buf_version = self.buffer().version();
        let dec_gen = self.decorations.generation;
        // When markdown conceal is active, the renderer draws concealed (often
        // much shorter) text for every line *except* the cursor line, which it
        // reveals so editing isn't blind. The wrap map must mirror that exactly
        // or row counts (and therefore cursor row / scrolling) drift from what
        // is drawn. `conceal_cursor_line` is `Some(cursor_line)` only when
        // conceal affects layout, so plain buffers never rebuild on j/k.
        let conceal_active = self.options.markdown_conceal
            && self
                .buffer()
                .file_path()
                .map(|p| p.ends_with(".md"))
                .unwrap_or(false);
        let cursor_line = self.buffer().cursor().line();
        let conceal_cursor_line = if conceal_active {
            Some(cursor_line)
        } else {
            None
        };
        if let Some(map) = existing {
            if map.buffer_version() == buf_version
                && map.wrap_width() == width
                && map.line_count() == line_count
                && existing_dec_gen == dec_gen
                && map.conceal_cursor_line() == conceal_cursor_line
            {
                return WrapMapRefresh::UpToDate;
            }
        }

        let rope = self.buffer().rope();
        let edit_log = self.buffer().edit_log();
        // Feed the wrap walker the *visible* line content (terminator stripped),
        // matching what the renderer's `apply_inline_decorations` sees — without
        // this the walker treats a trailing `\n` as a 1-col character and
        // disagrees with the renderer about row counts at width boundaries.
        // (OV-00257) `display::line_content` is the shared "line content for a
        // `&Rope` holder" helper (mirrors `Buffer::line_text`).
        //
        // When conceal is active, replace links/images with their concealed
        // form on every line but the cursor line, mirroring the renderer
        // (`render_buffer` skips conceal on `is_cursor_line_for_conceal`).
        let make_line_text = |line_idx: usize| {
            let raw = crate::display::line_content(rope, line_idx);
            if conceal_active && line_idx != cursor_line {
                let spans = crate::markdown_conceal::scan_markdown_conceal(&raw);
                if spans.is_empty() {
                    raw
                } else {
                    crate::markdown_conceal::apply_conceal(&raw, &spans).text
                }
            } else {
                raw
            }
        };
        // Wrap-width calculation reads *projected* decoration widths so wrap
        // points match what the renderer will draw (decorations are immutable
        // after placement; the projection replays `edit_log` since each
        // decoration's `source_version` — see `decoration.rs`).
        let inline_widths = |line_idx: usize| {
            self.decorations
                .inline_decorations_for_line_projected(line_idx, rope, edit_log)
        };

        let mut map = WrapMap::new_with_decorations(
            line_count,
            width,
            tab_width,
            buf_version,
            make_line_text,
            inline_widths,
        );
        map.set_conceal_cursor_line(conceal_cursor_line);
        WrapMapRefresh::Rebuilt(map, dec_gen)
    }

    fn cursor_grapheme_to_char_col(&self, line_idx: usize, grapheme_col: GraphemeCol) -> usize {
        let line_text = self.buffer().line_text(line_idx).unwrap_or_default();
        grapheme_to_char_col(&line_text, grapheme_col).0
    }

    fn cursor_line_text(&self, line_idx: usize) -> String {
        self.buffer()
            .line_text(line_idx)
            .unwrap_or_default()
            .to_string()
    }

    /// Gets the horizontal scroll offset (leftmost visible column)
    pub fn horizontal_offset(&self) -> usize {
        if let Some(wm) = &self.window_manager {
            if let Some(window) = wm.focused_window() {
                return window.horizontal_offset();
            }
        }
        // Fall back to 0 for headless/test mode
        0
    }

    /// Updates scroll offset to keep cursor visible
    ///
    /// Uses scrolloff for comfortable cursor positioning during normal movements.
    /// Viewport commands (zt, zz, zb) can override this by requesting viewport preservation.
    pub fn update_scroll_offset(&mut self) {
        // Skip if viewport command just ran - it has full control over positioning
        if self.viewport.should_preserve_after_input() {
            return;
        }

        let cursor_line = self.buffer().cursor().line();
        let visible_lines = if let Some(wm) = &self.window_manager {
            if let Some(window) = wm.focused_window() {
                (window.height() as usize).max(1)
            } else {
                self.viewport.viewport_height.max(1)
            }
        } else {
            self.viewport.viewport_height.max(1)
        };
        let current_offset = self.scroll_offset();
        let max_line = self.buffer().line_count().saturating_sub(1);

        // Only use wrap-aware scrolling if the wrap map covers the current buffer.
        // After edits (e.g. `o` inserting a line) the map is stale until the next
        // render pass rebuilds it.  Using stale data causes cursor_to_visual to
        // return 0 for the new line, jumping the viewport to the top.
        //
        // Consult the *active* map via `wrap_map()` — the focused window owns its
        // map when there's a window manager (the common case); `self.viewport.
        // wrap_map` is only the no-window-manager fallback slot.
        let len_lines = self.buffer().rope().len_lines();
        let wrap_map_usable =
            self.options.wrap && self.wrap_map().is_some_and(|m| m.line_count() >= len_lines);

        // In wrap mode, each logical line can consume multiple visual rows. Clamping using
        // logical line counts can prevent scrolling far enough to reveal the final logical
        // lines when a wrapped line appears near EOF. Instead, derive the maximum scroll
        // offset from total visual rows.
        let wrap_width_known = self.wrap_map().is_some() || self.render_cache.last_text_width > 0;

        let max_scroll = if wrap_map_usable {
            self.wrap_map()
                .map(|m| Self::compute_wrap_max_scroll_offset(m, visible_lines, max_line))
                .unwrap_or_else(|| max_line.saturating_sub(visible_lines.saturating_sub(1)))
        } else if self.options.wrap && wrap_width_known {
            // Wrap enabled but map stale: allow scrolling all the way to the last logical line.
            // This prevents the viewport from getting "stuck" above EOF between the edit and
            // the next render pass (which rebuilds the wrap map).
            max_line
        } else {
            max_line.saturating_sub(visible_lines.saturating_sub(1))
        };

        // Clamp scrolloff so top and bottom margins don't overlap.
        // When scrolloff >= ceil(visible_lines/2), both margins would claim
        // the same lines, causing the viewport to oscillate on every movement.
        let scrolloff = self
            .options
            .scrolloff
            .min(visible_lines.saturating_sub(1) / 2);

        // Calculate new scroll offset (and, in wrap mode, the visual sub-row
        // offset within the top logical line).
        let current_subrow = self.scroll_subrow();
        let mut new_offset;
        let mut new_subrow = 0usize;
        // Set when the wrap path has already bounded the position in visual-row
        // space; skip the logical `max_scroll` clamp below, which knows nothing
        // about sub-rows and would desync `new_offset` from `new_subrow`.
        let mut visual_clamped = false;

        if wrap_map_usable {
            if let Some(wrap_map) = self.wrap_map() {
                // Wrap-aware scrolling: work in absolute visual rows.
                let line_text = self.cursor_line_text(cursor_line);
                let cursor_char_col =
                    self.cursor_grapheme_to_char_col(cursor_line, self.buffer().cursor().col());
                let disp_col = crate::display::char_col_to_display_col(
                    &line_text,
                    cursor_char_col,
                    self.options.tab_width,
                );
                let (cursor_visual_row, _) =
                    wrap_map.cursor_to_visual(cursor_line, disp_col, &line_text);
                let viewport_visual_start =
                    wrap_map.logical_to_visual(current_offset) + current_subrow;

                if cursor_visual_row < viewport_visual_start + scrolloff {
                    // Cursor above the top margin — scroll up. Begin at the start
                    // of the logical line containing the target row (conservative:
                    // a line boundary, matching long-standing behaviour).
                    let target_visual = cursor_visual_row.saturating_sub(scrolloff);
                    let (new_line, _) = wrap_map.visual_to_logical(target_visual);
                    new_offset = new_line;
                    new_subrow = 0;
                } else if cursor_visual_row + scrolloff >= viewport_visual_start + visible_lines {
                    // Cursor below the bottom margin — scroll down by exact visual
                    // rows, beginning *partway into* a wrapped line when needed so
                    // a logical line taller than the viewport stays fully reachable
                    // (its tail no longer falls off the bottom edge).
                    let total_visual = wrap_map.total_visual_lines();
                    let max_visual_start = total_visual.saturating_sub(visible_lines);
                    let target_visual = (cursor_visual_row + scrolloff + 1)
                        .saturating_sub(visible_lines)
                        .min(max_visual_start);
                    let (new_line, sub_line) = wrap_map.visual_to_logical(target_visual);
                    new_offset = new_line;
                    new_subrow = sub_line;
                    visual_clamped = true;
                } else {
                    // Cursor already visible — hold position, including the sub-row
                    // offset so a mid-line scroll isn't snapped back to the top.
                    new_offset = current_offset;
                    new_subrow = current_subrow;
                }
            } else {
                // Wrap enabled but no wrap map yet — use logical line fallback
                new_offset = Self::compute_logical_scroll_offset(
                    cursor_line,
                    current_offset,
                    visible_lines,
                    scrolloff,
                );
            }
        } else if self.options.wrap {
            // Wrap enabled but wrap map stale (e.g. immediately after inserting/removing
            // newlines). Do a cheap on-the-fly wrap-aware scroll calculation limited to
            // the current viewport region so cursor visibility stays correct until the
            // next render pass rebuilds the wrap map.
            new_offset = self.compute_fallback_wrap_scroll_offset(
                cursor_line,
                current_offset,
                visible_lines,
                scrolloff,
            );
        } else {
            new_offset = Self::compute_logical_scroll_offset(
                cursor_line,
                current_offset,
                visible_lines,
                scrolloff,
            );
        };

        // Clamp to max_scroll only when the viewport actually needs to move for
        // cursor visibility.  When the cursor is already visible the scroll paths
        // return `current_offset` unchanged — clamping that would snap away the
        // deliberate positioning set by viewport commands (zt/zz/zb) near EOF.
        // The wrap down-scroll path has already clamped in visual-row space.
        new_offset = if !visual_clamped && new_offset != current_offset {
            new_offset.min(max_scroll)
        } else {
            new_offset
        };

        // Update both editor-level and window-level scroll offsets
        self.viewport.scroll_offset = new_offset;
        self.viewport.scroll_subrow = new_subrow;

        // Extract cursor column and options before mutably borrowing window_manager
        // Convert char column to display column for proper horizontal scrolling
        let cursor_line = self.buffer().cursor().line();
        let tab_width = self.options.tab_width;
        let cursor_char_col =
            self.cursor_grapheme_to_char_col(cursor_line, self.buffer().cursor().col());
        let cursor_display_col = {
            let line_text = self.buffer().line_text(cursor_line).unwrap_or_default();
            let raw_col =
                crate::display::char_col_to_display_col(&line_text, cursor_char_col, tab_width);
            // Include inline decoration widths (inlay hints) so horizontal
            // scroll keeps the *decorated* cursor position visible.  Without
            // this, h_offset is set from raw text only, but the renderer adds
            // decoration widths to the cursor, causing it to float right.
            raw_col
                + self.decorations.inline_width_before_projected(
                    cursor_line,
                    cursor_char_col,
                    self.buffer().rope(),
                    self.buffer().edit_log(),
                )
        };
        let wrap = self.options.wrap;
        let sidescroll = self.options.sidescroll;
        let sidescrolloff = self.options.sidescrolloff;

        if let Some(wm) = &mut self.window_manager {
            if let Some(window) = wm.focused_window_mut() {
                window.set_scroll_position(new_offset, new_subrow);

                // Update horizontal scroll offset to keep cursor visible horizontally
                if window.ensure_cursor_visible_horizontal(
                    cursor_display_col,
                    wrap,
                    sidescroll,
                    sidescrolloff,
                ) {
                    // Horizontal offset changed, mark for re-render
                    self.mark_dirty();
                }
            }
        }
    }

    /// Computes scroll offset using logical line counting (non-wrap path).
    /// Caller is responsible for clamping scrolloff so top/bottom margins
    /// don't overlap (scrolloff <= (visible_lines - 1) / 2).
    fn compute_logical_scroll_offset(
        cursor_line: usize,
        current_offset: usize,
        visible_lines: usize,
        scrolloff: usize,
    ) -> usize {
        if cursor_line < current_offset + scrolloff {
            cursor_line.saturating_sub(scrolloff)
        } else if cursor_line + scrolloff >= current_offset + visible_lines {
            cursor_line + scrolloff + 1 - visible_lines
        } else {
            current_offset
        }
    }

    /// Compute the maximum logical scroll offset in wrap mode based on total visual rows.
    ///
    /// The renderer can only start rendering at a logical line boundary (not a wrapped
    /// sub-line), so if the ideal max visual start lands mid-line, we advance to the
    /// next logical line to ensure the final logical lines can still be reached.
    fn compute_wrap_max_scroll_offset(
        wrap_map: &WrapMap,
        visible_rows: usize,
        max_line: usize,
    ) -> usize {
        let visible_rows = visible_rows.max(1);
        let total_visual = wrap_map.total_visual_lines();
        if total_visual <= visible_rows {
            return 0;
        }
        let max_visual_start = total_visual - visible_rows;
        let (line, sub_line) = wrap_map.visual_to_logical(max_visual_start);
        let candidate = if sub_line > 0 {
            line.saturating_add(1)
        } else {
            line
        };
        candidate.min(max_line)
    }

    fn compute_fallback_wrap_scroll_offset(
        &self,
        cursor_line: usize,
        current_offset: usize,
        visible_rows: usize,
        scrolloff: usize,
    ) -> usize {
        let visible_rows = visible_rows.max(1);
        let scrolloff = scrolloff.min(visible_rows.saturating_sub(1) / 2);

        let wrap_width = if let Some(map) = self.viewport.wrap_map.as_ref() {
            map.wrap_width()
        } else if self.render_cache.last_text_width > 0 {
            self.render_cache.last_text_width
        } else {
            // No reliable wrap width in headless/test mode — fall back to logical scrolling.
            return Self::compute_logical_scroll_offset(
                cursor_line,
                current_offset,
                visible_rows,
                scrolloff,
            );
        }
        .max(1);
        let tab_width = self.options.tab_width;

        let line_count = self.buffer().line_count();
        let max_line = line_count.saturating_sub(1);

        // If cursor is logically above the viewport, scroll to it.
        if cursor_line < current_offset {
            return cursor_line;
        }

        // Compute cursor sub-line within its logical line.
        let line_text = self
            .buffer()
            .line_text(cursor_line)
            .unwrap_or_default()
            .to_string();
        let cursor_char_col = grapheme_to_char_col(&line_text, self.buffer().cursor().col());
        let cursor_display_col =
            crate::display::char_col_to_display_col(&line_text, cursor_char_col.0, tab_width);
        let rope = self.buffer().rope();
        let edit_log = self.buffer().edit_log();
        let cursor_inline_widths =
            self.decorations
                .inline_decorations_for_line_projected(cursor_line, rope, edit_log);
        let cursor_display_col = cursor_display_col
            + self.decorations.inline_width_before_projected(
                cursor_line,
                cursor_char_col.0,
                rope,
                edit_log,
            );
        let cursor_subline = Self::cursor_subline_in_wrapped_line(
            &line_text,
            cursor_display_col,
            wrap_width,
            tab_width,
            &cursor_inline_widths,
        );

        // Fast path: if cursor is logically far below the viewport, just position it near bottom.
        let logical_view_end = current_offset + visible_rows.saturating_sub(1);
        if cursor_line > logical_view_end {
            let rows_above_cursor = visible_rows.saturating_sub(scrolloff + 1);
            return Self::top_offset_for_wrapped_cursor(
                self,
                cursor_line,
                cursor_subline,
                rows_above_cursor,
                wrap_width,
                tab_width,
                true,
            )
            .min(max_line);
        }

        // Cursor is logically within the viewport — check if it is visually within.
        let mut rows_from_top = 0usize;
        for line in current_offset..cursor_line {
            let text = self
                .buffer()
                .line_text(line)
                .unwrap_or_default()
                .to_string();
            rows_from_top += crate::wrap::visual_line_count(&text, wrap_width, tab_width);
            if rows_from_top > visible_rows + scrolloff + 5 {
                break;
            }
        }
        rows_from_top += cursor_subline;

        if rows_from_top < scrolloff {
            // Scroll up so cursor lands at scrolloff from top.
            Self::top_offset_for_wrapped_cursor(
                self,
                cursor_line,
                cursor_subline,
                scrolloff,
                wrap_width,
                tab_width,
                false,
            )
            .min(max_line)
        } else if rows_from_top + scrolloff >= visible_rows {
            // Scroll down so cursor lands at (visible_rows - scrolloff - 1).
            let rows_above_cursor = visible_rows.saturating_sub(scrolloff + 1);
            Self::top_offset_for_wrapped_cursor(
                self,
                cursor_line,
                cursor_subline,
                rows_above_cursor,
                wrap_width,
                tab_width,
                true,
            )
            .min(max_line)
        } else {
            current_offset
        }
    }

    fn cursor_subline_in_wrapped_line(
        line_text: &str,
        cursor_display_col: usize,
        wrap_width: usize,
        tab_width: usize,
        inline_widths: &[(usize, usize)],
    ) -> usize {
        let wrap_points = crate::wrap::compute_wrap_points_with_decorations(
            line_text,
            wrap_width,
            tab_width,
            inline_widths,
        );
        if wrap_points.is_empty() {
            return 0;
        }

        let mut current_display = 0usize;
        let mut segment_start_display = 0usize;
        let mut sub_line = 0usize;
        let mut wp_idx = 0usize;

        for (char_idx, ch) in line_text.chars().enumerate() {
            // Consume all wrap points at this char_idx (there can be multiple
            // when a decoration spans several visual rows).
            while wp_idx < wrap_points.len() && char_idx == wrap_points[wp_idx] {
                segment_start_display = current_display;
                sub_line += 1;
                wp_idx += 1;
            }
            if cursor_display_col < current_display {
                break;
            }
            let ch_width = if ch == '\t' {
                tab_width - (current_display % tab_width)
            } else {
                crate::display::char_display_width(ch)
            };
            current_display += ch_width;
        }

        if cursor_display_col >= segment_start_display {
            sub_line
        } else {
            0
        }
    }

    fn top_offset_for_wrapped_cursor(
        &self,
        cursor_line: usize,
        cursor_subline: usize,
        rows_above_cursor: usize,
        wrap_width: usize,
        tab_width: usize,
        advance_if_mid_line: bool,
    ) -> usize {
        // If the target visual start would land within the cursor's own wrapped line,
        // we can't start mid-line, so start at the cursor's logical line.
        if rows_above_cursor <= cursor_subline {
            return cursor_line;
        }

        let mut remaining = rows_above_cursor.saturating_sub(cursor_subline);
        let mut line = cursor_line;

        while line > 0 {
            line -= 1;
            let text = self
                .buffer()
                .line_text(line)
                .unwrap_or_default()
                .to_string();
            let count = crate::wrap::visual_line_count(&text, wrap_width, tab_width);

            if remaining == 0 {
                return line;
            }

            if remaining < count {
                return if advance_if_mid_line && remaining > 0 {
                    line.saturating_add(1)
                } else {
                    line
                };
            }

            remaining = remaining.saturating_sub(count);
            if remaining == 0 {
                return line;
            }
        }

        0
    }

    /// Calculates half-page scroll amount (Ctrl-D / Ctrl-U).
    ///
    /// Uses `options.scroll` when set. Otherwise it's half the *focused window's*
    /// height — not the whole buffer area — so that in a split, Ctrl-D/U scroll
    /// half of the focused pane rather than half of the entire editor.
    pub fn half_page_scroll(&self) -> usize {
        if let Some(scroll) = self.options.scroll {
            return scroll;
        }
        let height = self
            .window_manager
            .as_ref()
            .and_then(|wm| wm.focused_window())
            .map(|window| window.height() as usize)
            .unwrap_or(self.viewport.viewport_height);
        (height / 2).max(1)
    }

    /// Clears the pending command
    pub fn clear_pending_command(&mut self) {
        self.input.pending_command = None;
    }

    /// Returns whether the editor should quit
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Sets the quit flag
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Quit with a specific exit code (used by :cq)
    pub fn quit_with_code(&mut self, code: i32) {
        self.should_quit = true;
        self.exit_code = code;
    }

    /// Returns the exit code (0 = success, non-zero = error)
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Gets the current count
    pub fn count(&self) -> Option<usize> {
        self.input.count
    }

    /// Sets the count
    pub fn set_count(&mut self, count: usize) {
        self.input.count = Some(count);
    }

    /// Appends a digit to the count
    pub fn append_count(&mut self, digit: usize) {
        let current = self.input.count.unwrap_or(0);
        self.input.count = Some(current * 10 + digit);
    }

    /// Clears the count
    pub fn clear_count(&mut self) {
        self.input.count = None;
    }

    /// Gets the effective count (count or 1)
    pub fn effective_count(&self) -> usize {
        self.input.count.unwrap_or(1)
    }

    /// Gets the pending operator
    pub fn pending_operator(&self) -> Option<Operator> {
        self.input.pending_operator
    }

    /// Sets the pending operator
    pub fn set_pending_operator(&mut self, op: Operator) {
        self.input.pending_operator = Some(op);
    }

    /// Clears the pending operator
    pub fn clear_pending_operator(&mut self) {
        self.input.pending_operator = None;
    }

    /// Gets a reference to the registers
    pub fn registers(&self) -> &RegisterManager {
        &self.registers
    }

    /// Gets a mutable reference to the registers
    pub fn registers_mut(&mut self) -> &mut RegisterManager {
        &mut self.registers
    }

    /// Gets a reference to the keymaps
    pub fn keymaps(&self) -> &KeyMapManager {
        &self.keymaps
    }

    /// Gets a mutable reference to the keymaps
    pub fn keymaps_mut(&mut self) -> &mut KeyMapManager {
        &mut self.keymaps
    }

    /// Returns true when normal-mode keymap matching is waiting for more input.
    pub fn has_pending_mapping(&self) -> bool {
        !self.input.pending_mapping_sequence.is_empty()
    }

    /// Returns the pending normal-mode mapping key sequence.
    pub fn pending_mapping_sequence(&self) -> &str {
        &self.input.pending_mapping_sequence
    }

    /// Appends one encoded key token to the pending mapping sequence.
    pub fn append_pending_mapping(&mut self, token: &str, event: crate::KeyEvent) {
        self.input.pending_mapping_sequence.push_str(token);
        self.input.pending_mapping_events.push(event);
    }

    /// Clears all pending mapping state.
    pub fn clear_pending_mapping(&mut self) {
        self.input.pending_mapping_sequence.clear();
        self.input.pending_mapping_events.clear();
    }

    /// Drains pending mapping events and clears the pending sequence.
    pub fn take_pending_mapping_events(&mut self) -> Vec<crate::KeyEvent> {
        self.input.pending_mapping_sequence.clear();
        std::mem::take(&mut self.input.pending_mapping_events)
    }

    /// Gets the pending register for next operation
    pub fn pending_register(&self) -> Option<char> {
        self.input.pending_register
    }

    /// Sets the pending register for next operation
    pub fn set_pending_register(&mut self, reg: char) {
        self.input.pending_register = Some(reg);
    }

    /// Clears the pending register
    pub fn clear_pending_register(&mut self) {
        self.input.pending_register = None;
    }

    /// Loads a file into the editor (async version)
    /// If the file is already open in a buffer, switches to that buffer
    /// Otherwise, adds it as a new buffer
    pub async fn load_file_async<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        // Check if file is already open in a buffer
        for (i, buf) in self.buffers.iter().enumerate() {
            if buf.file_path() == Some(&path_str) {
                // File already open - just switch to it
                // Save current file to alternate file register
                if let Some(current_path) = self.buffer().file_path() {
                    self.registers.set_alternate_file(current_path.to_string());
                }
                self.current_buffer_index = i;
                // Update current file register
                self.registers.set_current_file(path_str);
                // Sync tab's buffer index to match the existing buffer
                self.sync_current_tab_buffer_index();
                // Still need to initialize LSP for this file if it hasn't been yet
                self.lsp.state.needs_lsp_init = true;
                return Ok(());
            }
        }

        // Store old file path before loading new file
        let old_file_path = self.buffer().file_path().map(|s| s.to_string());

        // Save current file to alternate file register
        if let Some(current_path) = old_file_path.as_ref() {
            self.registers.set_alternate_file(current_path.to_string());
        }

        // Load new buffer
        let new_buffer = Buffer::load_file_async(path).await?;

        // Parse and apply modeline options from the loaded file
        let content = new_buffer.rope().to_string();
        if let Some(modeline) = crate::modeline::Modeline::parse(&content) {
            self.apply_modeline(&modeline);
        }

        // Load git branch name for the new file
        self.git_branch = new_buffer.file_path().and_then(crate::git::branch_name);

        self.add_buffer(new_buffer);

        // Update current file register
        self.registers.set_current_file(path_str);

        // Update tab title to match the loaded file
        self.update_current_tab_title();

        // Sync tab's buffer index to match the newly loaded buffer
        self.sync_current_tab_buffer_index();

        // Mark that we need to send didClose for the old file
        if old_file_path.is_some() {
            self.lsp.state.pending_did_close_file = old_file_path;
        }

        Ok(())
    }

    /// Loads a file into the editor (blocking wrapper around load_file_async)
    pub fn load_file<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.load_file_async(path))
        })
    }

    /// Process pending syntax re-highlighting (CPU-intensive, runs in background)
    /// This now uses incremental parsing - the parse tree was already updated incrementally
    /// via InputEdit when the buffer was modified, so we just need to query it for highlights.
    pub async fn process_pending_rehighlight(&mut self) {
        // Check if buffer needs re-highlighting
        if !self.buffer().needs_rehighlight() {
            return;
        }

        // Rebuild highlight cache from the incrementally-updated parse tree
        // This is FAST because tree-sitter already updated the tree via InputEdit.
        // We're just querying the tree for highlights, not re-parsing!
        let _ = self.buffer_mut().rebuild_highlight_cache();

        // Fix Bug 2: Mark dirty after highlighting update so the UI re-renders
        // Without this, highlighting updates after debounce but screen doesn't refresh
        self.mark_dirty();
    }

    /// Immediate viewport-only syntax rehighlight.
    /// Queries tree-sitter for just the visible lines and updates the cache.
    /// This is called immediately after input so highlights are accurate without waiting for the debounce.
    pub fn process_viewport_rehighlight(&mut self) {
        if !self.buffer().needs_rehighlight() {
            return;
        }

        let start_line = self.scroll_offset();
        let end_line = start_line + self.viewport_height();

        self.buffer_mut()
            .rebuild_viewport_highlight_cache(start_line, end_line);

        self.mark_dirty();
    }

    /// Apply modeline options to editor settings
    fn apply_modeline(&mut self, modeline: &crate::modeline::Modeline) {
        // Indentation options
        if let Some(ts) = modeline.get_int("tabstop", "ts") {
            self.options.tab_width = ts;
        }
        if let Some(sw) = modeline.get_int("shiftwidth", "sw") {
            self.options.shift_width = sw;
        }
        if let Some(et) = modeline.get_bool("expandtab", "et") {
            self.options.expand_tab = et;
        }

        // Display options
        if let Some(tw) = modeline.get_int("textwidth", "tw") {
            self.options.textwidth = Some(tw);
        }
        if let Some(nu) = modeline.get_bool("number", "nu") {
            self.options.number = nu;
        }
        if let Some(rnu) = modeline.get_bool("relativenumber", "rnu") {
            self.options.relative_number = rnu;
        }
        if let Some(cul) = modeline.get_bool("cursorline", "cul") {
            self.options.cursorline = cul;
        }

        // Search options
        if let Some(ic) = modeline.get_bool("ignorecase", "ic") {
            self.options.ignorecase = ic;
        }
        if let Some(scs) = modeline.get_bool("smartcase", "scs") {
            self.options.smartcase = scs;
        }

        // Other options
        if let Some(sm) = modeline.get_bool("showmatch", "sm") {
            self.options.showmatch = sm;
        }
    }

    /// Yanks text to the appropriate register (pending_register or default)
    pub fn yank_to_register(&mut self, text: String) {
        self.yank_to_register_with_type(text, RegisterType::Character);
    }

    /// Yanks text to the appropriate register with explicit type
    pub fn yank_to_register_with_type(&mut self, text: String, reg_type: RegisterType) {
        if let Some(reg) = self.input.pending_register {
            self.input.pending_register = None;
            match reg {
                '_' => return, // black hole register: discard
                r if RegisterManager::is_read_only(r) => {
                    // Read-only registers: silently use default behavior
                    self.registers.yank_with_type(text.clone(), reg_type);
                    if !self.options.clipboard.is_empty() {
                        self.registers.set_clipboard(text);
                    }
                    return;
                }
                '+' | '*' => {
                    self.registers.set_clipboard(text.clone());
                    self.registers.set_with_type(Some(reg), text, reg_type);
                    return;
                }
                _ => {
                    self.registers
                        .set_with_type(Some(reg), text.clone(), reg_type);
                    // Also update unnamed + yank register (Vim behavior)
                    self.registers.yank_with_type(text.clone(), reg_type);
                }
            }
        } else {
            self.registers.yank_with_type(text.clone(), reg_type);
        }
        // Sync to system clipboard when clipboard option is set and no explicit register was used
        if !self.options.clipboard.is_empty() {
            self.registers.set_clipboard(text);
        }
    }

    /// Deletes text and stores in the appropriate register (pending_register or default)
    pub fn delete_to_register(&mut self, text: String) {
        self.delete_to_register_with_type(text, RegisterType::Character);
    }

    /// Deletes text and stores in the appropriate register with explicit type
    pub fn delete_to_register_with_type(&mut self, text: String, reg_type: RegisterType) {
        if let Some(reg) = self.input.pending_register {
            self.input.pending_register = None;
            match reg {
                '_' => return, // black hole register: discard
                r if RegisterManager::is_read_only(r) => {
                    // Read-only registers: silently use default behavior
                    self.registers.delete_with_type(text.clone(), reg_type);
                    if !self.options.clipboard.is_empty() {
                        self.registers.set_clipboard(text);
                    }
                    return;
                }
                '+' | '*' => {
                    self.registers.set_clipboard(text.clone());
                    self.registers.set_with_type(Some(reg), text, reg_type);
                    return;
                }
                _ => {
                    self.registers
                        .set_with_type(Some(reg), text.clone(), reg_type);
                    // Also update unnamed + delete history (Vim behavior)
                    self.registers.delete_with_type(text.clone(), reg_type);
                }
            }
        } else {
            self.registers.delete_with_type(text.clone(), reg_type);
        }
        // Sync to system clipboard when clipboard option is set and no explicit register was used
        if !self.options.clipboard.is_empty() {
            self.registers.set_clipboard(text);
        }
    }

    /// Gets text from the appropriate register (pending_register or default)
    pub fn get_from_register(&mut self) -> String {
        let text = if let Some(reg) = self.input.pending_register {
            match reg {
                '_' => String::new(), // black hole register: always empty
                '+' | '*' => self.registers.get_clipboard(),
                _ => self.registers.get(Some(reg)),
            }
        } else if !self.options.clipboard.is_empty() {
            // When clipboard option is set, read from system clipboard
            self.registers.get_clipboard()
        } else {
            self.registers.get_default().to_string()
        };
        self.input.pending_register = None;
        text
    }

    /// Gets text and type from the appropriate register (pending_register or default)
    pub fn get_from_register_with_type(&mut self) -> (String, RegisterType) {
        let (text, reg_type) = if let Some(reg) = self.input.pending_register {
            match reg {
                '_' => (String::new(), RegisterType::Character), // black hole: always empty
                '+' | '*' => {
                    let clipboard_text = self.registers.get_clipboard();
                    (clipboard_text, RegisterType::Character)
                }
                _ => self.registers.get_with_type(Some(reg)),
            }
        } else if !self.options.clipboard.is_empty() {
            // When clipboard option is set, read from system clipboard
            // Use Character type since system clipboard doesn't carry type info
            let clipboard_text = self.registers.get_clipboard();
            // Check if the unnamed register has the same text - if so, use its type
            let (default_text, default_type) = self.registers.get_default_with_type();
            if default_text == clipboard_text {
                (clipboard_text, default_type)
            } else {
                // Clipboard has different content (from external paste), treat as character
                (clipboard_text, RegisterType::Character)
            }
        } else {
            let (t, rt) = self.registers.get_default_with_type();
            (t.to_string(), rt)
        };
        self.input.pending_register = None;
        (text, reg_type)
    }

    /// Handles a bracketed paste event (for all supported modes, including chat input).
    pub fn handle_paste_event(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.has_codex_auth_dialog() {
            return Ok(());
        }
        if self.mode() == Mode::AiChat && self.ai_chat_has_exa_setup_dialog() {
            self.insert_exa_setup_text(text);
            return Ok(());
        }
        if self.mode() == Mode::AiChat && self.try_attach_dropped_chat_images(text)? {
            return Ok(());
        }

        // Strip CR variants so pasted CRLF/CR content doesn't leave `^M`
        // artifacts in the rope (OV-00250). Cheap when there are no CRs.
        let normalized = crate::buffer::normalize_for_buffer(text);
        let text = normalized.as_ref();

        match self.mode() {
            Mode::Insert => {
                // Insert pasted text at cursor position. The ambient insert
                // session captures the edit; the undo entry is pushed as a
                // single `Recorded` at `finalize_change_building`.
                let cursor = self.buffer().cursor();
                // Convert grapheme col to char col for buffer operations.
                let char_col = self.buffer().cursor_char_col();
                let line = cursor.line();
                let pasted = text.to_string();
                self.record_session_edit(|buf| {
                    buf.insert_text_at_positioning_cursor(line, char_col, &pasted)
                });
            }
            Mode::AiChat => {
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    if matches!(chat.focus, crate::ai::chat_types::ChatFocus::TextInput) {
                        chat.input.insert_str(chat.input_cursor, text);
                        chat.input_cursor += text.len();
                    }
                }
            }
            Mode::Normal => {
                // Set unnamed register and paste after cursor as a recorded
                // undo entry. `.`-repeat runs through `RepeatAction::PasteAfter`
                // so the re-paste re-anchors to the current cursor (same
                // semantics the old `Change::InsertText::repeat` provided).
                self.registers.set(None, text.to_string());
                let cursor = self.buffer().cursor();
                let cursor_before = CursorPos::new(cursor.line(), cursor.col());
                // Convert grapheme col to char col for buffer operations.
                let char_col = self.buffer().cursor_char_col();
                let line = cursor.line();
                let paste_col = char_col + 1;
                let pasted = text.to_string();
                let (_mutated, edits) = self
                    .buffer_mut()
                    .record(|buf| buf.insert_text_at_positioning_cursor(line, paste_col, &pasted));
                if !edits.is_empty() {
                    let cursor_after = self.cursor_position();
                    self.push_recorded_undo(edits, cursor_before, cursor_after);
                    self.set_repeat_action(crate::repeat_action::RepeatAction::PasteAfter {
                        count: 1,
                    });
                }
            }
            Mode::Command => {
                // Insert text into command buffer
                self.insert_into_command_line(text);
            }
            Mode::Search => {
                // Insert text into search buffer
                self.insert_into_search_buffer(text);
            }
            Mode::Picker => {
                if let Some(picker) = self.picker_mut() {
                    picker.insert_text(text);
                }
                self.mark_picker_query_changed();
            }
            _ => {
                // Visual modes: treat like normal mode paste
                self.registers.set(None, text.to_string());
            }
        }
        Ok(())
    }

    /// Starts building a composite change (e.g., when entering insert mode)
    pub fn start_change_building(&mut self, cursor_before: CursorPos) {
        self.buffer_mut()
            .change_manager_mut()
            .start_building(cursor_before);
        // Open the stateful recording session that `finalize_change_building`
        // will close — this feeds `RepeatAction::InsertSession` alongside the
        // `Recorded` undo entry. Session origin is captured lazily on the first
        // edit via `record_edit`.
        if !self.buffer().is_recording() {
            self.buffer_mut().begin_recording();
        }
    }

    /// Sets how insert mode was entered on the current change builder (for dot repeat).
    pub fn set_change_entry_mode(&mut self, mode: InsertEntryMode) {
        self.buffer_mut().change_manager_mut().set_entry_mode(mode);
    }

    /// Adds a change to the change manager
    pub fn add_change(&mut self, change: Change) {
        self.buffer_mut().change_manager_mut().add_change(change);
    }

    /// Finalizes the current insert-session change.
    ///
    /// Closes the stateful recording session and, if it produced edits,
    /// pushes a single `Change::Recorded` as the session's undo entry.
    /// Also installs a `RepeatAction::InsertSession` for dot-repeat. The
    /// legacy `ChangeBuilder`-built `Composite` is no longer produced here;
    /// the builder's role is now purely to carry `entry_mode` and
    /// `cursor_before` across the session.
    pub fn finalize_change_building(&mut self) {
        let cursor_after =
            CursorPos::new(self.buffer().cursor().line(), self.buffer().cursor().col());

        // Read session metadata from the builder, then drop it — no more
        // Pattern A Composite.
        let (cursor_before, entry_mode) = match self
            .buffer_mut()
            .change_manager_mut()
            .current_builder
            .take()
        {
            Some(builder) => (builder.cursor_before(), builder.entry_mode().clone()),
            None => {
                // No active session — still make sure recording is closed so
                // a leaked `begin_recording` doesn't trip later record() calls.
                let _ = self.buffer_mut().end_recording();
                return;
            }
        };

        let origin = self.buffer().recording_origin();
        let origin_cursor = self.buffer().recording_origin_cursor();
        let edits = self.buffer_mut().end_recording();
        if edits.is_empty() {
            return;
        }

        // Push the session as a mechanical-undo `Recorded` entry. The
        // `edit_start` override makes `g;` land at the first-edit cursor
        // (post-entry-mode) rather than the pre-entry-mode `cursor_before`
        // that undo restores to.
        let change = match origin_cursor {
            Some(edit_start) => Change::recorded_with_edit_start(
                edits.clone(),
                cursor_before,
                cursor_after,
                edit_start,
            ),
            None => Change::recorded(edits.clone(), cursor_before, cursor_after),
        };
        self.buffer_mut().change_manager_mut().push_change(change);

        // Install dot-repeat. push_change above cleared last_repeat_action;
        // set InsertSession now.
        if let Some(origin_offset) = origin {
            let cm = self.buffer_mut().change_manager_mut();
            cm.last_repeat_action = Some(RepeatAction::InsertSession {
                entry_mode,
                origin_offset,
                edits,
            });
        }
    }

    /// Sets a pending change repeat (for cc, C, s, cj, etc. dot-repeat)
    pub fn set_pending_change_repeat(&mut self, pending: PendingChangeRepeat) {
        self.editing.pending_change_repeat = Some(pending);
    }

    /// Takes and clears the pending change repeat
    pub fn take_pending_change_repeat(&mut self) -> Option<PendingChangeRepeat> {
        self.editing.pending_change_repeat.take()
    }

    /// Sets pending visual-block change repeat payload (line_count, width).
    pub fn set_pending_visual_block_change_repeat(&mut self, pending: Option<(usize, usize)>) {
        self.editing.pending_visual_block_change_repeat = pending;
    }

    /// Takes and clears pending visual-block change repeat payload.
    pub fn take_pending_visual_block_change_repeat(&mut self) -> Option<(usize, usize)> {
        self.editing.pending_visual_block_change_repeat.take()
    }

    /// Gets the leader key (default: space)
    pub fn leader_key(&self) -> char {
        self.input.leader_key
    }

    /// Sets the leader key
    pub fn set_leader_key(&mut self, key: char) {
        self.input.leader_key = key;
    }

    /// Gets the git branch name for the current file
    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns whether macro playback should abort (a motion failed to move).
    pub fn macro_aborted(&self) -> bool {
        self.macro_manager.aborted()
    }

    /// Signal that a motion failed (cursor didn't move), aborting macro playback.
    pub fn signal_macro_abort(&mut self) {
        self.macro_manager.signal_abort();
    }

    /// Clear the macro abort flag.
    pub fn clear_macro_abort(&mut self) {
        self.macro_manager.clear_abort();
    }

    /// Gets cached diagnostic count (sync, suitable for UI rendering)
    pub fn cached_diagnostic_count(&self) -> (usize, usize, usize, usize) {
        if self.diagnostics_cache_stale() {
            return (0, 0, 0, 0);
        }
        self.lsp.state.diagnostic_count
    }

    /// Whether the diagnostic badge has been dismissed by double-Escape
    pub fn diagnostic_badge_dismissed(&self) -> bool {
        self.ui_panels.diagnostic_badge_dismissed
    }

    /// Dismiss the diagnostic badge (called on double-Escape)
    pub fn dismiss_diagnostic_badge(&mut self) {
        self.ui_panels.diagnostic_badge_dismissed = true;
    }

    /// Called when diagnostic counts change to potentially un-dismiss the badge
    pub fn on_diagnostic_counts_changed(&mut self, errors: usize, warnings: usize) {
        let new_count = (errors, warnings);
        if new_count != self.ui_panels.diagnostic_badge_last_count {
            self.ui_panels.diagnostic_badge_last_count = new_count;
            self.ui_panels.diagnostic_badge_dismissed = false;
        }
    }

    /// Set the latest user-facing editor status message.
    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.ui_panels.status_message = message.into();
        self.mark_dirty();
    }

    /// Return the latest user-facing editor status message.
    pub fn status_message(&self) -> &str {
        &self.ui_panels.status_message
    }

    /// Clear the current user-facing editor status message.
    pub fn clear_status_message(&mut self) {
        self.set_status_message(String::new());
    }

    /// Push a toast notification into the top-right toast center.
    pub fn push_toast(&mut self, request: ToastRequest) -> u64 {
        let id = self.ui_panels.toast_center.push(request);
        self.mark_dirty();
        id
    }

    /// Returns true if there is at least one visible toast.
    pub fn has_visible_toasts(&self) -> bool {
        self.ui_panels.toast_center.has_visible()
    }

    /// Returns visible toasts ordered newest-first.
    pub fn visible_toasts_newest_first(&self, max: usize) -> Vec<Toast> {
        self.ui_panels.toast_center.visible_toasts_newest_first(max)
    }

    /// Dismiss the newest visible toast, if any.
    pub fn dismiss_latest_toast(&mut self) -> bool {
        let dismissed = self.ui_panels.toast_center.dismiss_latest_visible();
        if dismissed {
            self.mark_dirty();
        }
        dismissed
    }

    /// Returns true if either diagnostics badge or toast overlay has visible content.
    pub fn has_top_right_overlay(&self) -> bool {
        let (errors, warnings, _, _) = self.cached_diagnostic_count();
        let diagnostic_visible = !self.diagnostic_badge_dismissed() && (errors > 0 || warnings > 0);
        diagnostic_visible || self.has_visible_toasts()
    }

    /// Dismiss one top-right overlay item (newest toast first, then diagnostic badge).
    pub fn dismiss_top_right_overlay(&mut self) -> bool {
        if self.dismiss_latest_toast() {
            return true;
        }

        let (errors, warnings, _, _) = self.cached_diagnostic_count();
        let diagnostic_visible = !self.diagnostic_badge_dismissed() && (errors > 0 || warnings > 0);
        if diagnostic_visible {
            self.dismiss_diagnostic_badge();
            return true;
        }

        false
    }

    /// Get last escape time for double-Escape detection
    pub fn last_escape_time(&self) -> Option<std::time::Instant> {
        self.ui_panels.last_escape_time
    }

    /// Set last escape time
    pub fn set_last_escape_time(&mut self, time: std::time::Instant) {
        self.ui_panels.last_escape_time = Some(time);
    }

    /// Clear last escape time
    pub fn clear_last_escape_time(&mut self) {
        self.ui_panels.last_escape_time = None;
    }

    /// Gets a reference to the last change
    pub fn last_change(&self) -> Option<&Change> {
        self.buffer().change_manager().last_change()
    }

    /// Jump to next diagnostic (]d)
    pub fn goto_next_diagnostic(&mut self) {
        let current_line = self.buffer().cursor().line();
        let current_col = self.buffer().cursor().col();
        let current_col_utf16 = self.col_to_utf16(current_line, current_col.0);
        let diagnostics = &self.lsp.state.current_file_diagnostics;

        // Find first diagnostic after current position (compare line, then column)
        let next = diagnostics
            .iter()
            .filter(|d| {
                let dl = d.range.start.line as usize;
                dl > current_line
                    || (dl == current_line && d.range.start.character > current_col_utf16)
            })
            .min_by_key(|d| (d.range.start.line, d.range.start.character));

        let target = next
            .or_else(|| diagnostics.first())
            .map(|d| (d.range.start.line as usize, d.range.start.character));

        if let Some((line, character)) = target {
            let col = self.utf16_to_grapheme_col(line, character);
            self.buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
    }

    /// Jump to previous diagnostic ([d)
    pub fn goto_prev_diagnostic(&mut self) {
        let current_line = self.buffer().cursor().line();
        let current_col = self.buffer().cursor().col();
        let current_col_utf16 = self.col_to_utf16(current_line, current_col.0);
        let diagnostics = &self.lsp.state.current_file_diagnostics;

        // Find last diagnostic before current position (compare line, then column)
        let prev = diagnostics
            .iter()
            .filter(|d| {
                let dl = d.range.start.line as usize;
                dl < current_line
                    || (dl == current_line && d.range.start.character < current_col_utf16)
            })
            .max_by_key(|d| (d.range.start.line, d.range.start.character));

        let target = prev
            .or_else(|| diagnostics.last())
            .map(|d| (d.range.start.line as usize, d.range.start.character));

        if let Some((line, character)) = target {
            let col = self.utf16_to_grapheme_col(line, character);
            self.buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// Inject diagnostics for testing diagnostic navigation.
    /// Sets diagnostics for the current file (test helper).
    pub fn set_test_diagnostics(&mut self, diagnostics: Vec<lsp_types::Diagnostic>) {
        self.lsp.state.set_current_file_diagnostics(diagnostics);
        self.lsp.state.diagnostics_file_path = self.buffer().file_path().map(|p| p.to_string());
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;
    use std::mem::size_of;
    use std::sync::{Arc, Mutex};

    /// This test measures the size of the Editor struct and its major components.
    /// This is critical for understanding stack overhead and determining if we need
    /// to box large fields or wrap the entire struct in Arc<Mutex<>>.
    ///
    /// Educational context:
    /// - Stack allocation is fast but limited (~2MB on most systems)
    /// - CPU cache lines are typically 64 bytes
    /// - Structs > 512 bytes should be considered for heap allocation
    /// - Structs > 2KB definitely should be boxed or arc'd
    ///
    /// In Rust, when you pass a value by ownership (not reference), it gets moved,
    /// which involves copying all the bytes. For large structs, this becomes expensive.
    #[test]
    fn measure_editor_size() {
        println!("\n=== Editor Struct Size Analysis ===\n");

        let editor_size = size_of::<Editor>();
        println!(
            "Total Editor size: {} bytes ({:.2} KB)",
            editor_size,
            editor_size as f64 / 1024.0
        );

        // Measure major field types
        println!("\nMajor field sizes:");
        println!(
            "  Vec<Buffer>:                {} bytes",
            size_of::<Vec<Buffer>>()
        );
        println!(
            "  Option<WindowManager>:      {} bytes",
            size_of::<Option<WindowManager>>()
        );
        println!(
            "  RegisterManager:            {} bytes",
            size_of::<RegisterManager>()
        );
        println!(
            "  MarkManager:                {} bytes",
            size_of::<MarkManager>()
        );
        println!(
            "  KeyMapManager:              {} bytes",
            size_of::<KeyMapManager>()
        );
        println!(
            "  JumpList:                   {} bytes",
            size_of::<JumpList>()
        );
        println!(
            "  TagStack:                   {} bytes",
            size_of::<TagStack>()
        );
        println!(
            "  MacroManager:               {} bytes",
            size_of::<MacroManager>()
        );
        println!(
            "  Option<Picker>:             {} bytes",
            size_of::<Option<Picker>>()
        );
        println!(
            "  InputState:                 {} bytes",
            size_of::<InputState>()
        );
        println!(
            "  LspState:                   {} bytes",
            size_of::<LspState>()
        );
        println!(
            "  CompletionMenu:             {} bytes",
            size_of::<CompletionMenu>()
        );
        println!(
            "  HashMap<String, PreviewCache>: {} bytes",
            size_of::<HashMap<String, PreviewCache>>()
        );
        println!(
            "  ColorSchemeRegistry:        {} bytes",
            size_of::<crate::syntax::ColorSchemeRegistry>()
        );
        println!(
            "  EditorOptions:              {} bytes",
            size_of::<EditorOptions>()
        );
        println!(
            "  FileTree:                   {} bytes",
            size_of::<FileTree>()
        );
        println!(
            "  QuickfixList:               {} bytes",
            size_of::<QuickfixList>()
        );
        println!(
            "  LocationList:               {} bytes",
            size_of::<LocationList>()
        );
        println!(
            "  TabPageManager:             {} bytes",
            size_of::<TabPageManager>()
        );

        // Measure small scalar/enum fields for comparison
        println!("\nSmall field sizes (for reference):");
        println!("  Mode:                       {} bytes", size_of::<Mode>());
        println!("  bool:                       {} bytes", size_of::<bool>());
        println!("  usize:                      {} bytes", size_of::<usize>());
        println!(
            "  Option<usize>:              {} bytes",
            size_of::<Option<usize>>()
        );
        println!(
            "  Option<char>:               {} bytes",
            size_of::<Option<char>>()
        );
        println!(
            "  String:                     {} bytes",
            size_of::<String>()
        );
        println!(
            "  Option<(usize, usize)>:     {} bytes",
            size_of::<Option<(usize, usize)>>()
        );

        // Measure wrapping options
        println!("\nWrapping overhead:");
        println!(
            "  Arc<Mutex<Editor>>:         {} bytes (pointer-sized)",
            size_of::<Arc<Mutex<Editor>>>()
        );
        println!(
            "  Box<Editor>:                {} bytes (pointer-sized)",
            size_of::<Box<Editor>>()
        );

        // Analysis and recommendations
        println!("\n=== Analysis ===");

        const CACHE_LINE: usize = 64;
        const RECOMMENDED_MAX: usize = 512;
        const MUST_OPTIMIZE: usize = 2048;

        if editor_size <= CACHE_LINE {
            println!("Status: EXCELLENT - fits in a single cache line");
        } else if editor_size <= RECOMMENDED_MAX {
            println!("Status: GOOD - small enough for stack, no action needed");
        } else if editor_size <= MUST_OPTIMIZE {
            println!("Status: CONSIDER OPTIMIZATION - boxing large fields would help");
            println!("Recommendation: Box fields > 64 bytes to reduce struct size");
        } else {
            println!("Status: MUST OPTIMIZE - too large for efficient stack allocation");
            println!(
                "Recommendation: Box all fields > 64 bytes OR wrap entire Editor in Arc<Mutex<>>"
            );
        }

        println!("\n=== Stack Usage Patterns ===");
        println!("Current usage:");
        println!("  - run_headless_loop: takes &mut Editor (zero copy)");
        println!("  - UI rendering: borrows &Editor (zero copy)");
        println!("  - API handlers: borrow &mut Editor via channels (zero copy)");
        println!("\nVerdict: Editor is NEVER passed by value, only by reference.");
        println!(
            "Stack overhead = {} bytes once per thread (in main function).",
            editor_size
        );

        // Educational note about the measurement
        println!("\n=== Educational Context ===");
        println!("Why size matters:");
        println!(
            "1. Stack allocation: Creating Editor on stack uses {} bytes",
            editor_size
        );
        println!(
            "2. Move semantics: Moving Editor copies {} bytes",
            editor_size
        );
        println!("3. Async futures: Each .await point may store Editor state");
        println!(
            "4. Cache locality: Struct doesn't fit in L1 cache ({} bytes)",
            CACHE_LINE
        );
        println!("\nHowever, since Editor is always passed by &mut reference,");
        println!("the only overhead is the initial allocation in main().");
        println!("This is a one-time cost, not a per-call cost.");
    }

    /// Size regression test - fails if Editor grows beyond threshold
    /// This prevents accidental struct bloat during development
    #[test]
    fn editor_size_regression() {
        const MAX_ACCEPTABLE_SIZE: usize = 10_000; // 10KB - conservative threshold

        let actual = size_of::<Editor>();

        // This test allows for some growth but prevents runaway bloat
        assert!(
            actual <= MAX_ACCEPTABLE_SIZE,
            "Editor struct is {} bytes, exceeds maximum of {} bytes. \n\
             Consider:\n\
             1. Boxing large fields (HashMap, Vec, etc.)\n\
             2. Using Arc<Mutex<>> for shared state\n\
             3. Moving large data to heap with Box\n\
             \n\
             Run 'cargo test measure_editor_size -- --nocapture' to see size breakdown.",
            actual,
            MAX_ACCEPTABLE_SIZE
        );
    }

    /// Test that Arc<Mutex<Editor>> is pointer-sized (8 or 16 bytes)
    /// This verifies that wrapping in Arc doesn't add significant overhead
    #[test]
    fn arc_mutex_editor_is_pointer_sized() {
        let arc_size = size_of::<Arc<Mutex<Editor>>>();

        // Arc is a fat pointer (ptr + ref_count), should be 16 bytes on 64-bit
        assert!(
            arc_size <= 16,
            "Arc<Mutex<Editor>> should be pointer-sized (8-16 bytes), got {} bytes",
            arc_size
        );
    }
}

#[cfg(test)]
mod per_window_wrap_tests {
    use super::*;

    /// A buffer line long enough to wrap at narrow widths but not at wide ones.
    const LONG_LINE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"; // 52 chars

    #[test]
    fn ensure_wrap_map_for_window_uses_per_window_width() {
        let mut editor = Editor::with_content(&format!("{LONG_LINE}\n"));
        editor.options.wrap = true;
        editor.init_window_manager(80, 24);
        editor.split_window_vertical(); // windows 0 and 1, same buffer

        // Build each window's map at a *different* content width.
        editor.ensure_wrap_map_for_window(0, 16);
        editor.ensure_wrap_map_for_window(1, 40);

        let wm = editor.window_manager().expect("window manager");
        let w0 = wm.get_window(0).and_then(|w| w.wrap_map()).expect("w0 map");
        let w1 = wm.get_window(1).and_then(|w| w.wrap_map()).expect("w1 map");

        assert_eq!(w0.wrap_width(), 16, "window 0 wraps at its own width");
        assert_eq!(w1.wrap_width(), 40, "window 1 wraps at its own width");
        // 52 chars: at width 16 → 4 rows; at width 40 → 2 rows.
        assert_eq!(w0.visual_lines_for(0), 4);
        assert_eq!(w1.visual_lines_for(0), 2);
        // The editor-global fallback slot is *not* touched when a WindowManager
        // exists — it's only the headless path.
        assert!(editor.viewport.wrap_map.is_none());
    }

    #[test]
    fn wrap_map_accessor_follows_focus() {
        let mut editor = Editor::with_content(&format!("{LONG_LINE}\n"));
        editor.options.wrap = true;
        editor.init_window_manager(80, 24);
        editor.split_window_vertical();

        editor.ensure_wrap_map_for_window(0, 16);
        editor.ensure_wrap_map_for_window(1, 40);

        // Focus is on window 0 after a vertical split.
        assert_eq!(editor.window_manager().unwrap().focused_window_index(), 0);
        assert_eq!(editor.wrap_map().unwrap().wrap_width(), 16);

        // Move focus to window 1 — the accessor must follow.
        editor.focus_next_window();
        assert_eq!(editor.window_manager().unwrap().focused_window_index(), 1);
        assert_eq!(editor.wrap_map().unwrap().wrap_width(), 40);

        // And `ensure_wrap_map` (no explicit index) drives the focused window.
        editor.ensure_wrap_map(24);
        assert_eq!(editor.wrap_map().unwrap().wrap_width(), 24);
        // window 0's map untouched by the focused-window rebuild.
        assert_eq!(
            editor
                .window_manager()
                .unwrap()
                .get_window(0)
                .and_then(|w| w.wrap_map())
                .unwrap()
                .wrap_width(),
            16
        );
    }

    #[test]
    fn no_window_manager_uses_editor_global_slot() {
        // Headless / test-harness path: no WindowManager → ViewportState slot.
        let mut editor = Editor::with_content(&format!("{LONG_LINE}\n"));
        editor.options.wrap = true;
        assert!(editor.window_manager().is_none());

        editor.ensure_wrap_map(20);
        assert_eq!(editor.wrap_map().unwrap().wrap_width(), 20);
        assert_eq!(editor.viewport.wrap_map.as_ref().unwrap().wrap_width(), 20);
        // 52 chars at width 20 → 3 rows.
        assert_eq!(editor.wrap_map().unwrap().visual_lines_for(0), 3);

        // Turning wrap off clears the slot.
        editor.options.wrap = false;
        editor.ensure_wrap_map(20);
        assert!(editor.wrap_map().is_none());
    }

    #[test]
    fn ensure_wrap_map_for_window_is_noop_without_that_window() {
        let mut editor = Editor::with_content("x\n");
        editor.options.wrap = true;
        editor.init_window_manager(80, 24);
        // Only window 0 exists; targeting index 5 must not panic / allocate.
        editor.ensure_wrap_map_for_window(5, 20);
        assert!(editor.window_manager().unwrap().get_window(5).is_none());
    }
}
