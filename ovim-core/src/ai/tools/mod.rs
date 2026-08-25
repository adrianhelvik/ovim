pub mod browser;
pub mod builtins;
pub mod schema;
pub mod subagents;

use std::collections::HashMap;
use std::fmt;

use crate::ai::config::AiProfileConfig;
use crate::ai::scope::{Capabilities, RequiredScope};
use serde_json::Value;

/// How a tool affects state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    Read,
    /// Navigation tools change editor state (open files, move cursor, select)
    /// but don't edit text. Always allowed, even when edits are disabled.
    Navigation,
    Mutation,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeService {
    Browser,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeServices {
    pub browser: bool,
}

impl RuntimeServices {
    fn contains(self, service: RuntimeService) -> bool {
        match service {
            RuntimeService::Browser => self.browser,
        }
    }
}

/// Parameter type for JSON schema generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamType {
    String,
    StringEnum(StringEnum),
    StringArray,
    Integer,
    Boolean,
    FilePath,
    LineNumber,
    LineRange,
    /// Ordered code-walkthrough entries with a path, line range, and comment.
    CodeExplanationSteps,
    /// Atomic file operations staged for an explained change set.
    ChangeSet,
    /// Ordered source-reference and proposed-change walkthrough entries.
    TalkThroughChangesSteps,
}

/// A non-empty, duplicate-free set of exact string choices for a tool field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringEnum(Vec<String>);

impl StringEnum {
    pub fn new(
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, ToolSchemaError> {
        let values = values.into_iter().map(Into::into).collect::<Vec<_>>();
        if values.is_empty() {
            return Err(ToolSchemaError::EmptyStringEnum);
        }
        let mut seen = std::collections::BTreeSet::new();
        for value in &values {
            if value.is_empty() {
                return Err(ToolSchemaError::EmptyStringEnumValue);
            }
            if !seen.insert(value) {
                return Err(ToolSchemaError::DuplicateStringEnumValue(value.clone()));
            }
        }
        Ok(Self(values))
    }

    pub fn values(&self) -> &[String] {
        &self.0
    }
}

/// A prevalidated object schema for tools whose dynamic contract cannot be
/// represented as a flat parameter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictJsonSchema(Value);

impl StrictJsonSchema {
    pub fn new(schema: Value) -> Result<Self, ToolSchemaError> {
        jsonschema::validator_for(&schema)
            .map_err(|error| ToolSchemaError::InvalidJsonSchema(error.to_string()))?;
        validate_strict_object_schemas(&schema, "$".into())?;
        let object = schema.as_object().ok_or_else(|| {
            ToolSchemaError::StrictObjectRequired("root schema is not an object".into())
        })?;
        if object.get("type") != Some(&Value::String("object".into())) {
            return Err(ToolSchemaError::StrictObjectRequired(
                "root schema must declare type: object".into(),
            ));
        }
        validate_required_properties(object, "$".into())?;
        Ok(Self(schema))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Validate one provider-supplied argument object against this schema.
    ///
    /// Tool schemas are trusted configuration, while instances are untrusted
    /// model output. Keeping validation on the prevalidated schema makes it
    /// difficult for a caller to accidentally execute malformed arguments.
    pub fn validate_instance(&self, instance: &Value) -> Result<(), String> {
        let validator = jsonschema::validator_for(&self.0)
            .map_err(|error| format!("invalid registered tool schema: {error}"))?;
        let errors = validator
            .iter_errors(instance)
            .map(|error| error.to_string())
            .take(4)
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn validate_strict_object_schemas(schema: &Value, path: String) -> Result<(), ToolSchemaError> {
    match schema {
        Value::Object(object) => {
            if object.get("type") == Some(&Value::String("object".into())) {
                if object.get("additionalProperties") != Some(&Value::Bool(false)) {
                    return Err(ToolSchemaError::StrictObjectRequired(format!(
                        "{path} must set additionalProperties: false"
                    )));
                }
                validate_required_properties(object, path.clone())?;
            }
            for (key, value) in object {
                validate_strict_object_schemas(value, format!("{path}/{key}"))?;
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_strict_object_schemas(value, format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_required_properties(
    object: &serde_json::Map<String, Value>,
    path: String,
) -> Result<(), ToolSchemaError> {
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ToolSchemaError::StrictObjectRequired(format!(
                "{path} must declare an object properties map"
            ))
        })?;
    let Some(required) = object.get("required") else {
        return Ok(());
    };
    let required = required.as_array().ok_or_else(|| {
        ToolSchemaError::StrictObjectRequired(format!("{path}/required must be an array"))
    })?;
    let mut seen = std::collections::BTreeSet::new();
    for value in required {
        let name = value.as_str().ok_or_else(|| {
            ToolSchemaError::StrictObjectRequired(format!(
                "{path}/required must contain only strings"
            ))
        })?;
        if !properties.contains_key(name) {
            return Err(ToolSchemaError::StrictObjectRequired(format!(
                "{path}/required names missing property {name:?}"
            )));
        }
        if !seen.insert(name) {
            return Err(ToolSchemaError::StrictObjectRequired(format!(
                "{path}/required repeats property {name:?}"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSchemaError {
    EmptyStringEnum,
    EmptyStringEnumValue,
    DuplicateStringEnumValue(String),
    InvalidJsonSchema(String),
    StrictObjectRequired(String),
}

impl fmt::Display for ToolSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStringEnum => formatter.write_str("string enum has no values"),
            Self::EmptyStringEnumValue => {
                formatter.write_str("string enum contains an empty value")
            }
            Self::DuplicateStringEnumValue(value) => {
                write!(formatter, "string enum repeats value {value:?}")
            }
            Self::InvalidJsonSchema(detail) => write!(formatter, "invalid JSON schema: {detail}"),
            Self::StrictObjectRequired(detail) => {
                write!(formatter, "JSON schema is not strict: {detail}")
            }
        }
    }
}

impl std::error::Error for ToolSchemaError {}

/// A single tool parameter.
#[derive(Debug, Clone)]
pub struct ToolParam {
    pub name: String,
    pub param_type: ParamType,
    pub required: bool,
    pub description: String,
}

/// Definition of a tool that can be invoked by the AI.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub required_scope: RequiredScope,
    pub side_effect: SideEffect,
    /// Overrides `parameters` when a tool needs a nested or dynamically
    /// generated contract. Construction proves that every object is closed.
    pub custom_input_schema: Option<StrictJsonSchema>,
    pub parameters: Vec<ToolParam>,
}

/// A pending tool invocation with validated scope.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub enum ToolResult {
    Success(String),
    Error(String),
}

/// Registry of all available tools.
pub struct ToolRegistry {
    tools: HashMap<String, ToolDefinition>,
    runtime_requirements: HashMap<String, RuntimeService>,
}

impl ToolRegistry {
    /// Create a new registry with all built-in tools pre-registered.
    pub fn new() -> Self {
        let mut reg = Self::empty();
        builtins::register_builtins(&mut reg);
        browser::register_browser_tools(&mut reg);
        reg
    }

    fn empty() -> Self {
        Self {
            tools: HashMap::new(),
            runtime_requirements: HashMap::new(),
        }
    }

    /// Register a tool definition.
    pub fn register(&mut self, tool: ToolDefinition) {
        self.runtime_requirements.remove(&tool.name);
        self.tools.insert(tool.name.clone(), tool);
    }

    pub fn register_for_service(&mut self, tool: ToolDefinition, service: RuntimeService) {
        self.runtime_requirements.insert(tool.name.clone(), service);
        self.tools.insert(tool.name.clone(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// Return all tools whose required scope fits within the given capabilities.
    pub fn tools_for_scope(&self, caps: &Capabilities) -> Vec<&ToolDefinition> {
        self.tools_for_scope_with_services(caps, RuntimeServices::default())
    }

    pub fn tools_for_scope_with_services(
        &self,
        caps: &Capabilities,
        services: RuntimeServices,
    ) -> Vec<&ToolDefinition> {
        self.tools
            .values()
            .filter(|tool| {
                self.runtime_requirement_is_available(&tool.name, services)
                    && caps.contains(&tool.required_scope)
                    && caps.allows_side_effect(tool.side_effect)
            })
            .collect()
    }

    /// Filter tools by a profile's tool list AND scope capabilities.
    /// If the profile's tool list is empty, all scope-matching tools are returned.
    pub fn tools_for_profile(
        &self,
        profile: &AiProfileConfig,
        caps: &Capabilities,
    ) -> Vec<&ToolDefinition> {
        self.tools_for_profile_with_services(profile, caps, RuntimeServices::default())
    }

    pub fn tools_for_profile_with_services(
        &self,
        profile: &AiProfileConfig,
        caps: &Capabilities,
        services: RuntimeServices,
    ) -> Vec<&ToolDefinition> {
        self.tools
            .values()
            .filter(|t| {
                if !self.runtime_requirement_is_available(&t.name, services) {
                    return false;
                }
                // Must fit within capabilities (scope + side effect)
                if !caps.contains(&t.required_scope) || !caps.allows_side_effect(t.side_effect) {
                    return false;
                }
                // If profile has an explicit tool list, tool must be in it
                if !profile.tools.is_empty() && !profile.tools.contains(&t.name) {
                    return false;
                }
                true
            })
            .collect()
    }

    fn runtime_requirement_is_available(&self, tool_name: &str, services: RuntimeServices) -> bool {
        self.runtime_requirements
            .get(tool_name)
            .is_none_or(|required| services.contains(*required))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::FileScope;

    fn make_tool(name: &str, file_scope: FileScope, side_effect: SideEffect) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("Test tool {name}"),
            required_scope: RequiredScope {
                file_scope,
                shell: false,
                network: false,
            },
            side_effect,
            custom_input_schema: None,
            parameters: vec![],
        }
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = ToolRegistry::empty();
        reg.register(make_tool("read_file", FileScope::File, SideEffect::Read));
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn tools_for_scope_filters_correctly() {
        let mut reg = ToolRegistry::empty();
        reg.register(make_tool("read_file", FileScope::File, SideEffect::Read));
        reg.register(make_tool(
            "search_project",
            FileScope::Project,
            SideEffect::Read,
        ));
        reg.register(make_tool(
            "run_shell",
            FileScope::File,
            SideEffect::External,
        ));

        // File scope, no shell
        let caps = Capabilities {
            file_scope: FileScope::File,
            shell: false,
            network: false,
            allow_mutations: true,
        };
        let tools = reg.tools_for_scope(&caps);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"search_project")); // Needs Project scope
        assert!(!names.contains(&"run_shell")); // External side effect blocked (shell=false)
    }

    #[test]
    fn tools_for_profile_respects_tool_list() {
        let mut reg = ToolRegistry::empty();
        reg.register(make_tool("read_file", FileScope::File, SideEffect::Read));
        reg.register(make_tool(
            "read_selection",
            FileScope::File,
            SideEffect::Read,
        ));

        let caps = Capabilities {
            file_scope: FileScope::File,
            shell: false,
            network: false,
            allow_mutations: true,
        };

        // Profile with explicit tool list
        let mut profile = AiProfileConfig {
            name: "test".to_string(),
            provider: crate::ai::types::AiProviderKind::Ollama,
            model: "test".to_string(),
            base_url: None,
            api_key: None,
            api_key_env: None,
            temperature: None,
            max_tokens: None,
            system_prompt: None,
            edit_format: crate::ai::types::EditFormat::Json,
            chat_edit_format: None,
            context: crate::ai::types::ContextGatheringPolicy::default(),
            agent_loop: crate::ai::types::AgentLoopConfig::default(),
            tools: vec!["read_file".to_string()],
            scope: crate::ai::types::ProfileScope::default(),
            edit_prompt: None,
            chat_prompt: None,
            chat_edit_prompt: None,
            reasoning_effort: None,
            verbosity: None,
            syntax_check: None,
            retry: crate::ai::types::RetryPolicy::default(),
        };
        let tools = reg.tools_for_profile(&profile, &caps);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");

        // Empty tool list = all matching tools
        profile.tools.clear();
        let tools = reg.tools_for_profile(&profile, &caps);
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn default_registry_has_builtins() {
        let reg = ToolRegistry::new();
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("read_file_at_path").is_some());
        assert!(reg.get("view_image").is_some());
        assert!(reg.get("read_selection").is_some());
        assert!(reg.get("read_diagnostics").is_some());
        assert!(reg.get("read_project_diagnostics").is_some());
        assert!(reg.get("search_project").is_some());
        assert!(reg.get("list_files").is_some());
        // LSP tools
        assert!(reg.get("document_symbols").is_some());
        assert!(reg.get("hover").is_some());
        assert!(reg.get("goto_definition").is_some());
        // Navigation tools
        assert!(reg.get("open_file").is_some());
        assert!(reg.get("select_text").is_some());
        // External tools
        assert!(reg.get("bash").is_some());
        // Optional frontend tools are registered but catalog-gated.
        assert!(reg.get(browser::BROWSER_SESSION_TOOL).is_some());
        assert!(reg.get(browser::BROWSER_NAVIGATE_TOOL).is_some());
        assert!(reg.get(browser::BROWSER_SNAPSHOT_TOOL).is_some());
        assert!(reg.get(browser::BROWSER_ACT_TOOL).is_some());
        // Mutation tools
        assert!(reg.get("edit_range").is_some());
        assert!(reg.get("insert_lines").is_some());
        assert!(reg.get("delete_lines").is_some());
        assert!(reg.get("write_file_at_path").is_some());
        assert!(reg.get("create_file").is_some());
        assert!(reg.get("apply_patch_at_path").is_some());
        assert!(reg.get("snapshot_file").is_some());
        assert!(reg.get("restore_file").is_some());
    }

    #[test]
    fn mutation_tools_excluded_when_allow_edits_false() {
        let reg = ToolRegistry::new();
        let caps = Capabilities {
            file_scope: FileScope::Project,
            shell: true,
            network: true,
            allow_mutations: false,
        };
        let tools = reg.tools_for_scope(&caps);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        // Read tools present
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"search_project"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"read_project_diagnostics"));
        // Navigation tools present (always allowed)
        assert!(names.contains(&"open_file"));
        assert!(names.contains(&"select_text"));
        // Mutation tools excluded
        assert!(!names.contains(&"edit_range"));
        assert!(!names.contains(&"insert_lines"));
        assert!(!names.contains(&"delete_lines"));
        assert!(!names.contains(&"write_file_at_path"));
        assert!(!names.contains(&"create_file"));
        assert!(!names.contains(&"apply_patch_at_path"));
        assert!(!names.contains(&"snapshot_file"));
        assert!(!names.contains(&"restore_file"));
    }

    #[test]
    fn mutation_tools_included_when_allow_edits_true() {
        let reg = ToolRegistry::new();
        let caps = Capabilities {
            file_scope: FileScope::Project,
            shell: true,
            network: true,
            allow_mutations: true,
        };
        let tools = reg.tools_for_scope(&caps);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"open_file"));
        assert!(names.contains(&"select_text"));
        assert!(names.contains(&"edit_range"));
        assert!(names.contains(&"insert_lines"));
        assert!(names.contains(&"delete_lines"));
        assert!(names.contains(&"write_file_at_path"));
        assert!(names.contains(&"create_file"));
        assert!(names.contains(&"apply_patch_at_path"));
        assert!(names.contains(&"snapshot_file"));
        assert!(names.contains(&"restore_file"));
    }

    #[test]
    fn runtime_services_gate_browser_tools_independently_of_scope() {
        let reg = ToolRegistry::new();
        let caps = Capabilities {
            file_scope: FileScope::Project,
            shell: true,
            network: true,
            allow_mutations: true,
        };

        let without_browser = reg.tools_for_scope(&caps);
        assert!(!without_browser
            .iter()
            .any(|tool| browser::is_browser_tool(&tool.name)));

        let with_browser =
            reg.tools_for_scope_with_services(&caps, RuntimeServices { browser: true });
        for expected in [
            browser::BROWSER_SESSION_TOOL,
            browser::BROWSER_NAVIGATE_TOOL,
            browser::BROWSER_SNAPSHOT_TOOL,
            browser::BROWSER_ACT_TOOL,
        ] {
            assert!(with_browser.iter().any(|tool| tool.name == expected));
        }
    }
}
