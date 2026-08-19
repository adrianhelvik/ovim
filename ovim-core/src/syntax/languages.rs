use std::path::Path;

/// Supported programming languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Java,
    Kotlin,
    Scala,
    Groovy,
    Go,
    C,
    Cpp,
    Ruby,
    Bash,
    Lua,
    Dockerfile,
    Json,
    Yaml,
    Html,
    Xml,
    Astro,
    Css,
    Toml,
    Markdown,
    Zig,
    Sql,
    CSharp,
    Terraform,
    Hcl,
    Wgsl,
    /// Tree-sitter query files (used for highlight queries, etc.)
    TreeSitterQuery,
}

/// Registry for language detection and grammar access
pub struct LanguageRegistry;

impl LanguageRegistry {
    /// Resolve a stable built-in language ID to its compatibility enum.
    pub fn from_id(id: &str) -> Option<Language> {
        Some(match id {
            "rust" => Language::Rust,
            "javascript" => Language::JavaScript,
            "typescript" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "python" => Language::Python,
            "java" => Language::Java,
            "kotlin" => Language::Kotlin,
            "scala" => Language::Scala,
            "groovy" => Language::Groovy,
            "go" => Language::Go,
            "c" => Language::C,
            "cpp" => Language::Cpp,
            "ruby" => Language::Ruby,
            "bash" => Language::Bash,
            "lua" => Language::Lua,
            "dockerfile" => Language::Dockerfile,
            "json" => Language::Json,
            "yaml" => Language::Yaml,
            "html" => Language::Html,
            "xml" => Language::Xml,
            "astro" => Language::Astro,
            "css" => Language::Css,
            "toml" => Language::Toml,
            "markdown" => Language::Markdown,
            "zig" => Language::Zig,
            "sql" => Language::Sql,
            "csharp" => Language::CSharp,
            "terraform" => Language::Terraform,
            "hcl" => Language::Hcl,
            "wgsl" => Language::Wgsl,
            "tree-sitter-query" => Language::TreeSitterQuery,
            _ => return None,
        })
    }

    /// Detects language from file path/extension
    /// Supports a comprehensive list of file extensions for each language
    pub fn detect_from_path<P: AsRef<Path>>(path: P) -> Option<Language> {
        let path = path.as_ref();

        // Try extension first
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            if let Some(lang) = Self::detect_from_extension(extension) {
                return Some(lang);
            }
        }

        // Try full filename for special cases (e.g., .bashrc, Dockerfile)
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            return Self::detect_from_filename(filename);
        }

        None
    }

    /// Detects language from file extension only
    fn detect_from_extension(extension: &str) -> Option<Language> {
        match extension.to_lowercase().as_str() {
            // Rust
            "rs" => Some(Language::Rust),

            // JavaScript
            "js" | "jsx" | "mjs" | "cjs" | "es" | "es6" | "es7" => Some(Language::JavaScript),

            // TypeScript (without JSX)
            "ts" | "mts" | "cts" => Some(Language::TypeScript),

            // TSX (TypeScript with JSX)
            "tsx" => Some(Language::Tsx),

            // Python
            "py" | "pyw" | "pyi" | "pyx" | "pxd" | "pxi" | "pyc" | "pyd" | "pyo" | "pyz"
            | "pywz" | "py3" | "pyde" | "pyt" | "snakefile" | "smk" => Some(Language::Python),

            // Java
            "java" => Some(Language::Java),

            // Kotlin
            "kt" | "kts" => Some(Language::Kotlin),

            // Scala
            "scala" | "sc" | "sbt" => Some(Language::Scala),

            // Groovy (including Gradle build scripts)
            "groovy" | "gradle" => Some(Language::Groovy),

            // Go
            "go" => Some(Language::Go),

            // C
            "c" | "h" => Some(Language::C),

            // C++
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => Some(Language::Cpp),

            // Ruby
            "rb" | "rake" | "rbw" | "gemspec" => Some(Language::Ruby),

            // Bash
            "sh" | "bash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" => Some(Language::Bash),

            // Lua
            "lua" => Some(Language::Lua),

            // JSON
            "json" | "jsonc" | "json5" => Some(Language::Json),

            // YAML
            "yaml" | "yml" => Some(Language::Yaml),

            // HTML
            "html" | "htm" | "xhtml" => Some(Language::Html),

            // XML
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" => Some(Language::Xml),

            // Astro
            "astro" => Some(Language::Astro),

            // CSS
            "css" | "scss" | "sass" | "less" => Some(Language::Css),

            // TOML
            "toml" => Some(Language::Toml),

            // Markdown
            "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdx" => Some(Language::Markdown),

            // Zig
            "zig" | "zon" => Some(Language::Zig),

            // Tree-sitter query files
            "scm" => Some(Language::TreeSitterQuery),

            // SQL
            "sql" | "mysql" | "pgsql" | "sqlite" | "ddl" | "dml" => Some(Language::Sql),

            // C#
            "cs" | "csx" => Some(Language::CSharp),

            // Terraform
            "tf" | "tfvars" => Some(Language::Terraform),

            // HCL (HashiCorp Configuration Language)
            "hcl" | "nomad" | "vault" => Some(Language::Hcl),

            // WebGPU Shading Language
            "wgsl" => Some(Language::Wgsl),

            _ => None,
        }
    }

    /// Detects language from full filename (for files without extensions)
    fn detect_from_filename(filename: &str) -> Option<Language> {
        let lower = filename.to_lowercase();

        match lower.as_str() {
            // Python special files
            "pipfile" | "pipfile.lock" | "snakefile" | "wscript" | "sconstruct"
            | ".pythonstartup" | ".pythonrc" => Some(Language::Python),

            // JavaScript/Node special files
            "jakefile" | "gulpfile.js" | "gruntfile.js" | "webpack.config.js"
            | "rollup.config.js" => Some(Language::JavaScript),

            // TypeScript special files
            ".eslintrc.ts" | ".prettierrc.ts" => Some(Language::TypeScript),

            // TSX special files (React component configs)
            ".eslintrc.tsx" | ".prettierrc.tsx" => Some(Language::Tsx),

            // Bash special files
            ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".zshrc"
            | ".zprofile" | ".zshenv" | ".zlogin" | ".zlogout" | "bashrc" | "zshrc" => {
                Some(Language::Bash)
            }

            // Dockerfile special files
            "dockerfile" | "dockerfile.prod" | "dockerfile.dev" => Some(Language::Dockerfile),

            // Ruby special files
            "rakefile" | "gemfile" | "gemfile.lock" | "guardfile" | "capfile" | "vagrantfile" => {
                Some(Language::Ruby)
            }

            // Go special files
            "go.mod" | "go.sum" => Some(Language::Go),

            // Gradle build scripts
            "build.gradle" | "settings.gradle" => Some(Language::Groovy),
            "build.gradle.kts" | "settings.gradle.kts" => Some(Language::Kotlin),

            // SBT build files (Scala)
            "build.sbt" => Some(Language::Scala),

            // JSON special files
            ".eslintrc" | ".prettierrc" | ".babelrc" | "package.json" | "tsconfig.json"
            | "composer.json" => Some(Language::Json),

            // YAML special files
            ".travis.yml" | ".gitlab-ci.yml" | "docker-compose.yml" | ".clang-format"
            | ".clang-tidy" => Some(Language::Yaml),

            // Markdown special files
            "readme" | "changelog" | "contributing" | "license" => Some(Language::Markdown),

            // TOML special files
            "cargo.toml" | "cargo.lock" | "pyproject.toml" => Some(Language::Toml),

            _ => {
                // Check for common patterns
                if lower.starts_with(".python") {
                    Some(Language::Python)
                } else if lower.ends_with(".js") {
                    Some(Language::JavaScript)
                } else if lower.ends_with(".tsx") {
                    Some(Language::Tsx)
                } else if lower.ends_with(".ts") {
                    Some(Language::TypeScript)
                } else if lower.starts_with(".bash") || lower.starts_with(".zsh") {
                    Some(Language::Bash)
                } else if lower.contains("dockerfile") {
                    // Resolved: Added tree-sitter-dockerfile dependency
                    Some(Language::Dockerfile)
                } else if lower.ends_with("makefile") {
                    Some(Language::Bash)
                } else {
                    None
                }
            }
        }
    }

    /// Gets the tree-sitter language grammar
    pub fn get_tree_sitter_language(lang: Language) -> tree_sitter::Language {
        match lang {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            // Kotlin: use Java grammar as a fallback until we add a dedicated Kotlin grammar.
            Language::Kotlin => tree_sitter_java::LANGUAGE.into(),
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),
            Language::Groovy => tree_sitter_groovy::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            // Dockerfile uses Bash syntax highlighting to avoid tree-sitter version conflicts
            // tree-sitter-dockerfile depends on tree-sitter ^0.20, incompatible with our 0.23
            Language::Dockerfile => tree_sitter_bash::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Yaml => tree_sitter_yaml::language(),
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            Language::Xml => tree_sitter_xml::LANGUAGE_XML.into(),
            Language::Astro => tree_sitter_astro_next::LANGUAGE.into(),
            Language::Css => tree_sitter_css::LANGUAGE.into(),
            // TOML: tree-sitter-toml depends on tree-sitter 0.20, incompatible with our 0.23
            // Use JSON for highlighting fallback (similar key-value structure)
            Language::Toml => tree_sitter_json::LANGUAGE.into(),
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
            Language::Zig => tree_sitter_zig::LANGUAGE.into(),
            Language::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Terraform => tree_sitter_hcl::LANGUAGE.into(),
            Language::Hcl => tree_sitter_hcl::LANGUAGE.into(),
            Language::Wgsl => tree_sitter_wgsl_bevy::LANGUAGE.into(),
            // Tree-sitter query: use Bash grammar fallback for now (still gives *some* structure).
            // TODO: add a dedicated tree-sitter-query grammar crate.
            Language::TreeSitterQuery => tree_sitter_bash::LANGUAGE.into(),
        }
    }

    /// Gets the highlight query for a language
    /// Uses custom queries for better control, falls back to official queries
    pub fn get_highlight_query(lang: Language) -> &'static str {
        match lang {
            // Use custom queries for JS/TS family for better JSX/TSX support
            Language::JavaScript => include_str!("queries/javascript.scm"),
            Language::TypeScript => include_str!("queries/typescript.scm"),
            Language::Tsx => include_str!("queries/tsx.scm"),

            // Use official tree-sitter highlight queries for other languages
            // Note: Some crates use HIGHLIGHTS_QUERY (plural), others use HIGHLIGHT_QUERY (singular)
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Language::Java => tree_sitter_java::HIGHLIGHTS_QUERY,
            // Kotlin: Java highlights as a fallback.
            Language::Kotlin => tree_sitter_java::HIGHLIGHTS_QUERY,
            Language::Scala => tree_sitter_scala::HIGHLIGHTS_QUERY,
            Language::Groovy => include_str!("queries/groovy.scm"),
            Language::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Language::C => tree_sitter_c::HIGHLIGHT_QUERY,
            Language::Cpp => tree_sitter_cpp::HIGHLIGHT_QUERY,
            Language::Ruby => tree_sitter_ruby::HIGHLIGHTS_QUERY,
            Language::Bash => tree_sitter_bash::HIGHLIGHT_QUERY,
            Language::Lua => tree_sitter_lua::HIGHLIGHTS_QUERY,
            Language::Dockerfile => tree_sitter_bash::HIGHLIGHT_QUERY, // Use Bash syntax for now
            Language::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Language::Html => tree_sitter_html::HIGHLIGHTS_QUERY,
            Language::Xml => tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            Language::Astro => tree_sitter_astro_next::HIGHLIGHTS_QUERY,
            Language::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            // TOML uses JSON highlighting as fallback (similar key-value structure)
            Language::Toml => tree_sitter_json::HIGHLIGHTS_QUERY,
            // Custom queries for languages without good official ones
            Language::Yaml => include_str!("queries/yaml.scm"),
            Language::Markdown => include_str!("queries/markdown.scm"),
            Language::Zig => tree_sitter_zig::HIGHLIGHTS_QUERY,
            Language::Sql => tree_sitter_sequel::HIGHLIGHTS_QUERY,
            // C#: Use custom query since tree-sitter-c-sharp doesn't export one
            Language::CSharp => include_str!("queries/csharp.scm"),
            // Terraform/HCL: Use custom query
            Language::Terraform => include_str!("queries/terraform.scm"),
            Language::Hcl => include_str!("queries/terraform.scm"),
            Language::Wgsl => include_str!("queries/wgsl.scm"),
            // Tree-sitter query: basic fallback highlights.
            Language::TreeSitterQuery => include_str!("queries/tree_sitter_query.scm"),
        }
    }

    /// Gets the injection query for a language, if the grammar splits
    /// embedded regions (e.g. Astro's frontmatter/script/style blocks) out
    /// into raw-text nodes that need a separate language's highlighter.
    /// Returns `None` for grammars whose top-level highlight query already
    /// covers the whole file.
    pub fn get_injection_query(lang: Language) -> Option<&'static str> {
        match lang {
            Language::Astro => Some(tree_sitter_astro_next::INJECTIONS_QUERY),
            _ => None,
        }
    }

    /// Get LSP language identifier from file path
    /// Returns None if language is not supported by LSP
    pub fn get_lsp_language_id(file_path: &str) -> Option<&'static str> {
        Self::detect_from_path(file_path).and_then(|lang| match lang {
            Language::Rust => Some("rust"),
            // typescript-language-server uses standard LSP language IDs that
            // distinguish JSX from plain JS.  Both use Language::JavaScript
            // for syntax (tree-sitter-javascript handles JSX), but the LSP
            // ID must match what determine_language_id() used at server
            // registration time.
            Language::JavaScript => {
                let ext = std::path::Path::new(file_path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if ext.eq_ignore_ascii_case("jsx") {
                    Some("javascriptreact")
                } else {
                    Some("javascript")
                }
            }
            Language::TypeScript => Some("typescript"),
            Language::Tsx => Some("typescriptreact"),
            Language::Python => Some("python"),
            Language::Java => Some("java"),
            Language::Kotlin => Some("kotlin"),
            Language::Scala => Some("scala"),
            Language::Groovy => Some("groovy"),
            Language::Go => Some("go"),
            Language::C => Some("c"),
            Language::Cpp => Some("cpp"),
            Language::Ruby => Some("ruby"),
            Language::Bash => Some("bash"),
            Language::Lua => Some("lua"),
            Language::Dockerfile => Some("dockerfile"),
            Language::Json => Some("json"),
            Language::Yaml => Some("yaml"),
            Language::Html => Some("html"),
            Language::Xml => Some("xml"),
            Language::Astro => Some("astro"),
            Language::Css => Some("css"),
            Language::Toml => Some("toml"),
            Language::Markdown => Some("markdown"),
            Language::Zig => Some("zig"),
            Language::Sql => Some("sql"),
            Language::CSharp => Some("csharp"),
            Language::Terraform => Some("terraform"),
            Language::Hcl => Some("hcl"),
            Language::Wgsl => None,
            Language::TreeSitterQuery => None,
        })
    }

    /// Check if a file path has LSP support
    pub fn has_lsp_support(file_path: &str) -> bool {
        Self::get_lsp_language_id(file_path).is_some()
    }

    /// Maps markdown code fence info strings to Language enum
    /// Supports common language names and aliases used in markdown code blocks
    pub fn from_info_string(info: &str) -> Option<Language> {
        match info.trim().to_lowercase().as_str() {
            // Rust
            "rust" | "rs" => Some(Language::Rust),

            // JavaScript
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),

            // TypeScript
            "typescript" | "ts" => Some(Language::TypeScript),

            // TSX
            "tsx" => Some(Language::Tsx),

            // Python
            "python" | "py" | "python3" | "py3" => Some(Language::Python),

            // Java
            "java" => Some(Language::Java),

            // Kotlin
            "kotlin" | "kt" | "kts" => Some(Language::Kotlin),

            // Scala
            "scala" | "sc" | "sbt" => Some(Language::Scala),

            // Groovy / Gradle
            "groovy" | "gradle" => Some(Language::Groovy),

            // Go
            "go" | "golang" => Some(Language::Go),

            // C
            "c" => Some(Language::C),

            // C++
            "cpp" | "c++" | "cxx" | "cc" => Some(Language::Cpp),

            // Ruby
            "ruby" | "rb" => Some(Language::Ruby),

            // Bash/Shell
            "bash" | "sh" | "shell" | "zsh" | "fish" | "ksh" => Some(Language::Bash),

            // Lua
            "lua" => Some(Language::Lua),

            // Dockerfile
            "dockerfile" | "docker" => Some(Language::Dockerfile),

            // JSON
            "json" | "jsonc" => Some(Language::Json),

            // YAML
            "yaml" | "yml" => Some(Language::Yaml),

            // HTML
            "html" | "htm" | "xhtml" => Some(Language::Html),

            // XML
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" => Some(Language::Xml),

            // Astro
            "astro" => Some(Language::Astro),

            // CSS
            "css" | "scss" | "sass" | "less" => Some(Language::Css),

            // TOML
            "toml" => Some(Language::Toml),

            // Markdown (nested markdown in code blocks)
            "markdown" | "md" => Some(Language::Markdown),

            // Zig
            "zig" => Some(Language::Zig),

            // Tree-sitter query language
            "query" | "tree-sitter-query" | "treesitter" => Some(Language::TreeSitterQuery),

            // SQL
            "sql" | "mysql" | "pgsql" | "postgresql" | "sqlite" => Some(Language::Sql),

            // C#
            "csharp" | "cs" | "c#" => Some(Language::CSharp),

            // Terraform / HCL
            "terraform" | "tf" => Some(Language::Terraform),
            "hcl" => Some(Language::Hcl),
            "wgsl" => Some(Language::Wgsl),

            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_astro_files_and_markdown_fences() {
        assert_eq!(
            LanguageRegistry::detect_from_path("src/pages/index.astro"),
            Some(Language::Astro)
        );
        assert_eq!(
            LanguageRegistry::from_info_string("astro"),
            Some(Language::Astro)
        );
        assert_eq!(
            LanguageRegistry::get_lsp_language_id("src/pages/index.astro"),
            Some("astro")
        );
    }

    #[test]
    fn detects_lua_files_and_markdown_fences() {
        assert_eq!(
            LanguageRegistry::detect_from_path("plugin/config.lua"),
            Some(Language::Lua)
        );
        assert_eq!(
            LanguageRegistry::from_info_string("lua"),
            Some(Language::Lua)
        );
        assert_eq!(
            LanguageRegistry::get_lsp_language_id("plugin/config.lua"),
            Some("lua")
        );
    }

    #[test]
    fn detects_wgsl_files_and_markdown_fences() {
        assert_eq!(
            LanguageRegistry::detect_from_path("assets/shaders/void.wgsl"),
            Some(Language::Wgsl)
        );
        assert_eq!(
            LanguageRegistry::from_info_string("wgsl"),
            Some(Language::Wgsl)
        );
        assert_eq!(LanguageRegistry::get_lsp_language_id("shader.wgsl"), None);
    }

    #[test]
    fn detects_xml_files_and_markdown_fences() {
        for path in ["document.xml", "schema.xsd", "transform.xslt", "icon.svg"] {
            assert_eq!(
                LanguageRegistry::detect_from_path(path),
                Some(Language::Xml)
            );
        }
        assert_eq!(
            LanguageRegistry::from_info_string("xml"),
            Some(Language::Xml)
        );
        assert_eq!(
            LanguageRegistry::get_lsp_language_id("document.xml"),
            Some("xml")
        );

        let language = LanguageRegistry::get_tree_sitter_language(Language::Xml);
        tree_sitter::Query::new(
            &language,
            LanguageRegistry::get_highlight_query(Language::Xml),
        )
        .expect("XML highlight query should compile");

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser
            .parse(
                "<?xml version=\"1.0\"?><root id=\"1\"><child /></root>",
                None,
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
    }
}
