use serde::{Deserialize, Serialize};

pub const PROFILE_LOCAL: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileScope {
    /// Only the current selection.
    Selection,
    /// Current file only.
    #[default]
    File,
    /// All files in the project.
    Project,
    /// Unrestricted file access.
    Any,
}

impl FileScope {
    fn ordinal(self) -> u8 {
        match self {
            Self::Selection => 0,
            Self::File => 1,
            Self::Project => 2,
            Self::Any => 3,
        }
    }
}

impl PartialOrd for FileScope {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileScope {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordinal().cmp(&other.ordinal())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileScope {
    pub files: FileScope,
    pub shell: bool,
    pub network: bool,
}

impl Default for ProfileScope {
    fn default() -> Self {
        Self {
            files: FileScope::File,
            shell: false,
            network: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolApprovalMode {
    #[default]
    Auto,
    SensitivePrompt,
    AlwaysPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProviderKind {
    /// ChatGPT-subscription inference with Ovim owning the agent harness.
    Codex,
    /// Legacy Codex app-server transport, including its built-in harness.
    CodexAppServer,
    OpenAi,
    Anthropic,
    Ollama,
}

impl std::fmt::Display for AiProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Codex => "codex",
            Self::CodexAppServer => "codex_app_server",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EditFormat {
    Codeblock,
    #[default]
    Json,
    Raw,
    ApplyPatch,
    StrReplace,
    Lua(String),
}

impl std::fmt::Display for EditFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Codeblock => "codeblock",
            Self::Json => "json",
            Self::Raw => "raw",
            Self::ApplyPatch => "apply_patch",
            Self::StrReplace => "str_replace",
            Self::Lua(name) => return write!(f, "lua:{name}"),
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagnosticScope {
    #[default]
    Overlapping,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextGatheringPolicy {
    pub surrounding_lines: u16,
    pub symbols: u16,
    pub diagnostics: DiagnosticScope,
    pub related_slices: bool,
    pub budget: usize,
}

impl Default for ContextGatheringPolicy {
    fn default() -> Self {
        Self {
            surrounding_lines: 6,
            symbols: 12,
            diagnostics: DiagnosticScope::Overlapping,
            related_slices: true,
            budget: 8_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLoopConfig {
    /// Optional per-turn tool-call ceiling. Long-running agent turns are
    /// unlimited by default; callers can opt into a finite guardrail.
    pub max_tool_calls: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetryPolicy {
    pub max: u8,
    pub fallback: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodeSlice {
    pub label: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct SymbolFact {
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub character: u32,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DiagnosticFact {
    pub message: String,
    pub severity: Option<String>,
    pub line: u32,
    pub start_character: u32,
    pub end_character: u32,
}

#[derive(Debug, Clone, Default)]
pub struct AiContextPack {
    pub selection: String,
    pub surrounding: Vec<CodeSlice>,
    pub symbol_facts: Vec<SymbolFact>,
    pub diagnostics: Vec<DiagnosticFact>,
    pub related_slices: Vec<CodeSlice>,
}

#[derive(Debug, Clone, Default)]
pub struct ApiKeyConfig {
    pub env_var: Option<String>,
    pub file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AiRequest {
    pub prompt: String,
    pub selected_text: String,
    pub language_id: Option<String>,
    pub file_path: Option<String>,
    pub edit_format: EditFormat,
    pub context_pack: Option<AiContextPack>,
}

#[derive(Debug, Clone)]
pub struct AiJobResult {
    pub replacement: String,
    pub new_import_statements: Vec<String>,
    pub log_lines: Vec<String>,
    pub raw_output: String,
    pub provider: AiProviderKind,
    pub profile_name: String,
    pub model: String,
    /// Number of retry attempts before extraction succeeded (0 = first attempt).
    pub retry_attempts: u8,
    /// Elision markers detected in the replacement text (empty = clean).
    pub elision_markers: Vec<String>,
}
