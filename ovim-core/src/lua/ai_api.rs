//! `vim.ai.*` and `vim.api_keys.*` Lua API namespaces.

use super::editor_bridge::{AiCommand, EditorBridge, LuaProfileConfig};
use crate::ai::{AgentLoopConfig, ApiKeyConfig, ChatContextConfig, ProjectContextConfig};
use mlua::{Lua, Result, Table, Value};

/// Build the `vim.ai` table and all sub-namespaces.
pub fn setup_ai_api(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let ai = lua.create_table()?;

    // Global table for Lua-side format registrations (keeps function refs alive).
    lua.globals()
        .set("_ovim_format_registry", lua.create_table()?)?;

    // vim.ai.setup(opts)
    {
        let b = bridge.clone();
        let setup = lua.create_function(move |_lua, opts: Table| {
            // default_profile
            let configured_default_profile = opts.get::<_, String>("default_profile").ok();
            if let Some(ref dp) = configured_default_profile {
                b.set_ai_default_profile(dp.clone());
            }

            // contexts — string shorthand or table form
            let mut contexts_provided = false;
            if let Ok(contexts) = opts.get::<_, Table>("contexts") {
                contexts_provided = true;
                for pair in contexts.pairs::<String, Value>() {
                    let (key, val) = pair?;
                    let profile_name = match val {
                        Value::String(s) => s.to_str()?.to_string(),
                        Value::Table(t) => t.get::<_, String>("profile").map_err(|_| {
                            mlua::Error::external("contexts entry table must have 'profile' field")
                        })?,
                        _ => {
                            return Err(mlua::Error::external(format!(
                                "contexts.{}: expected string or table",
                                key
                            )));
                        }
                    };
                    b.set_ai_context(key, profile_name);
                }
            }
            // If user sets only default_profile, keep contexts aligned with it.
            // This lets user config override built-in context defaults without
            // requiring a full contexts table.
            if !contexts_provided {
                if let Some(dp) = configured_default_profile {
                    for ctx in ["chat", "query"] {
                        b.set_ai_context(ctx.to_string(), dp.clone());
                    }
                }
            }

            // profiles
            if let Ok(profiles) = opts.get::<_, Table>("profiles") {
                for pair in profiles.pairs::<String, Table>() {
                    let (name, tbl) = pair?;
                    let config = parse_lua_profile(&tbl)?;
                    b.register_ai_profile(name, config);
                }
            }

            Ok(())
        })?;
        ai.set("setup", setup)?;
    }

    // vim.ai.contexts — metatable with __index/__newindex
    {
        let contexts = lua.create_table()?;
        let meta = lua.create_table()?;

        let b = bridge.clone();
        let index = lua.create_function(move |lua, (_, key): (Value, String)| {
            match b.get_ai_context(&key) {
                Some(v) => Ok(Value::String(lua.create_string(&v)?)),
                None => Ok(Value::Nil),
            }
        })?;
        meta.set("__index", index)?;

        let b = bridge.clone();
        let newindex =
            lua.create_function(move |_lua, (_, key, value): (Value, String, String)| {
                b.set_ai_context(key, value);
                Ok(())
            })?;
        meta.set("__newindex", newindex)?;

        contexts.set_metatable(Some(meta));
        ai.set("contexts", contexts)?;
    }

    // vim.ai.default_profile — via metatable on ai table itself (handled at the end).

    // vim.ai.open_chat(opts)
    {
        let b = bridge.clone();
        let open_chat = lua.create_function(move |_lua, opts: Option<Table>| {
            let (name, profile, allow_edits, system_prompt, initial_message) =
                if let Some(ref opts) = opts {
                    (
                        opts.get::<_, String>("name").ok(),
                        opts.get::<_, String>("profile").ok(),
                        opts.get::<_, bool>("allow_edits").ok(),
                        opts.get::<_, String>("system_prompt").ok(),
                        opts.get::<_, String>("initial_message").ok(),
                    )
                } else {
                    (None, None, None, None, None)
                };

            b.queue_ai_command(AiCommand::OpenChat {
                name,
                profile,
                allow_edits,
                system_prompt,
                initial_message,
            });
            Ok(())
        })?;
        ai.set("open_chat", open_chat)?;
    }

    // vim.ai.edit_selection(opts)
    {
        let b = bridge.clone();
        let edit_selection = lua.create_function(move |_lua, opts: Option<Table>| {
            let profile = opts.and_then(|t| t.get::<_, String>("profile").ok());
            b.queue_ai_command(AiCommand::EditSelection { profile });
            Ok(())
        })?;
        ai.set("edit_selection", edit_selection)?;
    }

    // vim.ai.profiles.register(name, opts)
    {
        let profiles_table = lua.create_table()?;

        let b = bridge.clone();
        let register = lua.create_function(move |_lua, (name, opts): (String, Table)| {
            let config = parse_lua_profile(&opts)?;
            b.register_ai_profile(name, config);
            Ok(())
        })?;
        profiles_table.set("register", register)?;

        ai.set("profiles", profiles_table)?;
    }

    // vim.ai.prompts — metatable with __index/__newindex
    {
        let prompts = lua.create_table()?;
        let meta = lua.create_table()?;

        let b = bridge.clone();
        let index = lua.create_function(move |lua, (_, key): (Value, String)| {
            match b.get_ai_prompt(&key) {
                Some(v) => Ok(Value::String(lua.create_string(&v)?)),
                None => Ok(Value::Nil),
            }
        })?;
        meta.set("__index", index)?;

        let b = bridge.clone();
        let newindex =
            lua.create_function(move |_lua, (_, key, value): (Value, String, String)| {
                b.set_ai_prompt(key, value);
                Ok(())
            })?;
        meta.set("__newindex", newindex)?;

        prompts.set_metatable(Some(meta));
        ai.set("prompts", prompts)?;
    }

    // Stubs: vim.ai.models.register, vim.ai.tools.register
    {
        let models = lua.create_table()?;
        let models_register = lua.create_function(|_lua, (_name, _opts): (String, Table)| {
            // M4/M5 stub — register data, no behavior yet
            Ok(())
        })?;
        models.set("register", models_register)?;
        ai.set("models", models)?;

        let tools = lua.create_table()?;
        let tools_register = lua.create_function(|_lua, (_name, _opts): (String, Table)| Ok(()))?;
        tools.set("register", tools_register)?;
        ai.set("tools", tools)?;
    }

    // vim.ai.formats.register(name, opts) — real implementation
    {
        let formats_table = lua.create_table()?;

        let b = bridge.clone();
        let register = lua.create_function(move |lua, (name, opts): (String, Table)| {
            // Validate that opts.extract exists and is a function
            let _extract: mlua::Function = opts.get("extract").map_err(|_| {
                mlua::Error::external(format!(
                    "vim.ai.formats.register('{}', ...) requires an 'extract' function",
                    name
                ))
            })?;

            // Store full opts table (including function refs) in Lua-side global
            let registry: Table = lua.globals().get("_ovim_format_registry")?;
            registry.set(name.as_str(), opts.clone())?;

            // If opts.prompt is a string, sync to bridge for system prompt resolution
            if let Ok(prompt) = opts.get::<_, String>("prompt") {
                b.register_format_prompt(name, prompt);
            }

            Ok(())
        })?;
        formats_table.set("register", register)?;

        ai.set("formats", formats_table.clone())?;
        // Keep edit_formats as alias for backward compat
        ai.set("edit_formats", formats_table)?;
    }

    // vim.ai.workflows.{list,reload,run,status}
    {
        let workflows = lua.create_table()?;

        let b = bridge.clone();
        let list = lua.create_function(move |_lua, ()| {
            b.execute_command("workflow list".to_string())
                .map_err(mlua::Error::external)?;
            Ok(())
        })?;
        workflows.set("list", list)?;

        let b = bridge.clone();
        let reload = lua.create_function(move |_lua, ()| {
            b.execute_command("workflow reload".to_string())
                .map_err(mlua::Error::external)?;
            Ok(())
        })?;
        workflows.set("reload", reload)?;

        let b = bridge.clone();
        let status = lua.create_function(move |_lua, ()| {
            b.execute_command("workflow status".to_string())
                .map_err(mlua::Error::external)?;
            Ok(())
        })?;
        workflows.set("status", status)?;

        let b = bridge.clone();
        let run = lua.create_function(move |_lua, (name, opts): (String, Option<Table>)| {
            let mut cmd = format!("workflow run {}", name);
            if let Some(opts) = opts {
                for pair in opts.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    let mut encoded = match v {
                        Value::String(s) => serde_json::to_string(s.to_str().unwrap_or(""))
                            .unwrap_or_else(|_| "\"\"".to_string()),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Boolean(b) => b.to_string(),
                        Value::Nil => "null".to_string(),
                        _ => {
                            return Err(mlua::Error::external(format!(
                                "vim.ai.workflows.run: unsupported input type for key '{}'",
                                k
                            )));
                        }
                    };
                    encoded = encoded.replace(' ', "\\u0020");
                    cmd.push(' ');
                    cmd.push_str(&k);
                    cmd.push('=');
                    cmd.push_str(&encoded);
                }
            }
            b.execute_command(cmd).map_err(mlua::Error::external)?;
            Ok(())
        })?;
        workflows.set("run", run)?;

        ai.set("workflows", workflows)?;
    }

    // Metatable for vim.ai itself — handles default_profile as a virtual field.
    {
        let meta = lua.create_table()?;

        let b = bridge.clone();
        let index = lua.create_function(move |lua, (_, key): (Table, String)| {
            if key == "default_profile" {
                match b.get_ai_default_profile() {
                    Some(v) => Ok(Value::String(lua.create_string(&v)?)),
                    None => Ok(Value::Nil),
                }
            } else {
                Ok(Value::Nil)
            }
        })?;
        meta.set("__index", index)?;

        let b = bridge.clone();
        let newindex =
            lua.create_function(move |_lua, (tbl, key, value): (Table, String, Value)| {
                if key == "default_profile" {
                    let s = match value {
                        Value::String(s) => s.to_str()?.to_string(),
                        _ => {
                            return Err(mlua::Error::external(
                                "vim.ai.default_profile must be a string",
                            ));
                        }
                    };
                    b.set_ai_default_profile(s);
                    Ok(())
                } else if key == "project_context" || key == "chat" || key == "agent" {
                    match key.as_str() {
                        "project_context" => {
                            let cfg = parse_project_context_table(&value)?;
                            b.set_project_context(cfg);
                        }
                        "chat" => {
                            let cfg = parse_chat_context_table(&value)?;
                            b.set_chat_context(cfg);
                        }
                        "agent" => {
                            let cfg = parse_agent_loop_table(&value)?;
                            b.set_agent_loop(cfg);
                        }
                        _ => unreachable!(),
                    }
                    // Also rawset so Lua can read it back
                    tbl.raw_set(key, value)?;
                    Ok(())
                } else {
                    // Allow setting other fields normally via rawset
                    tbl.raw_set(key, value)?;
                    Ok(())
                }
            })?;
        meta.set("__newindex", newindex)?;

        ai.set_metatable(Some(meta));
    }

    Ok(ai)
}

/// Parse a Lua table into a LuaProfileConfig.
fn parse_lua_profile(tbl: &Table) -> Result<LuaProfileConfig> {
    let model: String = tbl
        .get::<_, String>("model")
        .map_err(|_| mlua::Error::external("profile must have a 'model' field"))?;

    // Read optional context sub-table fields
    let (ctx_surrounding_lines, ctx_symbols, ctx_diagnostics, ctx_related_slices, ctx_budget) =
        if let Ok(ctx) = tbl.get::<_, Table>("context") {
            (
                ctx.get::<_, u16>("surrounding_lines").ok(),
                ctx.get::<_, u16>("symbols").ok(),
                ctx.get::<_, String>("diagnostics").ok(),
                ctx.get::<_, bool>("related_slices").ok(),
                ctx.get::<_, usize>("budget").ok(),
            )
        } else {
            (None, None, None, None, None)
        };

    Ok(LuaProfileConfig {
        model,
        provider: tbl.get::<_, String>("provider").ok(),
        base_url: tbl.get::<_, String>("base_url").ok(),
        api_key: tbl.get::<_, String>("api_key").ok(),
        api_key_env: tbl.get::<_, String>("api_key_env").ok(),
        temperature: tbl.get::<_, f32>("temperature").ok(),
        max_tokens: tbl.get::<_, u32>("max_tokens").ok(),
        system_prompt: tbl.get::<_, String>("system_prompt").ok(),
        tools: parse_string_list(tbl, "tools"),
        scope: tbl.get::<_, String>("scope").ok(),
        scope_shell: tbl.get::<_, bool>("scope_shell").unwrap_or(false),
        scope_network: tbl.get::<_, bool>("scope_network").unwrap_or(false),
        edit_format: tbl.get::<_, String>("edit_format").ok(),
        chat_edit_format: tbl.get::<_, String>("chat_edit_format").ok(),
        context_surrounding_lines: ctx_surrounding_lines,
        context_symbols: ctx_symbols,
        context_diagnostics: ctx_diagnostics,
        context_related_slices: ctx_related_slices,
        context_budget: ctx_budget,
        max_tool_calls: tbl.get::<_, u64>("max_tool_calls").ok(),
        edit_prompt: tbl.get::<_, String>("edit_prompt").ok(),
        chat_prompt: tbl.get::<_, String>("chat_prompt").ok(),
        chat_edit_prompt: tbl.get::<_, String>("chat_edit_prompt").ok(),
        reasoning_effort: tbl.get::<_, String>("reasoning_effort").ok(),
        verbosity: tbl.get::<_, String>("verbosity").ok(),
        syntax_check: tbl.get::<_, bool>("syntax_check").ok(),
        retry_max: tbl.get::<_, u8>("retry_max").ok(),
        retry_fallback: tbl.get::<_, String>("retry_fallback").ok(),
    })
}

/// Build the `vim.api_keys` table.
pub fn setup_api_keys_api(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let api_keys = lua.create_table()?;

    // vim.api_keys.register(name, { env_var?, file? })
    let register = lua.create_function(move |_lua, (name, opts): (String, Table)| {
        let config = ApiKeyConfig {
            env_var: opts.get::<_, String>("env_var").ok(),
            file: opts.get::<_, String>("file").ok(),
        };
        if config.env_var.is_none() && config.file.is_none() {
            return Err(mlua::Error::external(
                "vim.api_keys.register: must specify at least one of 'env_var' or 'file'",
            ));
        }
        bridge.register_api_key(name, config);
        Ok(())
    })?;
    api_keys.set("register", register)?;

    Ok(api_keys)
}

/// Extract a string list from a Lua table field (returns empty vec on missing/invalid).
fn parse_string_list(tbl: &Table, key: &str) -> Vec<String> {
    tbl.get::<_, Table>(key)
        .ok()
        .map(|t| {
            t.sequence_values::<String>()
                .filter_map(|r| r.ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a Lua value (expected table) into ProjectContextConfig.
fn parse_project_context_table(value: &Value) -> Result<ProjectContextConfig> {
    let tbl = match value {
        Value::Table(t) => t,
        _ => {
            return Err(mlua::Error::external(
                "vim.ai.project_context must be a table",
            ))
        }
    };
    let mut cfg = ProjectContextConfig::default();
    if let Ok(files) = tbl.get::<_, Table>("files") {
        cfg.files = files
            .sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect();
    }
    if let Ok(v) = tbl.get::<_, bool>("hierarchical") {
        cfg.hierarchical = v;
    }
    if let Ok(v) = tbl.get::<_, usize>("budget") {
        cfg.budget = v;
    }
    if let Ok(v) = tbl.get::<_, bool>("enabled") {
        cfg.enabled = v;
    }
    Ok(cfg)
}

/// Parse a Lua value (expected table) into ChatContextConfig.
fn parse_chat_context_table(value: &Value) -> Result<ChatContextConfig> {
    let tbl = match value {
        Value::Table(t) => t,
        _ => return Err(mlua::Error::external("vim.ai.chat must be a table")),
    };
    let mut cfg = ChatContextConfig::default();
    if let Ok(v) = tbl.get::<_, usize>("observation_window") {
        cfg.observation_window = v;
    }
    if let Ok(v) = tbl.get::<_, String>("mask_template") {
        cfg.mask_template = v;
    }
    if let Ok(v) = tbl.get::<_, usize>("max_context_tokens") {
        cfg.max_context_tokens = v;
    }
    Ok(cfg)
}

/// Parse a Lua value (expected table) into AgentLoopConfig.
fn parse_agent_loop_table(value: &Value) -> Result<AgentLoopConfig> {
    let tbl = match value {
        Value::Table(t) => t,
        _ => return Err(mlua::Error::external("vim.ai.agent must be a table")),
    };
    let mut cfg = AgentLoopConfig::default();
    if let Ok(v) = tbl.get::<_, u64>("max_tool_calls") {
        cfg.max_tool_calls = (v > 0).then_some(v);
    }
    Ok(cfg)
}
