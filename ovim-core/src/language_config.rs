// Language Configuration System
//
// This module provides a declarative configuration system for language support,
// replacing the hardcoded LSP initialization pattern with a data-driven approach.
//
// Key Design Principles:
// 1. Declarative over Imperative - Configuration file defines languages, not Rust code
// 2. Convention over Configuration - Smart defaults for common cases
// 3. Extensibility First - Users can add languages without recompiling
// 4. Fail Gracefully - Missing LSP shouldn't break syntax highlighting

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ============================================================================
// Core Data Structures
// ============================================================================

/// Complete language configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LanguageConfig {
    /// Language ID (e.g., "rust", "typescript")
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// File extensions (e.g., ["rs"], ["ts", "tsx"])
    #[serde(default)]
    pub extensions: Vec<String>,

    /// Filenames without extensions (e.g., ["Dockerfile", "Makefile"])
    #[serde(default)]
    pub filenames: Vec<String>,

    /// Parent-qualified filenames (e.g., ["ghostty/config"])
    /// Format: "parent_dir/filename" — matches immediate parent directory only
    #[serde(default)]
    pub path_filenames: Vec<String>,

    /// Tree-sitter syntax highlighting config (optional)
    pub syntax: Option<SyntaxConfig>,

    /// LSP server configuration (optional)
    pub lsp: Option<LspConfig>,

    /// DAP server configuration (optional)
    pub dap: Option<DapConfig>,

    /// Test runner configuration (optional). Overrides the built-in
    /// runner for this language when present.
    pub test: Option<TestConfig>,
}

/// Test runner configuration.
///
/// Commands are shell templates with placeholders:
/// - `{file}`    — test file path relative to the project root
/// - `{name}`    — the resolved test filter (nearest test), already escaped
///   for use inside single quotes
/// - `{line}`    — 1-indexed cursor line
///
/// Any omitted command falls back to the built-in runner for the language
/// (if one exists), so a config can override just `nearest_command`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TestConfig {
    /// Command for running the whole suite (`:TestSuite` / `<Space>ta`)
    pub suite_command: Option<String>,

    /// Command for running the current file's tests (`:TestFile` / `<Space>tf`)
    pub file_command: Option<String>,

    /// Command for running the nearest test (`:TestNearest` / `<Space>tn`)
    pub nearest_command: Option<String>,

    /// Project root markers (searched upward from the file). The command
    /// runs with the marker's directory as working directory. Falls back to
    /// the built-in runner's root detection when empty.
    #[serde(default)]
    pub root_markers: Vec<String>,
}

/// Syntax highlighting configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyntaxConfig {
    /// Tree-sitter grammar name (matches crate name)
    pub grammar: String,

    /// Highlight query source (embedded or file path)
    #[serde(flatten)]
    pub query: QuerySource,
}

/// Where to find the highlight query
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QuerySource {
    /// Use official query from tree-sitter crate
    Official {
        #[serde(rename = "crate")]
        crate_name: String,
        constant: String,
    },

    /// Load from file (e.g., "queries/markdown.scm")
    File { path: String },

    /// Inline query string
    Inline { content: String },
}

/// Companion LSP server configuration (e.g., Tailwind CSS alongside TypeScript)
///
/// Companion LSPs serve the same files as the primary LSP but provide
/// additional capabilities (e.g., Tailwind class completions).
/// They are defined as top-level `[[companion_lsp]]` entries rather than
/// nested inside each `[[language]]` to avoid redundancy — a companion
/// like Tailwind applies to many languages (TS, JS, HTML, CSS, Vue, etc.).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompanionLspConfig {
    /// Unique identifier (e.g., "tailwindcss")
    pub id: String,

    /// Human-readable name (e.g., "Tailwind CSS")
    pub name: String,

    /// Server command (e.g., "tailwindcss-language-server")
    pub command: String,

    /// Command-line arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Language IDs this companion applies to (e.g., ["typescript", "typescriptreact", ...])
    pub applies_to: Vec<String>,

    /// Project root markers for this companion
    #[serde(default)]
    pub root_markers: Vec<String>,

    /// Files that must exist in the project for this companion to activate
    /// If empty, the companion always activates for matching languages
    #[serde(default)]
    pub activation_markers: Vec<String>,

    /// Installation instructions
    pub install_hint: Option<String>,

    /// Auto-install configuration
    pub auto_install: Option<AutoInstallConfig>,

    /// Alternative commands to try if primary fails
    #[serde(default)]
    pub fallback_commands: Vec<String>,
}

/// Debug Adapter Protocol (DAP) server configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DapConfig {
    /// Primary server command (e.g., "hyperion-lsp")
    pub command: String,

    /// Command-line arguments (e.g., ["dap"])
    #[serde(default)]
    pub args: Vec<String>,

    /// Alternative commands to try if primary fails
    #[serde(default)]
    pub fallback_commands: Vec<String>,

    /// Project root markers (searched in order)
    #[serde(default)]
    pub root_markers: Vec<String>,

    /// Installation instructions (shown on failure)
    pub install_hint: Option<String>,
}

/// LSP server configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LspConfig {
    /// Primary server command (e.g., "rust-analyzer")
    pub command: String,

    /// Command-line arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Alternative commands to try if primary fails
    #[serde(default)]
    pub fallback_commands: Vec<String>,

    /// Project root markers (searched in order)
    #[serde(default)]
    pub root_markers: Vec<String>,

    /// Installation instructions (shown on failure)
    pub install_hint: Option<String>,

    /// Auto-install configuration (optional)
    pub auto_install: Option<AutoInstallConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutoInstallConfig {
    /// Installation method
    pub method: InstallMethod,

    /// Version constraint (e.g., ">=1.0.0")
    pub version: Option<String>,

    /// Auto-install policy for this language server.
    #[serde(default)]
    pub policy: AutoInstallPolicy,

    /// Allow automatic install attempts in headless mode.
    #[serde(default)]
    pub allow_headless: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoInstallPolicy {
    /// Never auto-install automatically.
    ManualOnly,
    /// Auto-install only when the LSP command is missing.
    #[default]
    AutoOnMissing,
    /// Auto-install when missing, and attempt one repair on known startup failures.
    AutoOnMissingOrKnownFailure,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InstallMethod {
    /// Download from GitHub release
    Github {
        repo: String,
        /// Asset name pattern with optional `{os}` and `{arch}` placeholders
        asset_pattern: String,
        install_path: String,
        /// Path to the binary within an archive (e.g. "bin/lua-language-server").
        /// For non-archive assets, this is ignored.
        #[serde(default)]
        binary_name: Option<String>,
    },

    /// Install via npm
    Npm {
        /// Deprecated single-package field (kept for backward compatibility).
        #[serde(default)]
        package: Option<String>,
        /// Packages to install in one command.
        #[serde(default)]
        packages: Vec<String>,
        /// Binary name to verify after install (defaults to first package).
        #[serde(default)]
        bin: Option<String>,
        #[serde(default)]
        global: bool,
    },

    /// Install via cargo
    Cargo {
        package: String,
        /// Binary name to verify after install (defaults to package name).
        #[serde(default)]
        bin: Option<String>,
        /// Cargo features to enable (passed as --features).
        #[serde(default)]
        features: Vec<String>,
    },

    /// Custom shell command
    Shell { command: String },
}

// ============================================================================
// Language Registry (Singleton)
// ============================================================================

/// Global language registry
///
/// Educational Note: Why OnceLock?
/// - Thread-safe singleton initialization (no unsafe code)
/// - Immutable after init → no locks needed for reads
/// - Modern replacement for lazy_static! (stable since Rust 1.70)
/// - Perfect for "load once at startup" data
static LANGUAGE_REGISTRY: OnceLock<LanguageRegistry> = OnceLock::new();

pub struct LanguageRegistry {
    /// All configured languages
    languages: Vec<LanguageConfig>,

    /// Extension → Language index lookup (O(1))
    by_extension: HashMap<String, usize>,

    /// Filename → Language index lookup (for extensionless files)
    by_filename: HashMap<String, usize>,

    /// (parent_dir, filename) → Language index lookup (for parent-qualified filenames)
    by_path_filename: HashMap<(String, String), usize>,

    /// Language ID → Language index lookup
    by_id: HashMap<String, usize>,

    /// All configured companion LSP servers
    companions: Vec<CompanionLspConfig>,

    /// Language ID → indices into `companions` vec
    companions_by_language: HashMap<String, Vec<usize>>,
}

impl InstallMethod {
    /// Returns the list of npm packages to install for this method.
    pub fn npm_packages(&self) -> Vec<String> {
        match self {
            InstallMethod::Npm {
                package, packages, ..
            } => {
                let mut out = Vec::new();
                if let Some(pkg) = package {
                    if !pkg.is_empty() {
                        out.push(pkg.clone());
                    }
                }
                for pkg in packages {
                    if !pkg.is_empty() && !out.contains(pkg) {
                        out.push(pkg.clone());
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// Returns the expected npm binary name for verification.
    pub fn npm_bin(&self) -> Option<String> {
        match self {
            InstallMethod::Npm { bin, .. } => {
                bin.clone().or_else(|| self.npm_packages().first().cloned())
            }
            _ => None,
        }
    }
}

impl LanguageRegistry {
    /// Initialize the global registry from embedded + user configs
    ///
    /// This should be called early in main() before any language detection.
    pub fn init() -> Result<(), String> {
        // Load embedded config (compiled into binary)
        let embedded = include_str!("../languages.toml");

        // Load user config (if exists)
        let user_config = Self::load_user_config();

        // Parse and merge configurations
        let (languages, companions) = Self::parse_configs(embedded, user_config)?;

        // Build lookup indices for fast detection
        let registry = Self::build_indices(languages, companions);

        // Set global singleton (fails if already initialized)
        LANGUAGE_REGISTRY
            .set(registry)
            .map_err(|_| "LanguageRegistry already initialized".to_string())?;

        Ok(())
    }

    /// Get the global registry (panics if not initialized)
    ///
    /// Use this in application code after init() has been called in main().
    pub fn get() -> &'static LanguageRegistry {
        LANGUAGE_REGISTRY
            .get()
            .expect("LanguageRegistry not initialized - call LanguageRegistry::init() first")
    }

    /// Get the global registry (returns None if not initialized)
    ///
    /// Use this when you need to handle the uninitialized case gracefully.
    pub fn try_get() -> Option<&'static LanguageRegistry> {
        LANGUAGE_REGISTRY.get()
    }

    /// Detect language from file path
    ///
    /// Returns the language config if a match is found, None otherwise.
    ///
    /// Detection strategy:
    /// 1. Try extension (e.g., "rs" from "main.rs")
    /// 2. Try parent-qualified filename (e.g., "ghostty/config")
    /// 3. Try exact filename (e.g., "Dockerfile")
    /// 4. Try lowercase filename (case-insensitive matching)
    pub fn detect<P: AsRef<Path>>(&self, path: P) -> Option<&LanguageConfig> {
        let path = path.as_ref();

        // Try extension first
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if let Some(&idx) = self.by_extension.get(&ext.to_ascii_lowercase()) {
                return Some(&self.languages[idx]);
            }
        }

        // Try parent-qualified filename (e.g., "ghostty/config")
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Some(parent) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|p| p.to_str())
            {
                let key = (parent.to_lowercase(), name.to_lowercase());
                if let Some(&idx) = self.by_path_filename.get(&key) {
                    return Some(&self.languages[idx]);
                }
            }

            // Try exact filename match
            if let Some(&idx) = self.by_filename.get(name) {
                return Some(&self.languages[idx]);
            }

            // Try lowercase match (case-insensitive)
            let lower = name.to_lowercase();
            if let Some(&idx) = self.by_filename.get(&lower) {
                return Some(&self.languages[idx]);
            }
        }

        None
    }

    /// Get language by ID
    pub fn get_by_id(&self, id: &str) -> Option<&LanguageConfig> {
        self.by_id.get(id).map(|&idx| &self.languages[idx])
    }

    /// List all languages (for debugging/introspection)
    pub fn all(&self) -> &[LanguageConfig] {
        &self.languages
    }

    /// Get companion LSP configs that apply to a given language ID
    pub fn companions_for_language(&self, language_id: &str) -> Vec<&CompanionLspConfig> {
        self.companions_by_language
            .get(language_id)
            .map(|indices| indices.iter().map(|&idx| &self.companions[idx]).collect())
            .unwrap_or_default()
    }

    /// List all companion LSP configs
    pub fn all_companions(&self) -> &[CompanionLspConfig] {
        &self.companions
    }

    /// Load user config from ~/.config/ovim/languages.toml
    fn load_user_config() -> Option<String> {
        let config_path = dirs::config_dir()?.join("ovim/languages.toml");
        std::fs::read_to_string(config_path).ok()
    }

    /// Parse embedded and user configs, merging them
    ///
    /// Merging strategy:
    /// - User config overrides embedded config (by language ID)
    /// - User can add new languages not in embedded config
    /// - Companion LSP configs are merged by ID (user overrides embedded)
    fn parse_configs(
        embedded: &str,
        user: Option<String>,
    ) -> Result<(Vec<LanguageConfig>, Vec<CompanionLspConfig>), String> {
        // Parse embedded config
        #[derive(Deserialize)]
        struct ConfigFile {
            #[serde(default)]
            language: Vec<LanguageConfig>,
            #[serde(default)]
            companion_lsp: Vec<CompanionLspConfig>,
        }

        let embedded_config: ConfigFile = toml::from_str(embedded)
            .map_err(|e| format!("Failed to parse embedded languages.toml: {}", e))?;

        let mut languages = embedded_config.language;
        let mut companions = embedded_config.companion_lsp;

        // Parse and merge user config
        if let Some(user_toml) = user {
            let user_config: ConfigFile = toml::from_str(&user_toml).map_err(|e| {
                format!(
                    "Failed to parse user languages.toml (~/.config/ovim/languages.toml): {}",
                    e
                )
            })?;

            // User config overrides embedded (by language ID)
            for user_lang in user_config.language {
                if let Some(pos) = languages.iter().position(|l| l.id == user_lang.id) {
                    // Override existing language
                    languages[pos] = user_lang;
                } else {
                    // Add new language
                    languages.push(user_lang);
                }
            }

            // User companion config overrides embedded (by companion ID)
            for user_companion in user_config.companion_lsp {
                if let Some(pos) = companions.iter().position(|c| c.id == user_companion.id) {
                    companions[pos] = user_companion;
                } else {
                    companions.push(user_companion);
                }
            }
        }

        Ok((languages, companions))
    }

    /// Build lookup indices for fast detection
    ///
    /// Educational Note: Why HashMaps?
    /// - O(1) lookup time vs O(n) iteration
    /// - Language detection happens on every file open
    /// - Small memory cost (~100 entries) for massive speed gain
    fn build_indices(languages: Vec<LanguageConfig>, companions: Vec<CompanionLspConfig>) -> Self {
        let mut by_extension = HashMap::new();
        let mut by_filename = HashMap::new();
        let mut by_path_filename = HashMap::new();
        let mut by_id = HashMap::new();

        for (idx, lang) in languages.iter().enumerate() {
            // Index extensions
            for ext in &lang.extensions {
                by_extension.insert(ext.to_ascii_lowercase(), idx);
            }

            // Index filenames (store lowercase for case-insensitive matching)
            for name in &lang.filenames {
                by_filename.insert(name.to_lowercase(), idx);
            }

            // Index path_filenames (format: "parent/filename", stored lowercase)
            for path_name in &lang.path_filenames {
                if let Some((parent, filename)) = path_name.rsplit_once('/') {
                    by_path_filename.insert((parent.to_lowercase(), filename.to_lowercase()), idx);
                }
            }

            // Index ID
            by_id.insert(lang.id.clone(), idx);
        }

        // Build companion lookup: language_id -> [companion indices]
        let mut companions_by_language: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, companion) in companions.iter().enumerate() {
            for lang_id in &companion.applies_to {
                companions_by_language
                    .entry(lang_id.clone())
                    .or_default()
                    .push(idx);
            }
        }

        Self {
            languages,
            by_extension,
            by_filename,
            by_path_filename,
            by_id,
            companions,
            companions_by_language,
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Find LSP command by checking primary + fallbacks
///
/// This implements a fallback chain for graceful degradation:
/// 1. Try primary command in PATH
/// 2. Try each fallback location (supports ~ expansion)
/// 3. Return None if not found (caller should show install hint)
///
/// Educational Note: Why Option instead of Result?
/// - Missing LSP is not an error (it's an expected user environment state)
/// - Option communicates "this might not exist, handle it gracefully"
/// - Result is for programmer mistakes (invalid state)
pub fn find_lsp_command(config: &LspConfig) -> Option<String> {
    // Try primary command in PATH
    if which::which(&config.command).is_ok() {
        return Some(config.command.clone());
    }

    // Try fallback commands
    for fallback in &config.fallback_commands {
        // Expand ~ to home directory
        let expanded = shellexpand::tilde(fallback).to_string();

        // Check if path exists
        if std::path::Path::new(&expanded).exists() {
            return Some(expanded);
        }

        // Check if in PATH
        if which::which(&expanded).is_ok() {
            return Some(expanded);
        }
    }

    // Try well-known install locations that may not be in PATH.
    // This makes ovim work out of the box even when the user's shell
    // config doesn't include these directories in PATH.
    if let Some(path) = find_in_well_known_locations(&config.command) {
        return Some(path);
    }

    None
}

/// Check well-known package manager install directories for a binary.
///
/// Users often have tools installed via cargo, npm, pip, etc. but the
/// install directories aren't always in PATH (e.g., Arch Linux with
/// pacman-installed cargo puts `cargo` in /usr/bin but `cargo install`
/// targets go to ~/.cargo/bin which isn't in PATH by default).
///
/// This list must stay in sync with the directories the auto-installers
/// trust when *verifying* a successful install (see `auto_install.rs`).
/// If `verify_*` accepts a location that this finder doesn't search, a
/// post-install re-detection will fail even though the binary exists,
/// which sends the consent flow back into an infinite install prompt.
pub fn find_in_well_known_locations(binary: &str) -> Option<String> {
    use std::path::PathBuf;
    let home = dirs::home_dir()?;
    // ovim-managed sandboxed installs (mason-style). Checked first among the
    // well-known locations: these binaries were installed by ovim
    // specifically for ovim, so they beat other fallback guesses. PATH and
    // project-local fallbacks still win in find_lsp_command.
    //
    // Executables are resolved inside each sandbox rather than linked into
    // a shared bin dir: pnpm's .bin entries are shell shims that resolve
    // paths relative to $0, so they only work when invoked at their real
    // location.
    if let Some(root) = managed_lsp_root() {
        for (manager, bin_subpath) in [("npm", "node_modules/.bin"), ("cargo", "bin")] {
            if let Ok(entries) = std::fs::read_dir(root.join(manager)) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join(bin_subpath).join(binary);
                    if candidate.exists() {
                        return Some(candidate.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    let candidates = [
        // cargo install location ($CARGO_HOME/bin or ~/.cargo/bin)
        std::env::var("CARGO_HOME")
            .ok()
            .map(|h| PathBuf::from(h).join("bin").join(binary)),
        Some(home.join(".cargo/bin").join(binary)),
        // npm global install locations
        Some(home.join(".npm-global/bin").join(binary)),
        Some(home.join(".nvm/current/bin").join(binary)),
        // npm/Homebrew default prefixes (matches verify_npm_installation)
        Some(PathBuf::from("/opt/homebrew/bin").join(binary)),
        Some(PathBuf::from("/usr/local/bin").join(binary)),
        // pip / pipx
        Some(home.join(".local/bin").join(binary)),
        // go install ($GOBIN, $GOPATH/bin, or ~/go/bin)
        std::env::var("GOBIN")
            .ok()
            .map(|p| PathBuf::from(p).join(binary)),
        std::env::var("GOPATH")
            .ok()
            .map(|p| PathBuf::from(p).join("bin").join(binary)),
        Some(home.join("go/bin").join(binary)),
        // dotnet tool install --global
        Some(home.join(".dotnet/tools").join(binary)),
        // ovim-managed LSP installs
        Some(
            home.join(".local/share/ovim/lsp")
                .join(binary)
                .join("bin")
                .join(binary),
        ),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    // gem install locations — version dirs need scanning
    // ~/.local/share/gem/ruby/*/bin (Arch), ~/.gem/ruby/*/bin (other distros)
    for gem_base in [home.join(".local/share/gem/ruby"), home.join(".gem/ruby")] {
        if let Ok(entries) = std::fs::read_dir(&gem_base) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join(binary);
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }
    }

    None
}

/// Root of ovim's managed LSP install area (`~/.local/share/ovim/lsp`).
///
/// Mason-style layout: each package manager gets a subdirectory holding one
/// sandbox per package; executables are resolved inside each sandbox:
///
/// ```text
/// ~/.local/share/ovim/lsp/
///   npm/astrojs-language-server/{package.json, pnpm-workspace.yaml,
///                                node_modules/.bin/astro-ls, ...}
///   cargo/taplo-cli/bin/taplo
/// ```
///
/// Nothing here touches the user's global package-manager namespace.
pub fn managed_lsp_root() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".local/share/ovim/lsp"))
}

/// Sandbox directory for one package installed via one package manager,
/// e.g. `managed_lsp_package_dir("npm", "@astrojs/language-server")`
/// → `~/.local/share/ovim/lsp/npm/astrojs-language-server`.
pub fn managed_lsp_package_dir(manager: &str, package: &str) -> Option<std::path::PathBuf> {
    let sanitized: String = package
        .trim_start_matches('@')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    managed_lsp_root().map(|root| root.join(manager).join(sanitized))
}

/// Find the DAP server command, trying primary then fallbacks.
///
/// Same strategy as `find_lsp_command`.
pub fn find_dap_command(config: &DapConfig) -> Option<String> {
    if which::which(&config.command).is_ok() {
        return Some(config.command.clone());
    }

    for fallback in &config.fallback_commands {
        let expanded = shellexpand::tilde(fallback).to_string();
        if std::path::Path::new(&expanded).exists() {
            return Some(expanded);
        }
        if which::which(&expanded).is_ok() {
            return Some(expanded);
        }
    }

    if let Some(path) = find_in_well_known_locations(&config.command) {
        return Some(path);
    }

    None
}

/// Find project root by walking up and checking markers
///
/// Walks up the directory tree from the file, checking for marker files
/// like Cargo.toml, package.json, etc. Returns the first directory that
/// contains any of the specified markers.
///
/// Educational Note: Why walk up instead of down?
/// - Project roots are above files in the directory tree
/// - Walking down would be exponentially slower (must check all subdirs)
/// - Walking up is O(depth) where depth is typically <10
pub fn find_project_root(file_path: &Path, markers: &[String]) -> PathBuf {
    let mut current = file_path.parent();

    while let Some(dir) = current {
        // Check each marker in order
        for marker in markers {
            if dir.join(marker).exists() {
                return dir.to_path_buf();
            }
        }

        // Move up one directory
        current = dir.parent();
    }

    // Fallback to file's directory
    file_path
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_path_buf()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_config() {
        let toml = r#"
            [[language]]
            id = "rust"
            name = "Rust"
            extensions = ["rs"]

            [language.syntax]
            grammar = "tree-sitter-rust"
            official = { crate = "tree_sitter_rust", constant = "HIGHLIGHTS_QUERY" }

            [language.lsp]
            command = "rust-analyzer"
            root_markers = ["Cargo.toml"]
        "#;

        #[derive(Deserialize)]
        struct ConfigFile {
            language: Vec<LanguageConfig>,
        }

        let config: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(config.language.len(), 1);
        assert_eq!(config.language[0].id, "rust");
        assert_eq!(config.language[0].extensions, vec!["rs"]);
    }

    #[test]
    fn test_registry_building() {
        let languages = vec![
            LanguageConfig {
                id: "rust".to_string(),
                name: "Rust".to_string(),
                extensions: vec!["rs".to_string()],
                filenames: vec![],
                path_filenames: vec![],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
            LanguageConfig {
                id: "markdown".to_string(),
                name: "Markdown".to_string(),
                extensions: vec!["md".to_string(), "markdown".to_string()],
                filenames: vec!["README".to_string()],
                path_filenames: vec![],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
        ];

        let registry = LanguageRegistry::build_indices(languages, vec![]);

        // Test extension lookup
        assert!(registry.by_extension.contains_key("rs"));
        assert!(registry.by_extension.contains_key("md"));
        assert!(registry.by_extension.contains_key("markdown"));

        // Test filename lookup (stored as lowercase)
        assert!(registry.by_filename.contains_key("readme"));

        // Test ID lookup
        assert!(registry.by_id.contains_key("rust"));
        assert!(registry.by_id.contains_key("markdown"));
    }

    #[test]
    fn test_language_detection() {
        let languages = vec![
            LanguageConfig {
                id: "rust".to_string(),
                name: "Rust".to_string(),
                extensions: vec!["rs".to_string()],
                filenames: vec![],
                path_filenames: vec![],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
            LanguageConfig {
                id: "markdown".to_string(),
                name: "Markdown".to_string(),
                extensions: vec!["md".to_string()],
                filenames: vec!["readme".to_string()],
                path_filenames: vec![],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
        ];

        let registry = LanguageRegistry::build_indices(languages, vec![]);

        // Test extension detection
        assert_eq!(registry.detect("src/main.rs").unwrap().id, "rust");
        assert_eq!(registry.detect("docs/guide.md").unwrap().id, "markdown");

        // Test filename detection (case-insensitive)
        assert_eq!(registry.detect("README").unwrap().id, "markdown");
        assert_eq!(registry.detect("readme").unwrap().id, "markdown");

        // Test no match
        assert!(registry.detect("unknown.xyz").is_none());
    }

    #[test]
    fn test_find_project_root() {
        use std::fs;
        use tempfile::tempdir;

        // Create temp directory structure:
        // temp/
        //   Cargo.toml
        //   src/
        //     lib.rs
        //     subdir/
        //       mod.rs

        let temp = tempdir().unwrap();
        let root = temp.path();

        fs::write(root.join("Cargo.toml"), "").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::create_dir(root.join("src/subdir")).unwrap();
        let file = root.join("src/subdir/mod.rs");
        fs::write(&file, "").unwrap();

        // Find Cargo.toml from nested file
        let markers = vec!["Cargo.toml".to_string()];
        let found_root = find_project_root(&file, &markers);

        assert_eq!(found_root, root);
    }

    #[test]
    fn test_config_merging() {
        let embedded = r#"
            [[language]]
            id = "rust"
            name = "Rust"
            extensions = ["rs"]
        "#;

        let user = Some(
            r#"
            [[language]]
            id = "rust"
            name = "Rust (Custom)"
            extensions = ["rs"]

            [[language]]
            id = "custom"
            name = "Custom Language"
            extensions = ["custom"]
        "#
            .to_string(),
        );

        let (languages, _companions) = LanguageRegistry::parse_configs(embedded, user).unwrap();

        // Should have 2 languages (rust overridden, custom added)
        assert_eq!(languages.len(), 2);

        // Rust should be overridden
        let rust = languages.iter().find(|l| l.id == "rust").unwrap();
        assert_eq!(rust.name, "Rust (Custom)");

        // Custom should be added
        let custom = languages.iter().find(|l| l.id == "custom").unwrap();
        assert_eq!(custom.name, "Custom Language");
    }

    #[test]
    fn embedded_config_includes_wgsl_syntax() {
        let (languages, _companions) =
            LanguageRegistry::parse_configs(include_str!("../languages.toml"), None).unwrap();
        let wgsl = languages
            .iter()
            .find(|language| language.id == "wgsl")
            .expect("embedded WGSL language config");

        assert_eq!(wgsl.extensions, ["wgsl"]);
        assert_eq!(
            wgsl.syntax.as_ref().map(|syntax| syntax.grammar.as_str()),
            Some("tree-sitter-wgsl-bevy")
        );
        assert!(wgsl.lsp.is_none());
    }

    #[test]
    fn embedded_config_detects_yaml_and_common_file_variants() {
        let (languages, companions) =
            LanguageRegistry::parse_configs(include_str!("../languages.toml"), None).unwrap();
        let registry = LanguageRegistry::build_indices(languages, companions);

        for (path, expected_id) in [
            ("config.yaml", "yaml"),
            ("config.yml", "yaml"),
            ("config.YAML", "yaml"),
            ("legacy.es", "javascript"),
            ("types.pyi", "python"),
            ("page.xhtml", "html"),
            ("script.ksh", "bash"),
            ("package.zon", "zig"),
            ("Dockerfile", "dockerfile"),
            ("Containerfile", "dockerfile"),
            ("highlights.scm", "tree-sitter-query"),
        ] {
            assert_eq!(
                registry.detect(path).map(|language| language.id.as_str()),
                Some(expected_id),
                "wrong language for {path}"
            );
        }
    }

    #[test]
    fn embedded_config_includes_astro_lsp_and_tailwind_companion() {
        let (languages, companions) =
            LanguageRegistry::parse_configs(include_str!("../languages.toml"), None).unwrap();
        let astro = languages
            .iter()
            .find(|language| language.id == "astro")
            .expect("embedded Astro language config");

        assert_eq!(astro.extensions, ["astro"]);
        assert_eq!(
            astro.syntax.as_ref().map(|syntax| syntax.grammar.as_str()),
            Some("tree-sitter-astro-next")
        );
        let lsp = astro.lsp.as_ref().expect("Astro LSP config");
        assert_eq!(lsp.command, "astro-ls");
        assert_eq!(lsp.args, ["--stdio"]);
        assert!(matches!(
            lsp.auto_install.as_ref().map(|config| &config.method),
            Some(InstallMethod::Npm { bin: Some(bin), .. }) if bin == "astro-ls"
        ));

        let tailwind = companions
            .iter()
            .find(|companion| companion.id == "tailwindcss")
            .expect("embedded Tailwind companion config");
        assert!(tailwind.applies_to.iter().any(|id| id == "astro"));
    }

    #[test]
    fn test_path_filename_detection() {
        let languages = vec![LanguageConfig {
            id: "ghostty".to_string(),
            name: "Ghostty".to_string(),
            extensions: vec![],
            filenames: vec![],
            path_filenames: vec!["ghostty/config".to_string()],
            syntax: None,
            lsp: None,
            dap: None,
            test: None,
        }];

        let registry = LanguageRegistry::build_indices(languages, vec![]);

        // Matches when parent dir is "ghostty" and filename is "config"
        assert_eq!(
            registry
                .detect("/home/user/.config/ghostty/config")
                .unwrap()
                .id,
            "ghostty"
        );

        // Case-insensitive match
        assert_eq!(
            registry
                .detect("/home/user/.config/Ghostty/Config")
                .unwrap()
                .id,
            "ghostty"
        );

        // Does not match "config" in a different parent directory
        assert!(registry.detect("/home/user/.config/kitty/config").is_none());

        // Does not match bare "config" with no parent
        assert!(registry.detect("config").is_none());
    }

    #[test]
    fn test_path_filename_priority_over_filename() {
        let languages = vec![
            LanguageConfig {
                id: "generic-config".to_string(),
                name: "Generic Config".to_string(),
                extensions: vec![],
                filenames: vec!["config".to_string()],
                path_filenames: vec![],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
            LanguageConfig {
                id: "ghostty".to_string(),
                name: "Ghostty".to_string(),
                extensions: vec![],
                filenames: vec![],
                path_filenames: vec!["ghostty/config".to_string()],
                syntax: None,
                lsp: None,
                dap: None,
                test: None,
            },
        ];

        let registry = LanguageRegistry::build_indices(languages, vec![]);

        // Path-qualified match wins over generic filename match
        assert_eq!(
            registry
                .detect("/home/user/.config/ghostty/config")
                .unwrap()
                .id,
            "ghostty"
        );

        // Generic filename match still works for other parents
        assert_eq!(
            registry
                .detect("/home/user/.config/other/config")
                .unwrap()
                .id,
            "generic-config"
        );
    }
}
