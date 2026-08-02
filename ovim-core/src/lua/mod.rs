pub mod ai_api;
pub mod api;
pub mod editor_bridge;
pub mod language_api;
pub mod util;

pub use api::setup_vim_api;
pub use editor_bridge::EditorBridge;
pub use util::lua_value_to_string;

use anyhow::Result;
use mlua::{Lua, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Lua runtime context for configuration and plugins
pub struct LuaContext {
    lua: Lua,
    config_loaded: bool,
    source_context: language_api::LuaSourceContext,
}

impl LuaContext {
    /// Creates a new Lua context with standard libraries loaded
    pub fn new() -> Result<Self> {
        let lua = Lua::new();

        // Load standard libraries
        lua.load_from_std_lib(mlua::StdLib::ALL_SAFE)?;

        Ok(Self {
            lua,
            config_loaded: false,
            source_context: Arc::new(Mutex::new(None)),
        })
    }

    pub fn source_context(&self) -> language_api::LuaSourceContext {
        self.source_context.clone()
    }

    /// Gets a reference to the underlying Lua VM
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Executes Lua code string
    pub fn execute(&self, code: &str) -> Result<Value<'_>> {
        let result = self.lua.load(code).eval()?;
        Ok(result)
    }

    /// Executes Lua code and returns nothing (for side effects)
    pub fn execute_void(&self, code: &str) -> Result<()> {
        self.lua.load(code).exec()?;
        Ok(())
    }

    /// Loads and executes a Lua file
    pub fn execute_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        let code = std::fs::read_to_string(path)?;
        let source = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        *self
            .source_context
            .lock()
            .expect("Lua source context poisoned") = Some(source);
        let result = self
            .lua
            .load(&code)
            .set_name(path.to_string_lossy().as_ref())
            .exec();
        *self
            .source_context
            .lock()
            .expect("Lua source context poisoned") = None;
        result.map_err(Into::into)
    }

    /// Loads the built-in defaults (builtin.lua) that ship with the binary.
    /// Runs once before the user's init.lua. Sets up API keys, prompts,
    /// context policies, and profiles so ovim works out-of-the-box.
    pub fn load_builtin(&self) -> Result<()> {
        const BUILTIN: &str = include_str!("builtin.lua");
        self.lua.load(BUILTIN).set_name("[builtin]").exec()?;
        Ok(())
    }

    /// Loads configuration from standard locations
    pub fn load_config(&mut self) -> Result<bool> {
        if self.config_loaded {
            return Ok(true);
        }

        // Try config locations in order
        let config_paths = Self::get_config_paths();

        for path in config_paths {
            if path.exists() {
                self.execute_file(&path)?;
                self.config_loaded = true;
                return Ok(true);
            }
        }

        // No config found is not an error
        Ok(false)
    }

    /// Gets the list of potential config file paths in priority order
    fn get_config_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();

        // $OVIM_CONFIG/init.lua
        if let Ok(ovim_config) = std::env::var("OVIM_CONFIG") {
            let mut path = std::path::PathBuf::from(ovim_config);
            path.push("init.lua");
            paths.push(path);
        }

        // $XDG_CONFIG_HOME/ovim/init.lua
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            let mut path = std::path::PathBuf::from(xdg_config);
            path.push("ovim");
            path.push("init.lua");
            paths.push(path);
        }

        // ~/.config/ovim/init.lua
        if let Some(home) = std::env::var_os("HOME") {
            let mut path = std::path::PathBuf::from(&home);
            path.push(".config");
            path.push("ovim");
            path.push("init.lua");
            paths.push(path.clone());

            // ~/.ovim/init.lua
            let mut alt_path = std::path::PathBuf::from(&home);
            alt_path.push(".ovim");
            alt_path.push("init.lua");
            paths.push(alt_path);
        }

        paths
    }

    /// Reloads configuration
    pub fn reload_config(&mut self) -> Result<()> {
        self.config_loaded = false;
        self.load_config()?;
        Ok(())
    }

    /// Loads plugins from plugin directories
    pub fn load_plugins(&mut self) -> Result<()> {
        let plugin_paths = Self::get_plugin_paths();

        for plugin_dir in plugin_paths {
            if !plugin_dir.exists() {
                continue;
            }

            // Iterate through directories in plugin path
            if let Ok(entries) = std::fs::read_dir(&plugin_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_dir() {
                            // Try to load init.lua from plugin directory
                            let mut init_path = entry.path();
                            init_path.push("init.lua");

                            if init_path.exists() {
                                // Load the plugin, log errors but continue
                                if let Err(e) = self.execute_file(&init_path) {
                                    crate::log_error!(
                                        "lua",
                                        "Failed to load plugin {:?}: {}",
                                        entry.path(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Gets the list of plugin directories
    fn get_plugin_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();

        // $OVIM_CONFIG/plugins
        if let Ok(ovim_config) = std::env::var("OVIM_CONFIG") {
            let mut path = std::path::PathBuf::from(ovim_config);
            path.push("plugins");
            paths.push(path);
        }

        // $XDG_CONFIG_HOME/ovim/plugins
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            let mut path = std::path::PathBuf::from(xdg_config);
            path.push("ovim");
            path.push("plugins");
            paths.push(path);
        }

        // ~/.config/ovim/plugins
        if let Some(home) = std::env::var_os("HOME") {
            let mut path = std::path::PathBuf::from(&home);
            path.push(".config");
            path.push("ovim");
            path.push("plugins");
            paths.push(path);

            // ~/.ovim/plugins
            let mut alt_path = std::path::PathBuf::from(&home);
            alt_path.push(".ovim");
            alt_path.push("plugins");
            paths.push(alt_path);
        }

        paths
    }

    /// Sets a global variable in Lua
    pub fn set_global<'lua, V: mlua::IntoLua<'lua>>(
        &'lua self,
        name: &str,
        value: V,
    ) -> Result<()> {
        self.lua.globals().set(name, value)?;
        Ok(())
    }

    /// Gets a global variable from Lua
    pub fn get_global<'lua>(&'lua self, name: &str) -> Result<Value<'lua>> {
        let value = self.lua.globals().get(name)?;
        Ok(value)
    }
}

impl Default for LuaContext {
    fn default() -> Self {
        Self::new().expect("Failed to create Lua context")
    }
}
