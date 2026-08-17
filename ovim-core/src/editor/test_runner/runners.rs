//! Test command construction per language.
//!
//! Design (informed by vim-test and neotest, improving on both):
//! - The project root is resolved **per file** from root markers (nearest
//!   `Cargo.toml`, `package.json`, `go.mod`, …), not from the editor cwd.
//!   This is vim-test's biggest monorepo wart (vim-test#272/#490) fixed
//!   natively: commands always run in the right package directory.
//! - Test-name filters use three distinct dialects with their own escaping:
//!   literal ids (pytest `file::Class::test`), anchored regex
//!   (`go test -run`, jest/vitest `-t`), and exact names
//!   (`cargo test full::path -- --exact`).
//! - Parameterized tests (`#[rstest]`, `it.each`, go table entries) drop
//!   exact matching / trailing anchors so generated case names still match.

use super::nearest::{discover_tests, nearest_test, DiscoveredTest, TestFlavor};
use crate::language_config::TestConfig;
use crate::syntax::Language;
use std::path::{Path, PathBuf};

/// What to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestScope {
    Nearest,
    File,
    Suite,
}

/// A fully resolved test command.
#[derive(Debug, Clone)]
pub struct TestInvocation {
    /// Shell command line (run via `sh -c`).
    pub command: String,
    /// Working directory (project/package root).
    pub cwd: PathBuf,
}

/// Everything command construction needs to know about the current buffer.
pub struct TestContext<'a> {
    /// Absolute path of the file being tested.
    pub file: &'a Path,
    /// Buffer content (used for tree-sitter discovery).
    pub source: &'a str,
    /// 0-indexed cursor line.
    pub cursor_line: usize,
    /// Detected syntax language, if any.
    pub language: Option<Language>,
    /// `[language.test]` config override, if any.
    pub config: Option<&'a TestConfig>,
}

/// Builds the command for the requested scope, or a user-facing error.
pub fn build_test_command(scope: TestScope, ctx: &TestContext) -> Result<TestInvocation, String> {
    // A user-provided command template takes precedence when it covers the
    // requested scope; otherwise fall through to the built-in runner.
    if let Some(cfg) = ctx.config {
        let template = match scope {
            TestScope::Nearest => cfg.nearest_command.as_deref(),
            TestScope::File => cfg.file_command.as_deref(),
            TestScope::Suite => cfg.suite_command.as_deref(),
        };
        if let Some(template) = template {
            return build_from_template(template, &cfg.root_markers, scope, ctx);
        }
    }

    match ctx.language {
        Some(Language::Rust) => rust::build(scope, ctx),
        Some(Language::JavaScript) | Some(Language::TypeScript) | Some(Language::Tsx) => {
            js::build(scope, ctx)
        }
        Some(Language::Python) => python::build(scope, ctx),
        Some(Language::Go) => go::build(scope, ctx),
        _ => Err(
            "No test runner for this file type (configure [language.test] in languages.toml)"
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Walks up from `start`'s directory looking for any of `markers`.
/// Returns the first directory containing one.
fn find_up(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dir = if start.is_dir() {
        start
    } else {
        start.parent()?
    };
    loop {
        for marker in markers {
            if dir.join(marker).exists() {
                return Some(dir.to_path_buf());
            }
        }
        // Stop at the home directory (checked, but never walked past):
        // a stray ~/.git or ~/package.json must not claim every project
        // below it (neotest stops at $HOME for the same reason).
        if home.as_deref() == Some(dir) {
            return None;
        }
        dir = dir.parent()?;
    }
}

/// Wraps a string in single quotes, escaping embedded single quotes the
/// POSIX way (`'` → `'\''`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escapes regex metacharacters for filters that interpret their argument as
/// a regex (`go test -run`, jest/vitest `-t`). Escapes the full metachar set
/// including `{`, `}` and `\` — vim-test's go runner misses those
/// (its escape set omits braces and backslash, generating broken patterns).
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn rel_path(file: &Path, root: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .to_string()
}

/// Discovers the nearest test, as a user-facing result.
fn require_nearest<'t>(
    tests: &'t [DiscoveredTest],
    ctx: &TestContext,
) -> Result<&'t DiscoveredTest, String> {
    nearest_test(tests, ctx.cursor_line).ok_or_else(|| "No test found near cursor".to_string())
}

fn discover(ctx: &TestContext) -> Vec<DiscoveredTest> {
    match ctx.language {
        Some(lang) => discover_tests(lang, ctx.source),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Config-template runner
// ---------------------------------------------------------------------------

fn build_from_template(
    template: &str,
    root_markers: &[String],
    scope: TestScope,
    ctx: &TestContext,
) -> Result<TestInvocation, String> {
    let markers: Vec<&str> = root_markers.iter().map(|s| s.as_str()).collect();
    let root = if markers.is_empty() {
        builtin_root(ctx)
    } else {
        find_up(ctx.file, &markers)
    }
    .or_else(|| ctx.file.parent().map(|p| p.to_path_buf()))
    .ok_or_else(|| "Could not determine project root".to_string())?;

    let mut command = template.to_string();
    if command.contains("{file}") {
        command = command.replace("{file}", &rel_path(ctx.file, &root));
    }
    if command.contains("{line}") {
        command = command.replace("{line}", &(ctx.cursor_line + 1).to_string());
    }
    if command.contains("{name}") {
        if scope != TestScope::Nearest {
            return Err("{name} is only available in nearest_command".to_string());
        }
        let tests = discover(ctx);
        let test = require_nearest(&tests, ctx)?;
        let mut parts = test.namespaces.clone();
        parts.push(test.name.clone());
        // Escaped for single-quoted shell interpolation; the template
        // decides how to quote (documented as `-t '{name}'`).
        command = command.replace("{name}", &parts.join(" ").replace('\'', r"'\''"));
    }
    Ok(TestInvocation { command, cwd: root })
}

/// Root detection for the built-in runner of the context's language, reused
/// when a config override has no root_markers of its own.
fn builtin_root(ctx: &TestContext) -> Option<PathBuf> {
    match ctx.language {
        Some(Language::Rust) => find_up(ctx.file, &["Cargo.toml"]),
        Some(Language::JavaScript) | Some(Language::TypeScript) | Some(Language::Tsx) => {
            find_up(ctx.file, &["package.json"])
        }
        Some(Language::Python) => python::find_root(ctx.file),
        Some(Language::Go) => find_up(ctx.file, &["go.mod"]),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rust (cargo test)
// ---------------------------------------------------------------------------

mod rust {
    use super::*;

    /// How the file maps onto a cargo target.
    enum Target {
        /// `tests/<name>.rs` or `tests/<name>/…` — `--test <name>`
        Integration {
            name: String,
            module_prefix: Vec<String>,
        },
        /// `src/bin/<name>.rs` or `src/bin/<name>/…` — `--bin <name>`
        Bin {
            name: String,
            module_prefix: Vec<String>,
        },
        /// Ordinary `src/**` file — module path derived from the path
        Lib { module_prefix: Vec<String> },
    }

    pub(super) fn build(scope: TestScope, ctx: &TestContext) -> Result<TestInvocation, String> {
        let package_root = find_up(ctx.file, &["Cargo.toml"])
            .ok_or_else(|| "No Cargo.toml found above this file".to_string())?;

        if scope == TestScope::Suite {
            let (root, cmd) = suite_command(&package_root);
            return Ok(TestInvocation {
                command: cmd,
                cwd: root,
            });
        }

        let target = classify(ctx.file, &package_root);
        let target_flag = match &target {
            Target::Integration { name, .. } => format!(" --test {}", shell_quote(name)),
            Target::Bin { name, .. } => format!(" --bin {}", shell_quote(name)),
            Target::Lib { .. } => String::new(),
        };
        let module_prefix = match &target {
            Target::Integration { module_prefix, .. }
            | Target::Bin { module_prefix, .. }
            | Target::Lib { module_prefix } => module_prefix.clone(),
        };

        let command = match scope {
            TestScope::File => {
                if module_prefix.is_empty() {
                    // lib.rs / main.rs / tests/foo.rs: the target flag alone
                    // scopes the run.
                    format!("cargo test{}", target_flag)
                } else {
                    // Substring filter over the module path. Like vim-test,
                    // this also matches child modules — for "run this file's
                    // tests" that is a feature more than a bug.
                    format!(
                        "cargo test{} {}",
                        target_flag,
                        shell_quote(&format!("{}::", module_prefix.join("::")))
                    )
                }
            }
            TestScope::Nearest => {
                let tests = discover(ctx);
                let test = require_nearest(&tests, ctx)?;
                let mut path = module_prefix;
                path.extend(test.namespaces.iter().cloned());
                path.push(test.name.clone());
                let full = path.join("::");
                match test.flavor {
                    // Full path + --exact: never runs sibling tests that
                    // share a name prefix (the old bare-name matching did).
                    TestFlavor::Exact => format!(
                        "cargo test{} {} -- --exact",
                        target_flag,
                        shell_quote(&full)
                    ),
                    // rstest/test_case generate `name::case_1`-style names;
                    // prefix matching catches all cases.
                    TestFlavor::Parameterized => {
                        format!("cargo test{} {}", target_flag, shell_quote(&full))
                    }
                }
            }
            TestScope::Suite => unreachable!(),
        };

        Ok(TestInvocation {
            command,
            cwd: package_root,
        })
    }

    /// Suite scope runs the whole workspace from the workspace root when the
    /// package belongs to one.
    fn suite_command(package_root: &Path) -> (PathBuf, String) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dir = package_root.parent();
        while let Some(d) = dir {
            let manifest = d.join("Cargo.toml");
            if manifest.exists() {
                if let Ok(content) = std::fs::read_to_string(&manifest) {
                    if content.contains("[workspace]") {
                        return (d.to_path_buf(), "cargo test --workspace".to_string());
                    }
                }
            }
            // Same $HOME guard as find_up: never treat a manifest above the
            // home directory as this package's workspace.
            if home.as_deref() == Some(d) {
                break;
            }
            dir = d.parent();
        }
        // The package's own manifest may be the workspace root.
        if let Ok(content) = std::fs::read_to_string(package_root.join("Cargo.toml")) {
            if content.contains("[workspace]") {
                return (
                    package_root.to_path_buf(),
                    "cargo test --workspace".to_string(),
                );
            }
        }
        (package_root.to_path_buf(), "cargo test".to_string())
    }

    fn classify(file: &Path, package_root: &Path) -> Target {
        let rel = file.strip_prefix(package_root).unwrap_or(file);
        let components: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();

        match components.first().map(String::as_str) {
            Some("tests") if components.len() >= 2 => {
                let name = components[1].trim_end_matches(".rs").to_string();
                // tests/foo.rs → target foo, no module prefix.
                // tests/foo/bar.rs → target foo, module prefix [bar].
                let module_prefix = module_path_from(&components[2..]);
                Target::Integration {
                    name,
                    module_prefix,
                }
            }
            Some("src")
                if components.get(1).map(String::as_str) == Some("bin")
                    && components.len() >= 3 =>
            {
                let name = components[2].trim_end_matches(".rs").to_string();
                let module_prefix = module_path_from(&components[3..]);
                Target::Bin {
                    name,
                    module_prefix,
                }
            }
            Some("src") => Target::Lib {
                module_prefix: module_path_from(&components[1..]),
            },
            _ => Target::Lib {
                module_prefix: Vec::new(),
            },
        }
    }

    /// Converts path components (already relative to the module root) into a
    /// rust module path: strips `.rs`, drops crate/module roots
    /// (`main.rs`, `lib.rs`, `mod.rs`). This replaces the old
    /// `replace("mod", "")` string surgery that mangled any path containing
    /// the letters "mod" (`src/mode.rs` → filter `e`).
    fn module_path_from(components: &[String]) -> Vec<String> {
        let mut mods = Vec::new();
        for (i, comp) in components.iter().enumerate() {
            let is_last = i + 1 == components.len();
            if is_last {
                if matches!(comp.as_str(), "main.rs" | "lib.rs" | "mod.rs") {
                    break;
                }
                mods.push(comp.trim_end_matches(".rs").to_string());
            } else {
                mods.push(comp.clone());
            }
        }
        mods
    }
}

// ---------------------------------------------------------------------------
// JavaScript / TypeScript (vitest, jest, bun, npm)
// ---------------------------------------------------------------------------

mod js {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Runner {
        Vitest,
        Jest,
        Bun,
        NpmTest,
    }

    pub(super) fn build(scope: TestScope, ctx: &TestContext) -> Result<TestInvocation, String> {
        let pkg_root = find_up(ctx.file, &["package.json"])
            .ok_or_else(|| "No package.json found above this file".to_string())?;
        let runner = detect_runner(&pkg_root);
        let rel = rel_path(ctx.file, &pkg_root);

        let command = match (&runner, scope) {
            (Runner::NpmTest, _) => {
                // npm scripts can't target files or names; degrade gracefully.
                "npm test".to_string()
            }
            (Runner::Vitest, TestScope::Suite) => {
                format!("{} run", executable(&pkg_root, "vitest"))
            }
            (Runner::Vitest, TestScope::File) => {
                format!(
                    "{} run {}",
                    executable(&pkg_root, "vitest"),
                    shell_quote(&rel)
                )
            }
            (Runner::Vitest, TestScope::Nearest) => {
                let pattern = name_pattern(ctx, true)?;
                format!(
                    "{} run -t {} {}",
                    executable(&pkg_root, "vitest"),
                    shell_quote(&pattern),
                    shell_quote(&rel)
                )
            }
            (Runner::Jest, TestScope::Suite) => executable(&pkg_root, "jest"),
            (Runner::Jest, TestScope::File) => format!(
                "{} --runTestsByPath {}",
                executable(&pkg_root, "jest"),
                shell_quote(&rel)
            ),
            (Runner::Jest, TestScope::Nearest) => {
                let pattern = name_pattern(ctx, true)?;
                format!(
                    "{} --runTestsByPath -t {} {}",
                    executable(&pkg_root, "jest"),
                    shell_quote(&pattern),
                    shell_quote(&rel)
                )
            }
            (Runner::Bun, TestScope::Suite) => "bun test".to_string(),
            (Runner::Bun, TestScope::File) => format!("bun test {}", shell_quote(&rel)),
            (Runner::Bun, TestScope::Nearest) => {
                // bun's -t is unanchored; keep the escaped name only.
                let pattern = name_pattern(ctx, false)?;
                format!(
                    "bun test -t {} {}",
                    shell_quote(&pattern),
                    shell_quote(&rel)
                )
            }
        };

        Ok(TestInvocation {
            command,
            cwd: pkg_root,
        })
    }

    /// Builds the `-t` regex for the nearest test: namespaces and name joined
    /// with spaces (matching how jest/vitest assemble full names), regex
    /// escaped. Anchored `^…$` only for plain (non-parameterized) tests when
    /// the runner honors anchors — `it.each` templates get their printf token
    /// and everything after it cut off, unanchored (vim-test/jest behavior).
    fn name_pattern(ctx: &TestContext, anchor: bool) -> Result<String, String> {
        let tests = discover(ctx);
        let test = require_nearest(&tests, ctx)?;
        let mut parts = test.namespaces.clone();
        parts.push(test.name.clone());
        let full = parts.join(" ");

        match test.flavor {
            TestFlavor::Exact if anchor => Ok(format!("^{}$", regex_escape(&full))),
            TestFlavor::Exact => Ok(regex_escape(&full)),
            TestFlavor::Parameterized => {
                let truncated = truncate_at_dynamic_token(&full);
                Ok(regex_escape(truncated.trim_end()))
            }
        }
    }

    /// Cuts a parameterized name at the first `%x` printf token or `${`
    /// interpolation, since everything from there on is runtime-generated.
    fn truncate_at_dynamic_token(name: &str) -> &str {
        let bytes = name.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'%' => {
                        i += 2;
                        continue;
                    }
                    b's' | b'd' | b'i' | b'f' | b'j' | b'o' | b'p' | b'#' => return &name[..i],
                    _ => {}
                }
            }
            if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                return &name[..i];
            }
            i += 1;
        }
        name
    }

    /// Detects which runner owns this package: dependencies and config files
    /// at the package root first, then ancestor packages (covers hoisted
    /// monorepo setups where vitest/jest live in the workspace root), then
    /// lockfile hints, then the test script, then plain `npm test`.
    fn detect_runner(pkg_root: &Path) -> Runner {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dir = Some(pkg_root);
        while let Some(d) = dir {
            if let Some(runner) = detect_in_dir(d) {
                return runner;
            }
            if home.as_deref() == Some(d) {
                break;
            }
            dir = d.parent();
        }
        if let Some(script) = read_test_script(pkg_root) {
            if script.contains("vitest") {
                return Runner::Vitest;
            }
            if script.contains("jest") {
                return Runner::Jest;
            }
            if script.contains("bun test") {
                return Runner::Bun;
            }
        }
        Runner::NpmTest
    }

    fn detect_in_dir(dir: &Path) -> Option<Runner> {
        let pkg = dir.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pkg) {
            // Cheap dependency sniff — a JSON parse would be sturdier, but a
            // quoted key match has no false positives worth worrying about.
            if content.contains("\"vitest\"") {
                return Some(Runner::Vitest);
            }
            if content.contains("\"jest\"") {
                return Some(Runner::Jest);
            }
        }
        for ext in ["ts", "js", "mts", "mjs", "cts", "cjs"] {
            if dir.join(format!("vitest.config.{ext}")).exists() {
                return Some(Runner::Vitest);
            }
            if dir.join(format!("jest.config.{ext}")).exists() {
                return Some(Runner::Jest);
            }
        }
        if dir.join("jest.config.json").exists() {
            return Some(Runner::Jest);
        }
        if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
            return Some(Runner::Bun);
        }
        None
    }

    fn read_test_script(pkg_root: &Path) -> Option<String> {
        let content = std::fs::read_to_string(pkg_root.join("package.json")).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        value
            .get("scripts")?
            .get("test")?
            .as_str()
            .map(String::from)
    }

    /// Prefers the locally installed binary (fast, version-correct); falls
    /// back to `npx` which resolves-or-installs.
    fn executable(pkg_root: &Path, bin: &str) -> String {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dir = Some(pkg_root);
        while let Some(d) = dir {
            let candidate = d.join("node_modules").join(".bin").join(bin);
            if candidate.exists() {
                return shell_quote(&candidate.to_string_lossy());
            }
            if home.as_deref() == Some(d) {
                break;
            }
            dir = d.parent();
        }
        format!("npx {bin}")
    }
}

// ---------------------------------------------------------------------------
// Python (pytest)
// ---------------------------------------------------------------------------

mod python {
    use super::*;

    pub(super) fn find_root(file: &Path) -> Option<PathBuf> {
        find_up(
            file,
            &[
                "pytest.ini",
                "pyproject.toml",
                "setup.cfg",
                "setup.py",
                "tox.ini",
            ],
        )
        .or_else(|| find_up(file, &[".git"]))
    }

    pub(super) fn build(scope: TestScope, ctx: &TestContext) -> Result<TestInvocation, String> {
        let root = find_root(ctx.file)
            .or_else(|| ctx.file.parent().map(Path::to_path_buf))
            .ok_or_else(|| "Could not determine project root".to_string())?;
        let pytest = format!("{}pytest", runner_prefix(&root));
        let rel = rel_path(ctx.file, &root);

        let command = match scope {
            TestScope::Suite => pytest,
            TestScope::File => format!("{} {}", pytest, shell_quote(&rel)),
            TestScope::Nearest => {
                let tests = discover(ctx);
                let test = require_nearest(&tests, ctx)?;
                // pytest node ids are literal — no escaping dialect at all.
                let mut node_id = rel.clone();
                for ns in &test.namespaces {
                    node_id.push_str("::");
                    node_id.push_str(ns);
                }
                node_id.push_str("::");
                node_id.push_str(&test.name);
                format!("{} {}", pytest, shell_quote(&node_id))
            }
        };

        Ok(TestInvocation { command, cwd: root })
    }

    /// Environment-manager prefix, detected from lockfiles (vim-test's
    /// approach): `uv run pytest`, `poetry run pytest`, etc. Defaults to
    /// `python3 -m pytest` so the interpreter's environment resolves pytest.
    fn runner_prefix(root: &Path) -> &'static str {
        if root.join("uv.lock").exists() {
            "uv run "
        } else if root.join("poetry.lock").exists() {
            "poetry run "
        } else if root.join("Pipfile").exists() {
            "pipenv run "
        } else if root.join("pdm.lock").exists() {
            "pdm run "
        } else {
            "python3 -m "
        }
    }
}

// ---------------------------------------------------------------------------
// Go (go test)
// ---------------------------------------------------------------------------

mod go {
    use super::*;

    pub(super) fn build(scope: TestScope, ctx: &TestContext) -> Result<TestInvocation, String> {
        let root = find_up(ctx.file, &["go.mod"])
            .or_else(|| ctx.file.parent().map(Path::to_path_buf))
            .ok_or_else(|| "Could not determine module root".to_string())?;
        let pkg = package_arg(ctx.file, &root);

        let command = match scope {
            TestScope::Suite => "go test ./...".to_string(),
            TestScope::File => {
                // `go test` has no file granularity; approximate it by
                // running every top-level test func found in this file
                // (vim-test runs the whole package here — this is tighter).
                let tests = discover(ctx);
                let mut names: Vec<String> = Vec::new();
                for t in &tests {
                    if t.namespaces.is_empty() && !names.contains(&t.name) {
                        names.push(regex_escape(&t.name));
                    }
                }
                if names.is_empty() {
                    format!("go test {}", pkg)
                } else {
                    format!(
                        "go test -run {} {}",
                        shell_quote(&format!("^({})$", names.join("|"))),
                        pkg
                    )
                }
            }
            TestScope::Nearest => {
                let tests = discover(ctx);
                let test = require_nearest(&tests, ctx)?;
                let mut elements: Vec<String> = Vec::new();
                for ns in &test.namespaces {
                    elements.push(format!("^{}$", run_element(ns)));
                }
                // Go's runtime rewrites spaces in subtest names to
                // underscores; table entries stay unanchored because the
                // runtime name is generated (vim-test behavior).
                match test.flavor {
                    TestFlavor::Exact => elements.push(format!("^{}$", run_element(&test.name))),
                    TestFlavor::Parameterized => elements.push(run_element(&test.name)),
                }
                format!("go test -run {} {}", shell_quote(&elements.join("/")), pkg)
            }
        };

        Ok(TestInvocation { command, cwd: root })
    }

    /// A single `/`-separated element of a `-run` pattern: spaces become
    /// underscores (go's own subtest-name mangling), then regex escaping.
    fn run_element(name: &str) -> String {
        regex_escape(&name.replace(' ', "_"))
    }

    fn package_arg(file: &Path, root: &Path) -> String {
        let rel_dir = file
            .parent()
            .and_then(|d| d.strip_prefix(root).ok())
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default();
        if rel_dir.is_empty() {
            ".".to_string()
        } else {
            format!("./{}", rel_dir)
        }
    }
}
