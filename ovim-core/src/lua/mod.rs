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

/// Runs the user's init.lua and plugins against `catalog` so dynamically
/// registered languages become visible outside a full `Editor` (CLI tools
/// like `lsp check` and `lsp languages`). Editor-facing side effects
/// (keymaps, options, queued commands) land in a throwaway bridge and are
/// discarded. Failures are logged and skipped: a broken config degrades to
/// whatever registered before the error.
pub fn register_user_languages(catalog: &Arc<crate::language_catalog::LanguageCatalog>) {
    let Ok(mut context) = LuaContext::new() else {
        return;
    };
    let bridge = EditorBridge::new();
    if setup_vim_api(context.lua(), bridge).is_err() {
        return;
    }
    if language_api::setup_ovim_api(context.lua(), catalog.clone(), context.source_context())
        .is_err()
    {
        return;
    }
    if let Err(error) = context.load_builtin() {
        crate::log_warn!("lua", "built-in defaults failed to load: {}", error);
    }
    if let Err(error) = context.load_config() {
        crate::log_warn!(
            "lua",
            "init.lua failed while collecting language registrations: {}",
            error
        );
    }
    for (plugin_path, error) in context.load_plugins() {
        crate::log_warn!(
            "lua",
            "plugin '{}' failed while collecting language registrations: {}",
            plugin_path.display(),
            error
        );
    }
}

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
        // Keep the literal (symlink-preserving) path: language registration
        // uses it to classify plugin ownership and to resolve relative asset
        // paths, falling back to the symlink-resolved location itself.
        let source = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        };
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

    /// Loads plugins from plugin directories. Failures are logged and
    /// returned so the editor can surface them to the user; one broken
    /// plugin never prevents the others from loading.
    pub fn load_plugins(&mut self) -> Vec<(std::path::PathBuf, anyhow::Error)> {
        let mut failures = Vec::new();
        for plugin_dir in Self::get_plugin_paths() {
            self.load_plugins_from(&plugin_dir, &mut failures);
        }
        failures
    }

    fn load_plugins_from(
        &mut self,
        plugin_dir: &Path,
        failures: &mut Vec<(std::path::PathBuf, anyhow::Error)>,
    ) {
        let Ok(entries) = std::fs::read_dir(plugin_dir) else {
            return;
        };
        for entry in entries.flatten() {
            // `entry.path().is_dir()` follows symlinks; a plugin directory is
            // commonly a symlink into a local checkout.
            if !entry.path().is_dir() {
                continue;
            }
            let init_path = entry.path().join("init.lua");
            if !init_path.exists() {
                continue;
            }
            if let Err(e) = self.execute_file(&init_path) {
                crate::log_error!("lua", "Failed to load plugin {:?}: {}", entry.path(), e);
                failures.push((entry.path(), e));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn symlinked_plugin_dirs_load_and_broken_plugins_are_reported_not_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout/editors/ovim");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join("init.lua"),
            r#"
                ovim.languages.register({
                  id = "symtest",
                  name = "Symtest",
                  files = { extensions = { "symtest" } },
                  lsp = { cmd = { "symtest-lsp" } },
                })
            "#,
        )
        .unwrap();

        let plugins = temp.path().join("plugins");
        std::fs::create_dir_all(plugins.join("broken")).unwrap();
        std::fs::write(plugins.join("broken/init.lua"), "error('boom')").unwrap();
        std::os::unix::fs::symlink(&checkout, plugins.join("symtest-plugin")).unwrap();

        let catalog = crate::language_catalog::LanguageCatalog::built_in();
        let mut context = LuaContext::new().unwrap();
        language_api::setup_ovim_api(context.lua(), catalog.clone(), context.source_context())
            .unwrap();
        let mut failures = Vec::new();
        context.load_plugins_from(&plugins, &mut failures);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].0.ends_with("broken"));
        assert!(failures[0].1.to_string().contains("boom"));

        let language = catalog.detect("main.symtest").unwrap();
        match &language.owner {
            crate::language_catalog::RegistrationOwner::Plugin { name, .. } => {
                assert_eq!(name, "symtest-plugin");
            }
            other => panic!("expected plugin owner, got {other:?}"),
        }
    }
}
