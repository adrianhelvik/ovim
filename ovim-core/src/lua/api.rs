use crate::lua::editor_bridge::GlobalValue;
use crate::lua::EditorBridge;
use anyhow::Result;
use mlua::{Lua, Table, Value};

/// Sets up the vim global table with API namespaces
pub fn setup_vim_api(lua: &Lua, bridge: EditorBridge) -> Result<()> {
    let vim = lua.create_table()?;

    // Create vim.api namespace
    let api = create_api_table(lua, bridge.clone())?;
    vim.set("api", api)?;

    // Create vim.fn namespace
    let fn_table = create_fn_table(lua, bridge.clone())?;
    vim.set("fn", fn_table)?;

    // Create vim.cmd function
    let cmd = create_cmd_function(lua, bridge.clone())?;
    vim.set("cmd", cmd)?;

    // Create vim.keymap namespace
    let keymap = create_keymap_table(lua, bridge.clone())?;
    vim.set("keymap", keymap)?;

    // Create vim.g (global variables) namespace with metatable
    let g = create_g_table(lua, bridge.clone())?;
    vim.set("g", g)?;

    // Create vim.opt (options) namespace with metatable
    let opt = create_opt_table(lua, bridge.clone())?;
    vim.set("opt", opt)?;

    // Create vim.api_keys namespace
    let api_keys = crate::lua::ai_api::setup_api_keys_api(lua, bridge.clone())?;
    vim.set("api_keys", api_keys)?;

    // Create vim.ai namespace
    let ai = crate::lua::ai_api::setup_ai_api(lua, bridge.clone())?;
    vim.set("ai", ai)?;

    // Create vim.tbl_extend(behavior, ...)
    let tbl_extend = lua.create_function(
        |lua, (behavior, tables): (String, mlua::MultiValue)| {
            let result = lua.create_table()?;
            for val in tables {
                let tbl = match val {
                    Value::Table(t) => t,
                    _ => {
                        return Err(mlua::Error::external(
                            "vim.tbl_extend: all arguments after behavior must be tables",
                        ))
                    }
                };
                for pair in tbl.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    match behavior.as_str() {
                        "force" => {
                            result.set(k, v)?;
                        }
                        "keep" => {
                            if result.get::<_, Value>(k.clone())?.is_nil() {
                                result.set(k, v)?;
                            }
                        }
                        "error" => {
                            if !result.get::<_, Value>(k.clone())?.is_nil() {
                                return Err(mlua::Error::external(
                                    "vim.tbl_extend: duplicate key with behavior 'error'",
                                ));
                            }
                            result.set(k, v)?;
                        }
                        _ => {
                            return Err(mlua::Error::external(format!(
                                "vim.tbl_extend: invalid behavior '{}' (expected 'force', 'keep', or 'error')",
                                behavior
                            )))
                        }
                    }
                }
            }
            Ok(result)
        },
    )?;
    vim.set("tbl_extend", tbl_extend)?;

    // Set vim as a global
    lua.globals().set("vim", vim)?;

    Ok(())
}

/// Creates the vim.api table with editor API functions
fn create_api_table(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let api = lua.create_table()?;

    // vim.api.nvim_command(cmd)
    let bridge_clone = bridge.clone();
    let nvim_command = lua.create_function(move |_lua, cmd: String| {
        bridge_clone
            .execute_command(cmd)
            .map_err(mlua::Error::external)?;
        Ok(())
    })?;
    api.set("nvim_command", nvim_command)?;

    // vim.api.nvim_exec(src, output)
    let bridge_clone = bridge.clone();
    let nvim_exec = lua.create_function(move |_lua, (src, output): (String, bool)| {
        // Execute each line as a command
        for line in src.lines() {
            if !line.trim().is_empty() {
                bridge_clone
                    .execute_command(line.to_string())
                    .map_err(mlua::Error::external)?;
            }
        }
        if output {
            Ok(Some("".to_string()))
        } else {
            Ok(None)
        }
    })?;
    api.set("nvim_exec", nvim_exec)?;

    // vim.api.nvim_get_current_line()
    let bridge_clone = bridge.clone();
    let nvim_get_current_line = lua.create_function(move |_lua, ()| {
        if let Some((line, _)) = bridge_clone.get_cursor() {
            Ok(bridge_clone.get_line(line).unwrap_or_default())
        } else {
            Ok("".to_string())
        }
    })?;
    api.set("nvim_get_current_line", nvim_get_current_line)?;

    Ok(api)
}

/// Creates the vim.fn table with vim functions
fn create_fn_table(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let fn_table = lua.create_table()?;

    // vim.fn.line('.')
    let bridge_clone = bridge.clone();
    let line = lua.create_function(move |_lua, expr: String| {
        if expr == "." {
            // Current line (1-indexed)
            if let Some((line, _)) = bridge_clone.get_cursor() {
                Ok(line + 1)
            } else {
                Ok(1)
            }
        } else if expr == "$" {
            // Last line
            Ok(bridge_clone.get_line_count())
        } else {
            // Default to 1
            Ok(1)
        }
    })?;
    fn_table.set("line", line)?;

    // vim.fn.col('.')
    let bridge_clone = bridge.clone();
    let col = lua.create_function(move |_lua, expr: String| {
        if expr == "." {
            // Current column (1-indexed)
            if let Some((_, col)) = bridge_clone.get_cursor() {
                Ok(col + 1)
            } else {
                Ok(1)
            }
        } else {
            Ok(1)
        }
    })?;
    fn_table.set("col", col)?;

    Ok(fn_table)
}

/// Creates the vim.cmd function for executing ex commands
fn create_cmd_function(lua: &Lua, bridge: EditorBridge) -> Result<mlua::Function<'_>> {
    let cmd = lua.create_function(move |_lua, command: String| {
        bridge
            .execute_command(command)
            .map_err(mlua::Error::external)?;
        Ok(())
    })?;
    Ok(cmd)
}

fn keymap_command_for_mode(mode: &str, noremap: bool) -> Option<&'static str> {
    match (mode, noremap) {
        ("n", true) => Some("nnoremap"),
        ("n", false) => Some("nmap"),
        ("i", true) => Some("inoremap"),
        ("i", false) => Some("imap"),
        ("v", true) => Some("vnoremap"),
        ("v", false) => Some("vmap"),
        ("x", true) => Some("xnoremap"),
        ("x", false) => Some("xmap"),
        ("c", true) => Some("cnoremap"),
        ("c", false) => Some("cmap"),
        ("", true) => Some("noremap"),
        ("", false) => Some("map"),
        _ => None,
    }
}

/// Creates the vim.keymap table with keymap helper functions
fn create_keymap_table(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let keymap = lua.create_table()?;

    // vim.keymap.set(mode, lhs, rhs, opts?)
    let bridge_clone = bridge.clone();
    let set = lua.create_function(
        move |_lua, (mode, lhs, rhs, opts): (Value, String, String, Option<Table>)| {
            let mut noremap = true; // vim.keymap.set defaults to non-recursive mappings
            if let Some(opts) = opts {
                if let Ok(remap) = opts.get::<_, bool>("remap") {
                    noremap = !remap;
                }
                if let Ok(explicit_noremap) = opts.get::<_, bool>("noremap") {
                    noremap = explicit_noremap;
                }
            }

            let mut modes = Vec::new();
            match mode {
                Value::String(mode_str) => modes.push(
                    mode_str
                        .to_str()
                        .map_err(mlua::Error::external)?
                        .to_string(),
                ),
                Value::Table(mode_table) => {
                    for value in mode_table.sequence_values::<String>() {
                        modes.push(value.map_err(mlua::Error::external)?);
                    }
                    if modes.is_empty() {
                        return Err(mlua::Error::external(
                            "vim.keymap.set mode list cannot be empty",
                        ));
                    }
                }
                _ => {
                    return Err(mlua::Error::external(
                        "vim.keymap.set mode must be a string or list of strings",
                    ))
                }
            }

            for mode in modes {
                let cmd = keymap_command_for_mode(mode.as_str(), noremap).ok_or_else(|| {
                    mlua::Error::external(format!("vim.keymap.set unsupported mode '{}'", mode))
                })?;
                bridge_clone
                    .execute_command(format!("{} {} {}", cmd, lhs, rhs))
                    .map_err(mlua::Error::external)?;
            }

            Ok(())
        },
    )?;
    keymap.set("set", set)?;

    Ok(keymap)
}

/// Creates the vim.g table with metatable for global variables
fn create_g_table(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let g = lua.create_table()?;
    let metatable = lua.create_table()?;

    // __newindex: called when setting vim.g.name = value
    let bridge_clone = bridge.clone();
    let newindex = lua.create_function(move |_lua, (_, key, value): (Value, String, Value)| {
        let global_value = match value {
            Value::String(s) => GlobalValue::String(s.to_str().unwrap_or("").to_string()),
            Value::Integer(n) => GlobalValue::Integer(n),
            Value::Number(n) => GlobalValue::Number(n),
            Value::Boolean(b) => GlobalValue::Boolean(b),
            Value::Nil => GlobalValue::Nil,
            _ => {
                return Err(mlua::Error::external(format!(
                    "vim.g.{}: unsupported value type",
                    key
                )));
            }
        };
        // Special handling for mapleader: route to :set command
        if key == "mapleader" {
            if let GlobalValue::String(ref s) = global_value {
                if s.len() == 1 || s == " " {
                    let ch = if s == " " { " " } else { s.as_str() };
                    bridge_clone
                        .execute_command(format!("set mapleader={}", ch))
                        .map_err(|e| mlua::Error::external(e.to_string()))?;
                } else {
                    return Err(mlua::Error::external(
                        "vim.g.mapleader must be a single character (e.g., ' ', ',')",
                    ));
                }
            } else {
                return Err(mlua::Error::external("vim.g.mapleader must be a string"));
            }
        }
        bridge_clone.set_global(key, global_value);
        Ok(())
    })?;
    metatable.set("__newindex", newindex)?;

    // __index: called when getting vim.g.name
    let bridge_clone = bridge.clone();
    let index = lua.create_function(move |lua, (_, key): (Value, String)| {
        match bridge_clone.get_global(&key) {
            Some(GlobalValue::String(s)) => Ok(Value::String(lua.create_string(&s)?)),
            Some(GlobalValue::Integer(n)) => Ok(Value::Integer(n)),
            Some(GlobalValue::Number(n)) => Ok(Value::Number(n)),
            Some(GlobalValue::Boolean(b)) => Ok(Value::Boolean(b)),
            Some(GlobalValue::Nil) | None => Ok(Value::Nil),
        }
    })?;
    metatable.set("__index", index)?;

    g.set_metatable(Some(metatable));
    Ok(g)
}

/// Creates the vim.opt table with metatable for setting options
fn create_opt_table(lua: &Lua, bridge: EditorBridge) -> Result<Table<'_>> {
    let opt = lua.create_table()?;
    let metatable = lua.create_table()?;

    // __newindex: called when setting opt.name = value
    let bridge_clone = bridge.clone();
    let newindex = lua.create_function(
        move |_lua, (_, key, value): (mlua::Value, String, mlua::Value)| {
            let cmd: String = match key.as_str() {
                "number" | "nu" => match value {
                    mlua::Value::Boolean(true) => "set number".to_string(),
                    mlua::Value::Boolean(false) => "set nonumber".to_string(),
                    _ => return Err(mlua::Error::external("number must be boolean")),
                },
                "relativenumber" | "rnu" => match value {
                    mlua::Value::Boolean(true) => "set relativenumber".to_string(),
                    mlua::Value::Boolean(false) => "set norelativenumber".to_string(),
                    _ => return Err(mlua::Error::external("relativenumber must be boolean")),
                },
                "expandtab" | "et" => match value {
                    mlua::Value::Boolean(true) => "set expandtab".to_string(),
                    mlua::Value::Boolean(false) => "set noexpandtab".to_string(),
                    _ => return Err(mlua::Error::external("expandtab must be boolean")),
                },
                "copyindent" | "ci" => match value {
                    mlua::Value::Boolean(true) => "set copyindent".to_string(),
                    mlua::Value::Boolean(false) => "set nocopyindent".to_string(),
                    _ => return Err(mlua::Error::external("copyindent must be boolean")),
                },
                "tabstop" | "ts" => match value {
                    mlua::Value::Integer(n) => format!("set tabstop={}", n),
                    mlua::Value::Number(n) => format!("set tabstop={}", n as i64),
                    _ => return Err(mlua::Error::external("tabstop must be number")),
                },
                "shiftwidth" | "sw" => match value {
                    mlua::Value::Integer(n) => format!("set shiftwidth={}", n),
                    mlua::Value::Number(n) => format!("set shiftwidth={}", n as i64),
                    _ => return Err(mlua::Error::external("shiftwidth must be number")),
                },
                "softtabstop" | "sts" => match value {
                    mlua::Value::Integer(n) => format!("set softtabstop={}", n),
                    mlua::Value::Number(n) => format!("set softtabstop={}", n as i64),
                    _ => return Err(mlua::Error::external("softtabstop must be number")),
                },
                "scroll" => match value {
                    mlua::Value::Integer(n) => format!("set scroll={}", n),
                    mlua::Value::Number(n) => format!("set scroll={}", n as i64),
                    _ => return Err(mlua::Error::external("scroll must be number")),
                },
                "textwidth" | "tw" => match value {
                    mlua::Value::Integer(n) => format!("set textwidth={}", n),
                    mlua::Value::Number(n) => format!("set textwidth={}", n as i64),
                    _ => return Err(mlua::Error::external("textwidth must be number")),
                },
                "filetreereveal" => match value {
                    mlua::Value::Boolean(true) => "set filetreereveal".to_string(),
                    mlua::Value::Boolean(false) => "set nofiletreereveal".to_string(),
                    _ => return Err(mlua::Error::external("filetreereveal must be boolean")),
                },
                "blame" => match value {
                    mlua::Value::Boolean(true) => "set blame".to_string(),
                    mlua::Value::Boolean(false) => "set noblame".to_string(),
                    _ => return Err(mlua::Error::external("blame must be boolean")),
                },
                "margincolor" => match value {
                    mlua::Value::String(s) => {
                        let s = s.to_str().map_err(mlua::Error::external)?;
                        format!("set margincolor={}", s)
                    }
                    _ => {
                        return Err(mlua::Error::external(
                            "margincolor must be a string (hex color or 'none')",
                        ))
                    }
                },
                "marginpadding" => match value {
                    mlua::Value::Integer(n) => format!("set marginpadding={}", n),
                    mlua::Value::Number(n) => format!("set marginpadding={}", n as i64),
                    _ => return Err(mlua::Error::external("marginpadding must be a number")),
                },
                _ => {
                    return Err(mlua::Error::external(format!("Unknown option: {}", key)));
                }
            };

            bridge_clone
                .execute_command(cmd)
                .map_err(mlua::Error::external)?;
            Ok(())
        },
    )?;
    metatable.set("__newindex", newindex)?;

    opt.set_metatable(Some(metatable));
    Ok(opt)
}
