//! Strict, catalog-derived parent control-tool contracts.

use super::{SideEffect, StrictJsonSchema, ToolDefinition, ToolSchemaError};
use crate::agent_runtime::{AgentCapability, ScopedTool, SubagentModelCatalog};
use crate::ai::{AiSubagentConfig, FileScope, RequiredScope};
use crate::run_log::ToolSideEffect;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const SPAWN_AGENT_TOOL: &str = "spawn_agent";
pub const LIST_AGENTS_TOOL: &str = "list_agents";
pub const WAIT_AGENT_TOOL: &str = "wait_agent";
pub const INTERRUPT_AGENT_TOOL: &str = "interrupt_agent";
pub const SEND_MESSAGE_TOOL: &str = "send_message";
pub const FOLLOWUP_AGENT_TOOL: &str = "followup_agent";

pub fn is_parent_control_tool(name: &str) -> bool {
    matches!(
        name,
        SPAWN_AGENT_TOOL
            | LIST_AGENTS_TOOL
            | WAIT_AGENT_TOOL
            | INTERRUPT_AGENT_TOOL
            | SEND_MESSAGE_TOOL
            | FOLLOWUP_AGENT_TOOL
    )
}

pub fn parent_control_tools(
    catalog: &SubagentModelCatalog,
    policy: &AiSubagentConfig,
) -> Result<Vec<ToolDefinition>, ToolSchemaError> {
    let entries = catalog
        .entries()
        .filter(|entry| entry.available && entry.supports_tools)
        .collect::<Vec<_>>();
    let models = entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(ToolSchemaError::EmptyStringEnum);
    }
    let efforts = entries
        .iter()
        .flat_map(|entry| entry.supported_reasoning_efforts.iter())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let roles = policy
        .allowed_agent_kinds
        .iter()
        .filter(|role| matches!(role.as_str(), "explorer" | "reviewer"))
        .cloned()
        .collect::<Vec<_>>();
    if roles.is_empty() {
        return Err(ToolSchemaError::EmptyStringEnum);
    }
    let pairing = entries
        .iter()
        .map(|entry| {
            format!(
                "{} => [{}]",
                entry.id,
                entry
                    .supported_reasoning_efforts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    Ok(vec![
        definition(
            SPAWN_AGENT_TOOL,
            format!(
                "Dispatch one bounded independent read-only task and return its durable task and agent IDs immediately. Model and reasoning_effort are required and must be an advertised pair ({pairing}). Delegate independent research, review, or verification; avoid duplicate, tiny, or sequential work, and continue the local critical path while the child runs."
            ),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "task_name": {
                        "type": "string",
                        "pattern": "^[a-z][a-z0-9_]{0,63}$",
                        "description": "Stable unique task name within this root run."
                    },
                    "objective": { "type": "string", "minLength": 1, "maxLength": 8192 },
                    "agent_kind": { "type": "string", "enum": roles },
                    "model": { "type": "string", "enum": models },
                    "reasoning_effort": { "type": "string", "enum": efforts },
                    "context_mode": { "type": "string", "enum": ["brief"] },
                    "expected_output": {
                        "type": "string",
                        "enum": ["analysis", "review_report", "verification"]
                    },
                    "relevant_paths": string_array(32, 1024),
                    "done_when": string_array(16, 1024),
                    "non_goals": string_array(16, 1024),
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": policy.default_timeout_seconds
                    }
                },
                "required": [
                    "task_name", "objective", "agent_kind", "model", "reasoning_effort",
                    "context_mode", "expected_output", "relevant_paths", "done_when",
                    "non_goals", "timeout_seconds"
                ]
            }))?,
        ),
        definition(
            LIST_AGENTS_TOOL,
            "List this parent run's durable delegated tasks, routing, current state, workspace identity, and pending attention without waiting.".into(),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }))?,
        ),
        definition(
            WAIT_AGENT_TOOL,
            "Subscribe to this parent's durable mailbox until a child update, user steering, or the bounded deadline. The editor remains responsive while this delegated tool is parked.".into(),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 60 }
                },
                "required": ["timeout_seconds"]
            }))?,
        ),
        definition(
            SEND_MESSAGE_TOOL,
            "Queue one bounded parent message to a live delegated child. Delivery occurs at a safe provider/tool boundary and does not start a new turn; queued or terminal targets reject the message.".into(),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "agent_id": { "type": "string", "pattern": "^agt_.+" },
                    "message": { "type": "string", "minLength": 1, "maxLength": 8192 }
                },
                "required": ["agent_id", "message"]
            }))?,
        ),
        definition(
            FOLLOWUP_AGENT_TOOL,
            "Start a fresh turn on one completed or interrupted delegated child while preserving its agent identity, model route, workspace, capability ceiling, and budget ceiling. The provider session is reused only when it explicitly supports safe follow-up; otherwise the child receives bounded prior-handoff context in a fresh session.".into(),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "agent_id": { "type": "string", "pattern": "^agt_.+" },
                    "objective": { "type": "string", "minLength": 1, "maxLength": 8192 }
                },
                "required": ["agent_id", "objective"]
            }))?,
        ),
        definition(
            INTERRUPT_AGENT_TOOL,
            "Interrupt a delegated task and every nonterminal descendant while retaining durable partial history.".into(),
            strict(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "agent_id": { "type": "string", "pattern": "^agt_.+" },
                    "reason": { "type": "string", "minLength": 1, "maxLength": 1024 }
                },
                "required": ["agent_id", "reason"]
            }))?,
        ),
    ])
}

/// Provider-facing equivalents of the root controls for a delegated parent.
/// The runtime still decides whether a particular child has depth and
/// capability remaining; this function only centralizes the strict contracts
/// so root and nested agents cannot drift onto subtly different schemas.
pub fn delegated_parent_control_tools(
    catalog: &SubagentModelCatalog,
    policy: &AiSubagentConfig,
) -> Result<Vec<ScopedTool>, ToolSchemaError> {
    parent_control_tools(catalog, policy)?
        .into_iter()
        .map(|definition| {
            let input_schema = definition.custom_input_schema.ok_or_else(|| {
                ToolSchemaError::StrictObjectRequired(format!(
                    "delegated control {} has no strict schema",
                    definition.name
                ))
            })?;
            let side_effect = match definition.name.as_str() {
                LIST_AGENTS_TOOL | WAIT_AGENT_TOOL => ToolSideEffect::Read,
                _ => ToolSideEffect::External,
            };
            Ok(ScopedTool {
                name: definition.name,
                description: definition.description,
                input_schema,
                side_effect,
                required_capability: AgentCapability::DispatchAgents,
                requires_approval: false,
            })
        })
        .collect()
}

fn definition(name: &str, description: String, schema: StrictJsonSchema) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description,
        required_scope: RequiredScope {
            file_scope: FileScope::File,
            shell: false,
            network: false,
        },
        side_effect: SideEffect::Read,
        custom_input_schema: Some(schema),
        parameters: Vec::new(),
    }
}

fn strict(value: Value) -> Result<StrictJsonSchema, ToolSchemaError> {
    StrictJsonSchema::new(value)
}

fn string_array(max_items: usize, max_length: usize) -> Value {
    json!({
        "type": "array",
        "maxItems": max_items,
        "items": { "type": "string", "minLength": 1, "maxLength": max_length }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{catalog_model_id, ReasoningEffort};
    use crate::ai::tools::schema::tools_to_openai_schema;
    use crate::ai::{AiConfig, AiProviderKind};

    fn configured() -> (AiConfig, SubagentModelCatalog) {
        let mut config = AiConfig::default();
        config.subagents.enabled = true;
        let profile = config.profiles.get_mut("local").unwrap();
        profile.provider = AiProviderKind::OpenAi;
        profile.model = "test-model".into();
        profile.reasoning_effort = Some(ReasoningEffort::high().to_string());
        let catalog = SubagentModelCatalog::from_config(&config).unwrap();
        (config, catalog)
    }

    #[test]
    fn spawn_schema_is_dynamic_strict_and_requires_every_contract_field() {
        let (config, catalog) = configured();
        let tools = parent_control_tools(&catalog, &config.subagents).unwrap();
        let spawn = tools
            .iter()
            .find(|tool| tool.name == SPAWN_AGENT_TOOL)
            .unwrap();
        let schema = &tools_to_openai_schema(&[spawn])[0]["function"]["parameters"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["model"]["enum"],
            json!([catalog_model_id("local", "test-model")])
        );
        assert_eq!(
            schema["properties"]["reasoning_effort"]["enum"],
            json!(["high"])
        );
        let required = schema["required"].as_array().unwrap();
        for field in [
            "task_name",
            "objective",
            "agent_kind",
            "model",
            "reasoning_effort",
            "context_mode",
            "expected_output",
            "relevant_paths",
            "done_when",
            "non_goals",
            "timeout_seconds",
        ] {
            assert!(
                required.iter().any(|required| required == field),
                "missing {field}"
            );
        }
    }

    #[test]
    fn delegated_parent_controls_reuse_root_contracts_and_require_dispatch_capability() {
        let (config, catalog) = configured();
        let root = parent_control_tools(&catalog, &config.subagents).unwrap();
        let delegated = delegated_parent_control_tools(&catalog, &config.subagents).unwrap();
        assert_eq!(delegated.len(), root.len());
        for (root, delegated) in root.iter().zip(&delegated) {
            assert_eq!(delegated.name, root.name);
            assert_eq!(
                delegated.input_schema,
                root.custom_input_schema.clone().unwrap()
            );
            assert_eq!(
                delegated.required_capability,
                AgentCapability::DispatchAgents
            );
            assert!(!delegated.requires_approval);
        }
        assert_eq!(
            delegated
                .iter()
                .find(|tool| tool.name == SPAWN_AGENT_TOOL)
                .unwrap()
                .side_effect,
            ToolSideEffect::External
        );
        assert_eq!(
            delegated
                .iter()
                .find(|tool| tool.name == LIST_AGENTS_TOOL)
                .unwrap()
                .side_effect,
            ToolSideEffect::Read
        );
    }

    #[test]
    fn spawn_schema_rejects_missing_fields_unknown_fields_and_bad_shapes() {
        let (config, catalog) = configured();
        let tools = parent_control_tools(&catalog, &config.subagents).unwrap();
        let schema = tools
            .iter()
            .find(|tool| tool.name == SPAWN_AGENT_TOOL)
            .unwrap()
            .custom_input_schema
            .as_ref()
            .unwrap();
        assert!(schema.validate_instance(&json!({})).is_err());
        let valid = json!({
            "task_name": "inspect_store",
            "objective": "Inspect durable state",
            "agent_kind": "explorer",
            "model": catalog_model_id("local", "test-model"),
            "reasoning_effort": "high",
            "context_mode": "brief",
            "expected_output": "analysis",
            "relevant_paths": ["ovim-core/src"],
            "done_when": ["Evidence is cited"],
            "non_goals": ["Do not edit"],
            "timeout_seconds": 60
        });
        assert!(schema.validate_instance(&valid).is_ok());
        let mut unknown = valid;
        unknown["surprise"] = json!(true);
        assert!(schema.validate_instance(&unknown).is_err());
    }

    #[test]
    fn send_message_schema_is_bounded_and_strict() {
        let (config, catalog) = configured();
        let tools = parent_control_tools(&catalog, &config.subagents).unwrap();
        let schema = tools
            .iter()
            .find(|tool| tool.name == SEND_MESSAGE_TOOL)
            .unwrap()
            .custom_input_schema
            .as_ref()
            .unwrap();
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "message": "Inspect the restart edge."
            }))
            .is_ok());
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "message": "",
            }))
            .is_err());
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "message": "ok",
                "start_new_turn": true,
            }))
            .is_err());
    }

    #[test]
    fn followup_schema_is_bounded_and_strict() {
        let (config, catalog) = configured();
        let tools = parent_control_tools(&catalog, &config.subagents).unwrap();
        let schema = tools
            .iter()
            .find(|tool| tool.name == FOLLOWUP_AGENT_TOOL)
            .unwrap()
            .custom_input_schema
            .as_ref()
            .unwrap();
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "objective": "Verify the restart edge."
            }))
            .is_ok());
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "objective": "",
            }))
            .is_err());
        assert!(schema
            .validate_instance(&json!({
                "agent_id": "agt_child",
                "objective": "Continue",
                "model": "do-not-reroute",
            }))
            .is_err());
    }
}
