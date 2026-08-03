use crate::language_config::{LanguageConfig, LanguageRegistry as ConfigRegistry, LspConfig};
use crate::syntax::LanguageRegistry as SyntaxRegistry;
use libloading::Library;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOwner {
    BuiltIn,
    UserConfig { source: PathBuf },
    Plugin { name: String, root: PathBuf },
}

impl std::fmt::Display for RegistrationOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuiltIn => write!(f, "the built-in language set"),
            Self::UserConfig { source } => write!(f, "config file '{}'", source.display()),
            Self::Plugin { name, .. } => write!(f, "plugin '{name}'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_catalog() -> LanguageCatalog {
        LanguageCatalog {
            entries: RwLock::new(Entries::default()),
            native_libraries: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn lsp_only_registration_is_detectable_and_preserves_argv() {
        let catalog = empty_catalog();
        catalog
            .register_dynamic(
                DynamicLanguageSpec {
                    id: "Nula".into(),
                    name: "Nula".into(),
                    extensions: vec!["NULA".into()],
                    parser: None,
                    lsp: Some(DynamicLspSpec {
                        command: vec!["nula".into(), "lsp".into()],
                        language_id: "nula-document".into(),
                        root_markers: vec!["nula.toml".into(), ".git".into()],
                    }),
                },
                RegistrationOwner::UserConfig {
                    source: PathBuf::from("/tmp/init.lua"),
                },
                &[PathBuf::from("/tmp")],
            )
            .unwrap();

        let language = catalog.detect("example.nula").unwrap();
        assert_eq!(language.id(), "nula");
        assert_eq!(language.lsp_language_id, "nula-document");
        let lsp = language.lsp().unwrap();
        assert_eq!(lsp.command, "nula");
        assert_eq!(lsp.args, ["lsp"]);
        assert_eq!(lsp.root_markers, ["nula.toml", ".git"]);
    }

    fn nula_spec(name: &str, extension: &str) -> DynamicLanguageSpec {
        DynamicLanguageSpec {
            id: "nula".into(),
            name: name.into(),
            extensions: vec![extension.into()],
            parser: None,
            lsp: Some(DynamicLspSpec {
                command: vec!["nula".into()],
                language_id: "nula".into(),
                root_markers: vec![".git".into()],
            }),
        }
    }

    #[test]
    fn same_owner_re_registration_replaces_the_entry() {
        let catalog = empty_catalog();
        let owner = RegistrationOwner::UserConfig {
            source: PathBuf::from("/tmp/init.lua"),
        };
        catalog
            .register_dynamic(
                nula_spec("Nula", "nula"),
                owner.clone(),
                &[PathBuf::from("/tmp")],
            )
            .unwrap();
        catalog
            .register_dynamic(nula_spec("Nula v2", "nu"), owner, &[PathBuf::from("/tmp")])
            .unwrap();

        assert_eq!(catalog.detect("test.nu").unwrap().config.name, "Nula v2");
        assert!(
            catalog.detect("test.nula").is_none(),
            "stale extension mapping must be dropped on replace"
        );
    }

    #[test]
    fn info_string_resolution_tries_id_then_alias_then_extension() {
        let catalog = LanguageCatalog::built_in();
        // Exact id
        assert_eq!(
            catalog.detect_from_info_string("rust").unwrap().id(),
            "rust"
        );
        // Built-in alias table
        assert_eq!(
            catalog.detect_from_info_string("js").unwrap().id(),
            "javascript"
        );
        // Case-insensitive
        assert_eq!(
            catalog.detect_from_info_string("Rust").unwrap().id(),
            "rust"
        );
        assert!(catalog
            .detect_from_info_string("no-such-language")
            .is_none());
        assert!(catalog.detect_from_info_string("").is_none());

        // Extension fallback for a language the alias table doesn't know.
        catalog.insert_for_tests(Arc::new(LanguageDefinition {
            config: crate::language_config::LanguageConfig {
                id: "exttest".into(),
                name: "Exttest".into(),
                extensions: vec!["zzz".into()],
                filenames: Vec::new(),
                path_filenames: Vec::new(),
                syntax: None,
                lsp: None,
                dap: None,
            },
            lsp_language_id: "exttest".into(),
            syntax: Some(SyntaxDefinition {
                language: crate::syntax::LanguageRegistry::get_tree_sitter_language(
                    crate::syntax::Language::Rust,
                ),
                highlights: Arc::from(crate::syntax::LanguageRegistry::get_highlight_query(
                    crate::syntax::Language::Rust,
                )),
            }),
            owner: RegistrationOwner::UserConfig {
                source: PathBuf::from("/tmp/init.lua"),
            },
            source: PathBuf::from("/tmp"),
        }));
        assert_eq!(
            catalog.detect_from_info_string("zzz").unwrap().id(),
            "exttest"
        );
    }

    #[test]
    fn info_string_resolution_skips_languages_without_syntax() {
        let catalog = empty_catalog();
        catalog
            .register_dynamic(
                nula_spec("Nula", "nula"),
                RegistrationOwner::UserConfig {
                    source: PathBuf::from("/tmp/init.lua"),
                },
                &[PathBuf::from("/tmp")],
            )
            .unwrap();
        // Registered LSP-only: detectable for buffers, but a fence can't be
        // highlighted with it.
        assert!(catalog.detect("main.nula").is_some());
        assert!(catalog.detect_from_info_string("nula").is_none());
    }

    #[test]
    fn cross_owner_duplicate_id_is_rejected_without_replacing_the_first_entry() {
        let catalog = empty_catalog();
        catalog
            .register_dynamic(
                nula_spec("Nula", "nula"),
                RegistrationOwner::UserConfig {
                    source: PathBuf::from("/tmp/init.lua"),
                },
                &[PathBuf::from("/tmp")],
            )
            .unwrap();
        let error = catalog
            .register_dynamic(
                nula_spec("Impostor", "nula"),
                RegistrationOwner::Plugin {
                    name: "impostor".into(),
                    root: PathBuf::from("/tmp/plugins/impostor"),
                },
                &[PathBuf::from("/tmp/plugins/impostor")],
            )
            .unwrap_err();
        assert!(error.contains("already registered by config file"));
        assert_eq!(catalog.detect("test.nula").unwrap().config.name, "Nula");
    }
}

#[derive(Clone)]
pub struct SyntaxDefinition {
    pub language: tree_sitter::Language,
    pub highlights: Arc<str>,
}

#[derive(Clone)]
pub struct LanguageDefinition {
    pub config: LanguageConfig,
    pub lsp_language_id: String,
    pub syntax: Option<SyntaxDefinition>,
    pub owner: RegistrationOwner,
    pub source: PathBuf,
}

impl LanguageDefinition {
    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn lsp(&self) -> Option<&LspConfig> {
        self.config.lsp.as_ref()
    }
}

#[derive(Default)]
struct Entries {
    languages: Vec<Arc<LanguageDefinition>>,
    by_id: HashMap<String, usize>,
    by_extension: HashMap<String, usize>,
    by_filename: HashMap<String, usize>,
    by_path_filename: HashMap<(String, String), usize>,
}

impl Entries {
    fn rebuild_maps(&mut self) {
        self.by_id.clear();
        self.by_extension.clear();
        self.by_filename.clear();
        self.by_path_filename.clear();
        for (index, definition) in self.languages.iter().enumerate() {
            self.by_id.insert(definition.config.id.clone(), index);
            for extension in &definition.config.extensions {
                self.by_extension
                    .insert(extension.to_ascii_lowercase(), index);
            }
            for filename in &definition.config.filenames {
                self.by_filename
                    .insert(filename.to_ascii_lowercase(), index);
            }
            for path_filename in &definition.config.path_filenames {
                if let Some((parent, filename)) = path_filename.rsplit_once('/') {
                    self.by_path_filename.insert(
                        (parent.to_ascii_lowercase(), filename.to_ascii_lowercase()),
                        index,
                    );
                }
            }
        }
    }
}

pub struct DynamicLanguageSpec {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub parser: Option<DynamicParserSpec>,
    pub lsp: Option<DynamicLspSpec>,
}

pub struct DynamicParserSpec {
    pub path: PathBuf,
    pub symbol: String,
    pub highlights: PathBuf,
}

pub struct DynamicLspSpec {
    pub command: Vec<String>,
    pub language_id: String,
    pub root_markers: Vec<String>,
}

/// Application-owned catalog shared by the editor, its buffers, and Lua.
/// Native parser libraries are retained for the lifetime of the catalog.
pub struct LanguageCatalog {
    entries: RwLock<Entries>,
    native_libraries: Mutex<Vec<Library>>,
}

/// The catalog installed by [`LanguageCatalog::install_as_process_catalog`].
static PROCESS_CATALOG: OnceLock<Arc<LanguageCatalog>> = OnceLock::new();

impl LanguageCatalog {
    /// Immutable built-in catalog shared as the default for buffers that are
    /// created outside an `Editor` (which injects its own catalog with any
    /// user-registered languages).
    pub fn shared_built_in() -> Arc<Self> {
        static SHARED: OnceLock<Arc<LanguageCatalog>> = OnceLock::new();
        SHARED.get_or_init(Self::built_in).clone()
    }

    pub fn built_in() -> Arc<Self> {
        let catalog = Arc::new(Self {
            entries: RwLock::new(Entries::default()),
            native_libraries: Mutex::new(Vec::new()),
        });

        // The config registry is normally initialized early in main();
        // initialize it here for standalone consumers (tests, tools). Losing
        // an init race is fine — try_get sees the winner's registry.
        if ConfigRegistry::try_get().is_none() {
            let _ = ConfigRegistry::init();
        }
        let configs = ConfigRegistry::try_get()
            .map(|registry| registry.all().to_vec())
            .unwrap_or_default();
        for config in configs {
            let syntax = SyntaxRegistry::from_id(&config.id).map(|language| SyntaxDefinition {
                language: SyntaxRegistry::get_tree_sitter_language(language),
                highlights: Arc::from(SyntaxRegistry::get_highlight_query(language)),
            });
            let definition = LanguageDefinition {
                lsp_language_id: config.id.clone(),
                syntax,
                owner: RegistrationOwner::BuiltIn,
                source: PathBuf::from("[built-in]"),
                config,
            };
            catalog.insert(Arc::new(definition));
        }
        catalog
    }

    /// Test-only seam: inserts a prebuilt definition directly, bypassing the
    /// file-system validation `register_dynamic` performs on real plugins.
    #[cfg(test)]
    pub(crate) fn insert_for_tests(&self, definition: Arc<LanguageDefinition>) {
        self.insert(definition);
    }

    /// Inserts a definition, replacing any existing entry with the same id.
    fn insert(&self, definition: Arc<LanguageDefinition>) {
        let mut entries = self.entries.write().expect("language catalog poisoned");
        match entries.by_id.get(definition.config.id.as_str()).copied() {
            Some(index) => entries.languages[index] = definition,
            None => entries.languages.push(definition),
        }
        entries.rebuild_maps();
    }

    pub fn detect<P: AsRef<Path>>(&self, path: P) -> Option<Arc<LanguageDefinition>> {
        let path = path.as_ref();
        let entries = self.entries.read().ok()?;
        if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
            if let Some(index) = entries.by_extension.get(&extension.to_ascii_lowercase()) {
                return entries.languages.get(*index).cloned();
            }
        }
        let filename = path.file_name()?.to_str()?.to_ascii_lowercase();
        if let Some(parent) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
        {
            if let Some(index) = entries
                .by_path_filename
                .get(&(parent.to_ascii_lowercase(), filename.clone()))
            {
                return entries.languages.get(*index).cloned();
            }
        }
        let index = *entries.by_filename.get(&filename)?;
        entries.languages.get(index).cloned()
    }

    pub fn get_by_id(&self, id: &str) -> Option<Arc<LanguageDefinition>> {
        let entries = self.entries.read().ok()?;
        let index = *entries.by_id.get(id)?;
        entries.languages.get(index).cloned()
    }

    pub fn get_by_extension(&self, extension: &str) -> Option<Arc<LanguageDefinition>> {
        let entries = self.entries.read().ok()?;
        let index = *entries.by_extension.get(&extension.to_ascii_lowercase())?;
        entries.languages.get(index).cloned()
    }

    /// Resolves a markdown fence info-string token (` ```rust `, ` ```js `,
    /// ` ```nula `) to a language that can highlight the block's contents.
    ///
    /// Resolution order: exact language id, then built-in aliases ("js",
    /// "py", …), then file extension. Only languages with syntax support are
    /// returned — an LSP-only registration can't highlight anything.
    pub fn detect_from_info_string(&self, token: &str) -> Option<Arc<LanguageDefinition>> {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            return None;
        }
        self.get_by_id(&token)
            .or_else(|| {
                let language = SyntaxRegistry::from_info_string(&token)?;
                self.get_by_id(&format!("{language:?}").to_ascii_lowercase())
            })
            .or_else(|| self.get_by_extension(&token))
            .filter(|definition| definition.syntax.is_some())
    }

    /// Installs this catalog as the process-wide catalog. Subsystems without
    /// a path to the editor (chat markdown, hover previews) resolve languages
    /// through [`Self::process`]. First install wins; later calls are no-ops,
    /// which keeps concurrently constructed editors (tests) harmless.
    pub fn install_as_process_catalog(self: &Arc<Self>) {
        let _ = PROCESS_CATALOG.set(self.clone());
    }

    /// The process-wide catalog: the installed editor catalog, or the shared
    /// built-in catalog when nothing was installed.
    pub fn process() -> Arc<Self> {
        PROCESS_CATALOG
            .get()
            .cloned()
            .unwrap_or_else(Self::shared_built_in)
    }

    /// Registers a language. Relative parser/query paths are resolved against
    /// `source_dirs` in order (the declaring file's literal directory first,
    /// then its symlink-resolved directory).
    ///
    /// Re-registering an id is allowed when the new owner matches the existing
    /// entry's owner — the entry is replaced. This keeps config reloads and
    /// `:source` idempotent. Cross-owner id collisions are rejected so a
    /// plugin cannot hijack a language the user (or another plugin) declared.
    pub fn register_dynamic(
        &self,
        mut spec: DynamicLanguageSpec,
        owner: RegistrationOwner,
        source_dirs: &[PathBuf],
    ) -> Result<(), String> {
        let Some(source_dir) = source_dirs.first() else {
            return Err("registration source directory is unknown".into());
        };
        spec.id = normalize_identifier("id", &spec.id)?;
        if spec.name.trim().is_empty() {
            return Err("name must not be empty".into());
        }
        if spec.extensions.is_empty() {
            return Err("files.extensions must not be empty".into());
        }
        spec.extensions = spec
            .extensions
            .iter()
            .map(|extension| normalize_identifier("files.extensions", extension))
            .collect::<Result<_, _>>()?;
        if spec.parser.is_none() && spec.lsp.is_none() {
            return Err("at least one of syntax or lsp must be present".into());
        }
        if let Some(existing) = self.get_by_id(&spec.id) {
            if existing.owner != owner {
                return Err(format!(
                    "language id '{}' is already registered by {}",
                    spec.id, existing.owner
                ));
            }
        }

        // Validate every fallible syntax operation before publishing detection or LSP state.
        let (syntax, library) = match spec.parser {
            Some(parser) => {
                let parser_path = resolve_library_path(source_dirs, &parser.path)?;
                let query_path = resolve_asset_path(source_dirs, &parser.highlights)?;
                let highlights = std::fs::read_to_string(&query_path).map_err(|error| {
                    format!(
                        "failed to read syntax.highlights '{}': {error}",
                        query_path.display()
                    )
                })?;
                let library = unsafe { Library::new(&parser_path) }.map_err(|error| {
                    format!("failed to load parser '{}': {error}", parser_path.display())
                })?;
                type ParserFn = unsafe extern "C" fn() -> *const ();
                let function = unsafe {
                    library
                        .get::<ParserFn>(parser.symbol.as_bytes())
                        .map_err(|error| format!("parser symbol '{}': {error}", parser.symbol))?
                };
                let language_fn = unsafe { tree_sitter_language::LanguageFn::from_raw(*function) };
                let language = tree_sitter::Language::new(language_fn);
                let version = language.abi_version();
                if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
                    .contains(&version)
                {
                    return Err(format!(
                        "parser ABI {version} is incompatible; supported range is {}..={}",
                        tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION,
                        tree_sitter::LANGUAGE_VERSION
                    ));
                }
                tree_sitter::Query::new(&language, &highlights)
                    .map_err(|error| format!("invalid syntax.highlights query: {error}"))?;
                (
                    Some(SyntaxDefinition {
                        language,
                        highlights: Arc::from(highlights),
                    }),
                    Some(library),
                )
            }
            None => (None, None),
        };

        let (lsp, lsp_language_id) = match spec.lsp {
            Some(lsp) => {
                let Some((command, args)) = lsp.command.split_first() else {
                    return Err("lsp.cmd must not be empty".into());
                };
                (
                    Some(LspConfig {
                        command: command.clone(),
                        args: args.to_vec(),
                        fallback_commands: Vec::new(),
                        root_markers: lsp.root_markers,
                        install_hint: None,
                        auto_install: None,
                    }),
                    lsp.language_id,
                )
            }
            None => (None, spec.id.clone()),
        };

        let definition = Arc::new(LanguageDefinition {
            config: LanguageConfig {
                id: spec.id,
                name: spec.name,
                extensions: spec.extensions,
                filenames: Vec::new(),
                path_filenames: Vec::new(),
                syntax: None,
                lsp,
                dap: None,
            },
            lsp_language_id,
            syntax,
            owner,
            source: source_dir.to_path_buf(),
        });
        if let Some(library) = library {
            self.native_libraries
                .lock()
                .map_err(|_| "native parser library store is poisoned".to_string())?
                .push(library);
        }
        self.insert(definition);
        Ok(())
    }
}

fn normalize_identifier(field: &str, value: &str) -> Result<String, String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.is_empty()
        || !normalized.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
    {
        return Err(format!(
            "{field} must contain only lowercase ASCII letters, digits, '-' or '_'"
        ));
    }
    Ok(normalized)
}

fn candidate_paths(bases: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    if path.is_absolute() {
        vec![path.to_path_buf()]
    } else {
        let mut candidates: Vec<PathBuf> = bases.iter().map(|base| base.join(path)).collect();
        candidates.dedup();
        candidates
    }
}

fn resolve_asset_path(bases: &[PathBuf], path: &Path) -> Result<PathBuf, String> {
    let candidates = candidate_paths(bases, path);
    for candidate in &candidates {
        if let Ok(resolved) = candidate.canonicalize() {
            return Ok(resolved);
        }
    }
    Err(format!(
        "asset '{}' was not found (tried {})",
        path.display(),
        display_candidates(&candidates)
    ))
}

fn resolve_library_path(bases: &[PathBuf], path: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    for candidate in candidate_paths(bases, path) {
        if candidate.extension().is_none() {
            let mut with_extension = candidate.clone();
            with_extension.set_extension(std::env::consts::DLL_EXTENSION);
            candidates.push(with_extension);
        }
        candidates.push(candidate);
    }
    for candidate in &candidates {
        if let Ok(resolved) = candidate.canonicalize() {
            return Ok(resolved);
        }
    }
    Err(format!(
        "parser '{}' was not found (tried {})",
        path.display(),
        display_candidates(&candidates)
    ))
}

fn display_candidates(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|candidate| format!("'{}'", candidate.display()))
        .collect::<Vec<_>>()
        .join(", ")
}
