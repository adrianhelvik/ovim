use crate::language_config::{LanguageConfig, LanguageRegistry as ConfigRegistry, LspConfig};
use crate::syntax::LanguageRegistry as SyntaxRegistry;
use libloading::Library;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOwner {
    BuiltIn,
    UserConfig { source: PathBuf },
    Plugin { name: String, root: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_catalog() -> LanguageCatalog {
        LanguageCatalog {
            entries: RwLock::new(Entries::default()),
            native_libraries: Mutex::new(Vec::new()),
            frozen: AtomicBool::new(false),
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
                Path::new("/tmp"),
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

    #[test]
    fn duplicate_id_is_rejected_without_replacing_the_first_entry() {
        let catalog = empty_catalog();
        let registration = || DynamicLanguageSpec {
            id: "nula".into(),
            name: "Nula".into(),
            extensions: vec!["nula".into()],
            parser: None,
            lsp: Some(DynamicLspSpec {
                command: vec!["nula".into()],
                language_id: "nula".into(),
                root_markers: vec![".git".into()],
            }),
        };
        catalog
            .register_dynamic(
                registration(),
                RegistrationOwner::BuiltIn,
                Path::new("/tmp"),
            )
            .unwrap();
        let error = catalog
            .register_dynamic(
                registration(),
                RegistrationOwner::BuiltIn,
                Path::new("/tmp"),
            )
            .unwrap_err();
        assert!(error.contains("already registered"));
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
    frozen: AtomicBool,
}

impl LanguageCatalog {
    pub fn built_in() -> Arc<Self> {
        let catalog = Arc::new(Self {
            entries: RwLock::new(Entries::default()),
            native_libraries: Mutex::new(Vec::new()),
            frozen: AtomicBool::new(false),
        });

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

    fn insert(&self, definition: Arc<LanguageDefinition>) {
        let mut entries = self.entries.write().expect("language catalog poisoned");
        let index = entries.languages.len();
        entries.by_id.insert(definition.config.id.clone(), index);
        for extension in &definition.config.extensions {
            entries.by_extension.insert(extension.clone(), index);
        }
        for filename in &definition.config.filenames {
            entries
                .by_filename
                .insert(filename.to_ascii_lowercase(), index);
        }
        for path_filename in &definition.config.path_filenames {
            if let Some((parent, filename)) = path_filename.rsplit_once('/') {
                entries.by_path_filename.insert(
                    (parent.to_ascii_lowercase(), filename.to_ascii_lowercase()),
                    index,
                );
            }
        }
        entries.languages.push(definition);
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

    pub fn register_dynamic(
        &self,
        mut spec: DynamicLanguageSpec,
        owner: RegistrationOwner,
        source_dir: &Path,
    ) -> Result<(), String> {
        if self.frozen.load(Ordering::Acquire) {
            return Err("language registration is closed after startup".into());
        }
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
        if self.get_by_id(&spec.id).is_some() {
            return Err(format!("language id '{}' is already registered", spec.id));
        }

        // Validate every fallible syntax operation before publishing detection or LSP state.
        let (syntax, library) = match spec.parser {
            Some(parser) => {
                let parser_path = resolve_library_path(source_dir, &parser.path)?;
                let query_path = resolve_asset_path(source_dir, &parser.highlights)?;
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

    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
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

fn resolve_asset_path(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.canonicalize()
        .map_err(|error| format!("asset '{}' was not found: {error}", path.display()))
}

fn resolve_library_path(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let mut candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    if !candidate.exists() && candidate.extension().is_none() {
        candidate.set_extension(std::env::consts::DLL_EXTENSION);
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("parser '{}' was not found: {error}", candidate.display()))
}
