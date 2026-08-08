use crate::ai::types::{
    AgentLoopConfig, AiProviderKind, ApiKeyConfig, ContextGatheringPolicy, DiagnosticScope,
    EditFormat, FileScope, ProfileScope, RetryPolicy, ToolApprovalMode, PROFILE_LOCAL,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Preview policy for Ovim-owned delegated agents.
///
/// The feature is deliberately disabled by default. Enabling it grants only
/// the parent control surface; child authority is still narrowed separately
/// by the read-only workspace and role policy.
#[derive(Debug, Clone, PartialEq)]
pub struct AiSubagentConfig {
    pub enabled: bool,
    pub max_concurrent: usize,
    pub max_queued: usize,
    pub max_children_per_parent: usize,
    pub max_total_per_run: usize,
    pub max_depth: usize,
    pub default_timeout_seconds: u64,
    pub allow_writes: bool,
    pub allow_network: bool,
    /// Empty means every otherwise eligible configured catalog entry.
    pub allowed_models: Vec<String>,
    /// Empty fails closed. The preview defaults to the two read-only roles.
    pub allowed_agent_kinds: Vec<String>,
    /// Empty means every effort advertised by an allowed catalog entry.
    pub allowed_reasoning_efforts: Vec<String>,
    pub budgets: AiSubagentBudgetConfig,
    /// Storage and retention policy for Ovim-owned write workspaces.
    ///
    /// This has a separate root and retention window from the durable run log:
    /// deleting conversational history must never imply deletion of an
    /// unresolved worktree.
    pub workspaces: AiSubagentWorkspaceConfig,
}

impl Default for AiSubagentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent: 3,
            max_queued: 8,
            max_children_per_parent: 4,
            max_total_per_run: 8,
            max_depth: 1,
            default_timeout_seconds: 600,
            allow_writes: false,
            allow_network: false,
            allowed_models: Vec::new(),
            allowed_agent_kinds: vec!["explorer".into(), "reviewer".into()],
            allowed_reasoning_efforts: Vec::new(),
            budgets: AiSubagentBudgetConfig::default(),
            workspaces: AiSubagentWorkspaceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSubagentWorkspaceConfig {
    /// Explicit workspace root. `None` selects the platform-data default.
    pub root: Option<PathBuf>,
    pub branch_prefix: String,
    pub completed_retention_hours: u64,
    pub minimum_free_space_mb: u64,
}

impl Default for AiSubagentWorkspaceConfig {
    fn default() -> Self {
        Self {
            root: None,
            branch_prefix: "ovim".into(),
            completed_retention_hours: 24,
            minimum_free_space_mb: 2_048,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AiSubagentBudgetConfig {
    pub max_provider_events_per_agent: usize,
    pub max_tool_calls_per_agent: usize,
    pub max_total_provider_events: usize,
    pub max_total_tool_calls: usize,
    /// Reserved until delegated provider adapters expose trustworthy prices.
    /// Validated and persisted for config compatibility, but not enforced.
    pub max_estimated_cost: f64,
}

impl Default for AiSubagentBudgetConfig {
    fn default() -> Self {
        Self {
            max_provider_events_per_agent: 256,
            max_tool_calls_per_agent: 48,
            max_total_provider_events: 1024,
            max_total_tool_calls: 160,
            max_estimated_cost: 5.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiProfileConfig {
    pub name: String,
    pub provider: AiProviderKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub edit_format: EditFormat,
    pub chat_edit_format: Option<EditFormat>,
    pub context: ContextGatheringPolicy,
    pub agent_loop: AgentLoopConfig,
    pub tools: Vec<String>,
    pub scope: ProfileScope,
    pub edit_prompt: Option<String>,
    pub chat_prompt: Option<String>,
    pub chat_edit_prompt: Option<String>,
    pub reasoning_effort: Option<String>,
    pub verbosity: Option<String>,
    pub syntax_check: Option<bool>,
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone)]
pub struct ProjectContextConfig {
    /// File names to search for (e.g. ".ovim.md", "AGENTS.md", "CLAUDE.md").
    pub files: Vec<String>,
    /// Walk from repo root down to file's directory, collecting all matches.
    pub hierarchical: bool,
    /// Budget in characters (rough proxy for tokens).
    pub budget: usize,
    /// Master switch.
    pub enabled: bool,
}

impl Default for ProjectContextConfig {
    fn default() -> Self {
        Self {
            files: vec![
                ".ovim.md".to_string(),
                "AGENTS.md".to_string(),
                "CLAUDE.md".to_string(),
            ],
            hierarchical: true,
            budget: 2000,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatContextConfig {
    /// Number of recent turns whose tool results are kept verbatim.
    pub observation_window: usize,
    /// Template for masked tool results. `{turn}` is replaced with the turn number.
    pub mask_template: String,
    /// Max context tokens (deferred — not enforced yet).
    pub max_context_tokens: usize,
}

impl Default for ChatContextConfig {
    fn default() -> Self {
        Self {
            observation_window: 10,
            mask_template: "[output from turn {turn}]".to_string(),
            max_context_tokens: 100_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub default_profile: String,
    pub profiles: HashMap<String, AiProfileConfig>,
    /// Maps context names ("chat", "selection", "query") to profile names.
    pub contexts: HashMap<String, String>,
    /// Named API key sources (env var or file).
    pub api_key_registry: HashMap<String, ApiKeyConfig>,
    /// Global prompt templates (e.g. "edit", "chat") with `{{variable}}` interpolation.
    pub prompts: HashMap<String, String>,
    /// Format-specific system prompts registered via `vim.ai.formats.register()`.
    pub format_prompts: HashMap<String, String>,
    /// Project context file loading configuration.
    pub project_context: ProjectContextConfig,
    /// Chat context management (observation masking).
    pub chat_context: ChatContextConfig,
    /// Tool-approval behavior for AI chat tool calls.
    pub tool_approval_mode: ToolApprovalMode,
    /// Explicitly gated preview configuration for delegated agents.
    pub subagents: AiSubagentConfig,
}

#[derive(Debug, Deserialize)]
struct AiTomlConfig {
    default_profile: Option<String>,
    tool_approval_mode: Option<String>,
    #[serde(default)]
    profiles: HashMap<String, AiTomlProfile>,
    #[serde(default)]
    subagents: AiTomlSubagentConfig,
}

#[derive(Debug, Default, Deserialize)]
struct AiTomlSubagentConfig {
    enabled: Option<bool>,
    max_concurrent: Option<usize>,
    max_queued: Option<usize>,
    max_children_per_parent: Option<usize>,
    max_total_per_run: Option<usize>,
    max_depth: Option<usize>,
    default_timeout_seconds: Option<u64>,
    allow_writes: Option<bool>,
    allow_network: Option<bool>,
    allowed_models: Option<Vec<String>>,
    allowed_agent_kinds: Option<Vec<String>>,
    allowed_reasoning_efforts: Option<Vec<String>>,
    #[serde(default)]
    budgets: AiTomlSubagentBudgetConfig,
    #[serde(default)]
    workspaces: AiTomlSubagentWorkspaceConfig,
}

#[derive(Debug, Default, Deserialize)]
struct AiTomlSubagentWorkspaceConfig {
    root: Option<PathBuf>,
    branch_prefix: Option<String>,
    completed_retention_hours: Option<u64>,
    minimum_free_space_mb: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct AiTomlSubagentBudgetConfig {
    max_provider_events_per_agent: Option<usize>,
    max_tool_calls_per_agent: Option<usize>,
    max_total_provider_events: Option<usize>,
    max_total_tool_calls: Option<usize>,
    max_estimated_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct AiTomlProfile {
    provider: AiProviderKind,
    model: String,
    base_url: Option<String>,
    api_key: Option<String>,
    api_key_env: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
    edit_format: Option<String>,
    chat_edit_format: Option<String>,
    surrounding_lines: Option<u16>,
    symbols: Option<u16>,
    diagnostics: Option<String>,
    related_slices: Option<bool>,
    context_budget: Option<usize>,
    max_tool_calls: Option<u64>,
    tools: Option<Vec<String>>,
    scope: Option<String>,
    scope_shell: Option<bool>,
    scope_network: Option<bool>,
    reasoning_effort: Option<String>,
    verbosity: Option<String>,
    syntax_check: Option<bool>,
    retry_max: Option<u8>,
    retry_fallback: Option<String>,
    edit_prompt: Option<String>,
    chat_prompt: Option<String>,
    chat_edit_prompt: Option<String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        let mut profiles = HashMap::new();
        profiles.insert(
            PROFILE_LOCAL.to_string(),
            AiProfileConfig {
                name: PROFILE_LOCAL.to_string(),
                provider: AiProviderKind::Ollama,
                model: "qwen2.5-coder:7b".to_string(),
                base_url: Some("http://127.0.0.1:11434".to_string()),
                api_key: None,
                api_key_env: None,
                temperature: Some(0.2),
                max_tokens: Some(2048),
                system_prompt: Some(default_system_prompt().to_string()),
                edit_format: EditFormat::Json,
                chat_edit_format: None,
                context: ContextGatheringPolicy {
                    budget: 2_500,
                    related_slices: false,
                    symbols: 6,
                    ..ContextGatheringPolicy::default()
                },
                agent_loop: AgentLoopConfig::default(),
                tools: vec![],
                scope: ProfileScope::default(),
                edit_prompt: None,
                chat_prompt: None,
                chat_edit_prompt: None,
                reasoning_effort: None,
                verbosity: None,
                syntax_check: None,
                retry: RetryPolicy::default(),
            },
        );

        Self {
            default_profile: PROFILE_LOCAL.to_string(),
            profiles,
            contexts: HashMap::new(),
            api_key_registry: HashMap::new(),
            prompts: HashMap::new(),
            format_prompts: HashMap::new(),
            project_context: ProjectContextConfig::default(),
            chat_context: ChatContextConfig::default(),
            tool_approval_mode: ToolApprovalMode::default(),
            subagents: AiSubagentConfig::default(),
        }
    }
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read AI config: {}", path.display()))?;
        let parsed: AiTomlConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse AI config: {}", path.display()))?;

        let mut cfg = Self::default();
        for (name, profile) in parsed.profiles {
            let edit_format =
                parse_edit_format_str(profile.edit_format.as_deref().unwrap_or("json"));
            let chat_edit_format = profile
                .chat_edit_format
                .as_deref()
                .map(parse_edit_format_str);

            let diagnostics = match profile.diagnostics.as_deref() {
                Some("file") => DiagnosticScope::File,
                _ => DiagnosticScope::Overlapping,
            };

            let context = ContextGatheringPolicy {
                surrounding_lines: profile.surrounding_lines.unwrap_or(6),
                symbols: profile.symbols.unwrap_or(12),
                diagnostics,
                related_slices: profile.related_slices.unwrap_or(true),
                budget: profile.context_budget.unwrap_or(8_000),
            };

            let agent_loop = AgentLoopConfig {
                // Treat the legacy value 0 as unlimited as well as omission.
                max_tool_calls: profile.max_tool_calls.filter(|limit| *limit > 0),
            };

            let retry = RetryPolicy {
                max: profile.retry_max.unwrap_or(0),
                fallback: profile.retry_fallback,
            };
            let file_scope = profile
                .scope
                .as_deref()
                .and_then(parse_file_scope_str)
                .unwrap_or(FileScope::File);
            let scope = ProfileScope {
                files: file_scope,
                shell: profile.scope_shell.unwrap_or(false),
                network: profile.scope_network.unwrap_or(false),
            };
            let tools = profile.tools.unwrap_or_default();

            // Eagerly resolve api_key_env: if the user didn't set one in TOML,
            // fill in the provider default (e.g. "ANTHROPIC_API_KEY").
            let api_key_env = profile
                .api_key_env
                .or_else(|| default_api_key_env(profile.provider));

            cfg.profiles.insert(
                name.clone(),
                AiProfileConfig {
                    name,
                    provider: profile.provider,
                    model: profile.model,
                    base_url: profile.base_url,
                    api_key: profile.api_key,
                    api_key_env,
                    temperature: profile.temperature,
                    max_tokens: profile.max_tokens,
                    system_prompt: profile.system_prompt,
                    edit_format,
                    chat_edit_format,
                    context,
                    agent_loop,
                    tools,
                    scope,
                    edit_prompt: profile.edit_prompt,
                    chat_prompt: profile.chat_prompt,
                    chat_edit_prompt: profile.chat_edit_prompt,
                    reasoning_effort: profile.reasoning_effort,
                    verbosity: profile.verbosity,
                    syntax_check: profile.syntax_check,
                    retry,
                },
            );
        }

        if let Some(default_profile) = parsed.default_profile {
            cfg.default_profile = default_profile;
        }
        if let Some(mode) = parsed.tool_approval_mode.as_deref() {
            cfg.tool_approval_mode = parse_tool_approval_mode(mode);
        }
        cfg.subagents = merge_subagent_config(parsed.subagents)?;

        if !cfg.profiles.contains_key(&cfg.default_profile) {
            cfg.default_profile = PROFILE_LOCAL.to_string();
        }

        Ok(cfg)
    }

    pub fn resolve_profile(&self, name: &str) -> Option<&AiProfileConfig> {
        self.profiles.get(name)
    }
}

fn merge_subagent_config(parsed: AiTomlSubagentConfig) -> Result<AiSubagentConfig> {
    let mut config = AiSubagentConfig::default();
    config.enabled = parsed.enabled.unwrap_or(config.enabled);
    config.max_concurrent = parsed.max_concurrent.unwrap_or(config.max_concurrent);
    config.max_queued = parsed.max_queued.unwrap_or(config.max_queued);
    config.max_children_per_parent = parsed
        .max_children_per_parent
        .unwrap_or(config.max_children_per_parent);
    config.max_total_per_run = parsed.max_total_per_run.unwrap_or(config.max_total_per_run);
    config.max_depth = parsed.max_depth.unwrap_or(config.max_depth);
    config.default_timeout_seconds = parsed
        .default_timeout_seconds
        .unwrap_or(config.default_timeout_seconds);
    config.allow_writes = parsed.allow_writes.unwrap_or(config.allow_writes);
    config.allow_network = parsed.allow_network.unwrap_or(config.allow_network);
    config.allowed_models = parsed.allowed_models.unwrap_or(config.allowed_models);
    config.allowed_agent_kinds = parsed
        .allowed_agent_kinds
        .unwrap_or(config.allowed_agent_kinds);
    config.allowed_reasoning_efforts = parsed
        .allowed_reasoning_efforts
        .unwrap_or(config.allowed_reasoning_efforts);
    config.budgets.max_provider_events_per_agent = parsed
        .budgets
        .max_provider_events_per_agent
        .unwrap_or(config.budgets.max_provider_events_per_agent);
    config.budgets.max_tool_calls_per_agent = parsed
        .budgets
        .max_tool_calls_per_agent
        .unwrap_or(config.budgets.max_tool_calls_per_agent);
    config.budgets.max_total_provider_events = parsed
        .budgets
        .max_total_provider_events
        .unwrap_or(config.budgets.max_total_provider_events);
    config.budgets.max_total_tool_calls = parsed
        .budgets
        .max_total_tool_calls
        .unwrap_or(config.budgets.max_total_tool_calls);
    config.budgets.max_estimated_cost = parsed
        .budgets
        .max_estimated_cost
        .unwrap_or(config.budgets.max_estimated_cost);
    config.workspaces.root = parsed.workspaces.root.or(config.workspaces.root);
    config.workspaces.branch_prefix = parsed
        .workspaces
        .branch_prefix
        .unwrap_or(config.workspaces.branch_prefix);
    config.workspaces.completed_retention_hours = parsed
        .workspaces
        .completed_retention_hours
        .unwrap_or(config.workspaces.completed_retention_hours);
    config.workspaces.minimum_free_space_mb = parsed
        .workspaces
        .minimum_free_space_mb
        .unwrap_or(config.workspaces.minimum_free_space_mb);

    let positive = [
        config.max_concurrent,
        config.max_queued,
        config.max_children_per_parent,
        config.max_total_per_run,
        config.max_depth,
        config.budgets.max_provider_events_per_agent,
        config.budgets.max_tool_calls_per_agent,
        config.budgets.max_total_provider_events,
        config.budgets.max_total_tool_calls,
    ];
    if positive.contains(&0) || config.default_timeout_seconds == 0 {
        anyhow::bail!("subagent limits and budgets must be positive");
    }
    if !config.budgets.max_estimated_cost.is_finite() || config.budgets.max_estimated_cost < 0.0 {
        anyhow::bail!("subagent max_estimated_cost must be finite and non-negative");
    }
    if config.workspaces.branch_prefix.trim().is_empty()
        || config.workspaces.branch_prefix.contains("..")
        || config.workspaces.branch_prefix.starts_with('/')
        || config.workspaces.branch_prefix.ends_with('/')
    {
        anyhow::bail!("subagent workspace branch_prefix must be a non-empty relative ref prefix");
    }
    if config.allow_writes || config.allow_network || !(1..=4).contains(&config.max_depth) {
        anyhow::bail!(
            "the subagent preview supports only read-only, network-disabled children at depth 1 through 4"
        );
    }
    validate_allowlist("allowed_models", &config.allowed_models)?;
    validate_allowlist("allowed_agent_kinds", &config.allowed_agent_kinds)?;
    validate_allowlist(
        "allowed_reasoning_efforts",
        &config.allowed_reasoning_efforts,
    )?;
    Ok(config)
}

fn validate_allowlist(name: &str, values: &[String]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            anyhow::bail!("subagent {name} contains an empty value");
        }
        if !seen.insert(value) {
            anyhow::bail!("subagent {name} repeats {value:?}");
        }
    }
    Ok(())
}

/// Parse a string into an EditFormat enum.
pub fn parse_edit_format_str(s: &str) -> EditFormat {
    match s {
        "codeblock" => EditFormat::Codeblock,
        "json" => EditFormat::Json,
        "raw" => EditFormat::Raw,
        "apply_patch" => EditFormat::ApplyPatch,
        "str_replace" => EditFormat::StrReplace,
        other => {
            if let Some(name) = other.strip_prefix("lua:") {
                EditFormat::Lua(name.to_string())
            } else {
                EditFormat::Lua(other.to_string())
            }
        }
    }
}

fn parse_file_scope_str(s: &str) -> Option<FileScope> {
    match s.to_ascii_lowercase().as_str() {
        "selection" => Some(FileScope::Selection),
        "file" => Some(FileScope::File),
        "project" => Some(FileScope::Project),
        "any" => Some(FileScope::Any),
        _ => None,
    }
}

fn parse_tool_approval_mode(s: &str) -> ToolApprovalMode {
    match s.to_ascii_lowercase().as_str() {
        "auto" => ToolApprovalMode::Auto,
        "always_prompt" => ToolApprovalMode::AlwaysPrompt,
        _ => ToolApprovalMode::SensitivePrompt,
    }
}

/// Infer provider from model name prefix.
pub fn infer_provider(model: &str) -> AiProviderKind {
    let m = model.to_lowercase();
    if m.starts_with("gpt-5.6-") || m.contains("codex") {
        AiProviderKind::Codex
    } else if m.starts_with("claude") {
        AiProviderKind::Anthropic
    } else if m.starts_with("gpt-")
        || m.starts_with("o1-")
        || m.starts_with("o3-")
        || m.starts_with("o4-")
    {
        AiProviderKind::OpenAi
    } else {
        AiProviderKind::Ollama
    }
}

/// Default API key environment variable for a given provider.
pub fn default_api_key_env(provider: AiProviderKind) -> Option<String> {
    match provider {
        AiProviderKind::Codex | AiProviderKind::CodexAppServer => None,
        AiProviderKind::OpenAi => Some("OPENAI_API_KEY".to_string()),
        AiProviderKind::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        AiProviderKind::Ollama => None,
    }
}

/// Parse a provider string (e.g. from Lua) into AiProviderKind.
pub fn parse_provider_str(s: &str) -> Option<AiProviderKind> {
    match s.to_lowercase().as_str() {
        "codex" => Some(AiProviderKind::Codex),
        "codex_app_server" => Some(AiProviderKind::CodexAppServer),
        "openai" => Some(AiProviderKind::OpenAi),
        "anthropic" => Some(AiProviderKind::Anthropic),
        "ollama" => Some(AiProviderKind::Ollama),
        _ => None,
    }
}

fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("ovim").join("ai.toml")
}

fn default_system_prompt() -> &'static str {
    "You are an editing agent. Return JSON: {\"replacement\": string, \"new_import_statements\": string[], \"log\": string[]}. Only include valid JSON."
}

/// Returns a system prompt appropriate for the given edit format.
/// Used as a fallback when a profile has no explicit system prompt.
pub fn system_prompt_for_edit_format(format: &EditFormat) -> &'static str {
    match format {
        EditFormat::Json => {
            "You are a code editing assistant. Return your response as JSON with the schema: {\"replacement\": string, \"new_import_statements\": string[], \"log\": string[]}. Only output valid JSON, no explanation."
        }
        EditFormat::Codeblock => {
            "You are a code editing assistant. Return ONLY the replacement code inside a single fenced code block (```). Do not include any explanation outside the code block."
        }
        EditFormat::Raw => {
            "You are a code editing assistant. Return ONLY the replacement code with no explanation, no markdown, no code fences."
        }
        EditFormat::ApplyPatch | EditFormat::StrReplace | EditFormat::Lua(_) => {
            "You are a code editing assistant. Return ONLY the replacement code with no explanation, no markdown, no code fences."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_the_default_tool_approval_mode() {
        assert_eq!(ToolApprovalMode::default(), ToolApprovalMode::Auto);
        assert_eq!(
            AiConfig::default().tool_approval_mode,
            ToolApprovalMode::Auto
        );
    }

    #[test]
    fn agent_turns_are_unlimited_by_default() {
        assert_eq!(AgentLoopConfig::default().max_tool_calls, None);
        assert_eq!(
            AiConfig::default()
                .resolve_profile(PROFILE_LOCAL)
                .unwrap()
                .agent_loop
                .max_tool_calls,
            None
        );
    }

    #[test]
    fn profile_scope_default_is_file() {
        assert_eq!(ProfileScope::default().files, FileScope::File);
    }

    #[test]
    fn parse_file_scope_values() {
        assert_eq!(
            parse_file_scope_str("selection"),
            Some(FileScope::Selection)
        );
        assert_eq!(parse_file_scope_str("file"), Some(FileScope::File));
        assert_eq!(parse_file_scope_str("project"), Some(FileScope::Project));
        assert_eq!(parse_file_scope_str("any"), Some(FileScope::Any));
        assert_eq!(parse_file_scope_str("unknown"), None);
    }

    #[test]
    fn parse_tool_approval_mode_values() {
        assert_eq!(parse_tool_approval_mode("auto"), ToolApprovalMode::Auto);
        assert_eq!(
            parse_tool_approval_mode("always_prompt"),
            ToolApprovalMode::AlwaysPrompt
        );
        assert_eq!(
            parse_tool_approval_mode("sensitive_prompt"),
            ToolApprovalMode::SensitivePrompt
        );
        // Unknown values fail closed to sensitive_prompt.
        assert_eq!(
            parse_tool_approval_mode("something_else"),
            ToolApprovalMode::SensitivePrompt
        );
    }

    #[test]
    fn codex_provider_is_parseable_and_needs_no_api_key() {
        assert_eq!(parse_provider_str("codex"), Some(AiProviderKind::Codex));
        assert_eq!(
            parse_provider_str("codex_app_server"),
            Some(AiProviderKind::CodexAppServer)
        );
        assert_eq!(infer_provider("gpt-5.6-sol"), AiProviderKind::Codex);
        assert_eq!(infer_provider("gpt-5.6-terra"), AiProviderKind::Codex);
        assert_eq!(default_api_key_env(AiProviderKind::Codex), None);
    }

    #[test]
    fn subagent_preview_defaults_fail_closed() {
        let preview = AiConfig::default().subagents;
        assert!(!preview.enabled);
        assert_eq!(preview.max_concurrent, 3);
        assert_eq!(preview.max_depth, 1);
        assert!(!preview.allow_writes);
        assert!(!preview.allow_network);
        assert_eq!(preview.allowed_agent_kinds, ["explorer", "reviewer"]);
        assert_eq!(preview.budgets.max_tool_calls_per_agent, 48);
        assert_eq!(preview.budgets.max_total_tool_calls, 160);
        assert_eq!(preview.workspaces.root, None);
        assert_eq!(preview.workspaces.branch_prefix, "ovim");
        assert_eq!(preview.workspaces.completed_retention_hours, 24);
        assert_eq!(preview.workspaces.minimum_free_space_mb, 2_048);
    }

    #[test]
    fn subagent_workspace_config_parses_without_process_environment() {
        let parsed: AiTomlConfig = toml::from_str(
            r#"
                [subagents.workspaces]
                root = "/fast/ovim-workspaces"
                branch_prefix = "agents/ovim"
                completed_retention_hours = 72
                minimum_free_space_mb = 4096
            "#,
        )
        .unwrap();

        let workspaces = merge_subagent_config(parsed.subagents).unwrap().workspaces;
        assert_eq!(
            workspaces.root.as_deref(),
            Some(std::path::Path::new("/fast/ovim-workspaces"))
        );
        assert_eq!(workspaces.branch_prefix, "agents/ovim");
        assert_eq!(workspaces.completed_retention_hours, 72);
        assert_eq!(workspaces.minimum_free_space_mb, 4_096);
    }

    #[test]
    fn subagent_preview_rejects_unsafe_or_ambiguous_policy() {
        assert!(merge_subagent_config(AiTomlSubagentConfig {
            enabled: Some(true),
            allow_writes: Some(true),
            ..AiTomlSubagentConfig::default()
        })
        .is_err());
        assert!(merge_subagent_config(AiTomlSubagentConfig {
            enabled: Some(true),
            allowed_models: Some(vec!["same/model".into(), "same/model".into()]),
            ..AiTomlSubagentConfig::default()
        })
        .is_err());
        assert!(merge_subagent_config(AiTomlSubagentConfig {
            enabled: Some(true),
            max_concurrent: Some(0),
            ..AiTomlSubagentConfig::default()
        })
        .is_err());
        assert_eq!(
            merge_subagent_config(AiTomlSubagentConfig {
                enabled: Some(true),
                max_depth: Some(2),
                ..AiTomlSubagentConfig::default()
            })
            .unwrap()
            .max_depth,
            2
        );
        assert!(merge_subagent_config(AiTomlSubagentConfig {
            enabled: Some(true),
            max_depth: Some(5),
            ..AiTomlSubagentConfig::default()
        })
        .is_err());
        assert!(merge_subagent_config(AiTomlSubagentConfig {
            enabled: Some(true),
            max_depth: Some(0),
            ..AiTomlSubagentConfig::default()
        })
        .is_err());
    }
}
