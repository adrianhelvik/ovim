use crate::ai::{
    default_api_key_env, infer_provider, parse_edit_format_str, parse_provider_str,
    AgentLoopConfig, AiProfileConfig, ApiKeyConfig, ChatContextConfig, ContextGatheringPolicy,
    DiagnosticScope, EditFormat, FileScope, ProfileScope, ProjectContextConfig, RetryPolicy,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A thread-safe bridge between Lua and the Editor
/// Allows Lua code to safely interact with editor state
#[derive(Clone)]
pub struct EditorBridge {
    inner: Arc<Mutex<EditorBridgeInner>>,
}

struct EditorBridgeInner {
    /// Commands to execute on the editor
    pending_commands: Vec<String>,
    /// Current cursor position (line, column)
    cursor_pos: Option<(usize, usize)>,
    /// Current buffer content (cached)
    buffer_content: Option<String>,
    /// Current mode
    mode: Option<String>,
    /// Global variables (vim.g namespace)
    global_vars: HashMap<String, GlobalValue>,
    /// AI context mappings (e.g. "chat" -> "opus")
    ai_contexts: HashMap<String, String>,
    /// AI default profile override
    ai_default_profile: Option<String>,
    /// AI profiles registered from Lua
    ai_profiles: HashMap<String, LuaProfileConfig>,
    /// Pending AI commands from Lua
    ai_pending_commands: Vec<AiCommand>,
    /// API key registry (name → config)
    api_key_registry: HashMap<String, ApiKeyConfig>,
    /// Global prompt templates (e.g. "edit" → template string)
    ai_prompts: HashMap<String, String>,
    /// Format-specific prompts (format name → system prompt string)
    format_prompts: HashMap<String, String>,
    /// Project context configuration from Lua
    project_context: ProjectContextConfig,
    /// Chat context configuration from Lua
    chat_context: ChatContextConfig,
    /// Agent loop configuration from Lua
    agent_loop: AgentLoopConfig,
    /// Whether AI config has been modified since last sync
    ai_dirty: bool,
}

/// AI profile configuration from Lua (before conversion to AiProfileConfig).
#[derive(Debug, Clone)]
pub struct LuaProfileConfig {
    pub model: String,
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub scope: Option<String>,
    pub scope_shell: bool,
    pub scope_network: bool,
    pub edit_format: Option<String>,
    pub chat_edit_format: Option<String>,
    pub context_surrounding_lines: Option<u16>,
    pub context_symbols: Option<u16>,
    pub context_diagnostics: Option<String>,
    pub context_related_slices: Option<bool>,
    pub context_budget: Option<usize>,
    pub max_tool_calls: Option<u64>,
    pub edit_prompt: Option<String>,
    pub chat_prompt: Option<String>,
    pub chat_edit_prompt: Option<String>,
    pub reasoning_effort: Option<String>,
    pub verbosity: Option<String>,
    pub syntax_check: Option<bool>,
    pub retry_max: Option<u8>,
    pub retry_fallback: Option<String>,
}

impl LuaProfileConfig {
    /// Convert to the engine's AiProfileConfig.
    pub fn into_profile_config(self, name: String) -> AiProfileConfig {
        let provider = self
            .provider
            .as_deref()
            .and_then(parse_provider_str)
            .unwrap_or_else(|| infer_provider(&self.model));

        let scope = ProfileScope {
            files: match self.scope.as_deref() {
                Some("selection") => FileScope::Selection,
                Some("file") => FileScope::File,
                Some("any") => FileScope::Any,
                Some("project") => FileScope::Project,
                _ => FileScope::File,
            },
            shell: self.scope_shell,
            network: self.scope_network,
        };

        let edit_format = self
            .edit_format
            .as_deref()
            .map(parse_edit_format_str)
            .unwrap_or(EditFormat::Codeblock);

        let chat_edit_format = self.chat_edit_format.as_deref().map(parse_edit_format_str);

        let diagnostics = match self.context_diagnostics.as_deref() {
            Some("file") => DiagnosticScope::File,
            _ => DiagnosticScope::Overlapping,
        };

        let context = ContextGatheringPolicy {
            surrounding_lines: self.context_surrounding_lines.unwrap_or(6),
            symbols: self.context_symbols.unwrap_or(12),
            diagnostics,
            related_slices: self.context_related_slices.unwrap_or(true),
            budget: self.context_budget.unwrap_or(8_000),
        };

        let agent_loop = AgentLoopConfig {
            max_tool_calls: self.max_tool_calls.filter(|limit| *limit > 0),
        };

        let retry = RetryPolicy {
            max: self.retry_max.unwrap_or(0),
            fallback: self.retry_fallback,
        };

        AiProfileConfig {
            name,
            provider,
            model: self.model,
            base_url: self.base_url,
            api_key: self.api_key,
            api_key_env: self.api_key_env.or_else(|| default_api_key_env(provider)),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            system_prompt: self.system_prompt,
            edit_format,
            chat_edit_format,
            context,
            agent_loop,
            tools: self.tools,
            scope,
            edit_prompt: self.edit_prompt,
            chat_prompt: self.chat_prompt,
            chat_edit_prompt: self.chat_edit_prompt,
            reasoning_effort: self.reasoning_effort,
            verbosity: self.verbosity,
            syntax_check: self.syntax_check,
            retry,
        }
    }
}

/// Snapshot of AI config from Lua bridge:
/// (contexts, default_profile, profiles, api_key_registry, prompts, format_prompts,
///  project_context, chat_context, agent_loop).
pub type AiConfigSnapshot = (
    HashMap<String, String>,
    Option<String>,
    HashMap<String, LuaProfileConfig>,
    HashMap<String, ApiKeyConfig>,
    HashMap<String, String>,
    HashMap<String, String>,
    ProjectContextConfig,
    ChatContextConfig,
    AgentLoopConfig,
);

/// Commands queued by Lua for the editor to process.
#[derive(Debug, Clone)]
pub enum AiCommand {
    OpenChat {
        name: Option<String>,
        profile: Option<String>,
        allow_edits: Option<bool>,
        system_prompt: Option<String>,
        initial_message: Option<String>,
    },
    EditSelection {
        profile: Option<String>,
    },
}

/// Value types that can be stored in vim.g
#[derive(Clone, Debug)]
pub enum GlobalValue {
    String(String),
    Number(f64),
    Integer(i64),
    Boolean(bool),
    Nil,
}

impl EditorBridge {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(EditorBridgeInner {
                pending_commands: Vec::new(),
                cursor_pos: None,
                buffer_content: None,
                mode: None,
                global_vars: HashMap::new(),
                ai_contexts: HashMap::new(),
                ai_default_profile: None,
                ai_profiles: HashMap::new(),
                ai_pending_commands: Vec::new(),
                api_key_registry: HashMap::new(),
                ai_prompts: HashMap::new(),
                format_prompts: HashMap::new(),
                project_context: ProjectContextConfig::default(),
                chat_context: ChatContextConfig::default(),
                agent_loop: AgentLoopConfig::default(),
                ai_dirty: false,
            })),
        }
    }

    /// Set a global variable (vim.g.name = value)
    pub fn set_global(&self, name: String, value: GlobalValue) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.global_vars.insert(name, value);
    }

    /// Get a global variable (vim.g.name)
    pub fn get_global(&self, name: &str) -> Option<GlobalValue> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.global_vars.get(name).cloned()
    }

    /// Queue a command to be executed on the editor
    pub fn execute_command(&self, command: String) -> Result<()> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.pending_commands.push(command);
        Ok(())
    }

    /// Update the cached cursor position
    pub fn update_cursor(&self, line: usize, column: usize) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.cursor_pos = Some((line, column));
    }

    /// Get the current cursor position
    pub fn get_cursor(&self) -> Option<(usize, usize)> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.cursor_pos
    }

    /// Update the cached buffer content
    pub fn update_buffer(&self, content: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.buffer_content = Some(content);
    }

    /// Get the current buffer content
    pub fn get_buffer(&self) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.buffer_content.clone()
    }

    /// Update the cached mode
    pub fn update_mode(&self, mode: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.mode = Some(mode);
    }

    /// Get the current mode
    pub fn get_mode(&self) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.mode.clone()
    }

    /// Get all pending commands and clear the queue
    pub fn drain_commands(&self) -> Vec<String> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut inner.pending_commands)
    }

    /// Get a specific line from the buffer
    pub fn get_line(&self, line: usize) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(ref content) = inner.buffer_content {
            content.lines().nth(line).map(|s| s.to_string())
        } else {
            None
        }
    }

    // -----------------------------------------------------------------
    // AI config bridge
    // -----------------------------------------------------------------

    pub fn set_ai_context(&self, name: String, profile: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_contexts.insert(name, profile);
        inner.ai_dirty = true;
    }

    pub fn get_ai_context(&self, name: &str) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_contexts.get(name).cloned()
    }

    pub fn set_ai_default_profile(&self, name: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_default_profile = Some(name);
        inner.ai_dirty = true;
    }

    pub fn get_ai_default_profile(&self) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_default_profile.clone()
    }

    pub fn register_ai_profile(&self, name: String, config: LuaProfileConfig) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_profiles.insert(name, config);
        inner.ai_dirty = true;
    }

    pub fn queue_ai_command(&self, cmd: AiCommand) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_pending_commands.push(cmd);
    }

    pub fn drain_ai_commands(&self) -> Vec<AiCommand> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut inner.ai_pending_commands)
    }

    pub fn register_api_key(&self, name: String, config: ApiKeyConfig) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.api_key_registry.insert(name, config);
        inner.ai_dirty = true;
    }

    pub fn set_ai_prompt(&self, name: String, template: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_prompts.insert(name, template);
        inner.ai_dirty = true;
    }

    pub fn get_ai_prompt(&self, name: &str) -> Option<String> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.ai_prompts.get(name).cloned()
    }

    pub fn register_format_prompt(&self, name: String, prompt: String) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.format_prompts.insert(name, prompt);
        inner.ai_dirty = true;
    }

    pub fn set_project_context(&self, config: ProjectContextConfig) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.project_context = config;
        inner.ai_dirty = true;
    }

    pub fn set_chat_context(&self, config: ChatContextConfig) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.chat_context = config;
        inner.ai_dirty = true;
    }

    pub fn set_agent_loop(&self, config: AgentLoopConfig) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.agent_loop = config;
        inner.ai_dirty = true;
    }

    /// Returns AI config if it was modified since last call.
    /// Clears the dirty flag.
    pub fn take_ai_config_if_dirty(&self) -> Option<AiConfigSnapshot> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !inner.ai_dirty {
            return None;
        }
        inner.ai_dirty = false;
        Some((
            inner.ai_contexts.clone(),
            inner.ai_default_profile.clone(),
            inner.ai_profiles.clone(),
            inner.api_key_registry.clone(),
            inner.ai_prompts.clone(),
            inner.format_prompts.clone(),
            inner.project_context.clone(),
            inner.chat_context.clone(),
            inner.agent_loop.clone(),
        ))
    }

    /// Get the number of lines in the buffer
    pub fn get_line_count(&self) -> usize {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(ref content) = inner.buffer_content {
            content.lines().count()
        } else {
            0
        }
    }
}

impl Default for EditorBridge {
    fn default() -> Self {
        Self::new()
    }
}
