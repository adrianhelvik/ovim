//! Tests for tree-sitter test discovery and command construction.
//!
//! Expected commands are derived from the actual CLI contracts of cargo
//! test, vitest, jest, pytest and go test (see runners.rs doc comments for
//! the dialect rules), cross-checked against vim-test's runner sources and
//! its issue tracker's known bugs — several cases below are regression
//! tests for bugs vim-test still has.

use super::nearest::{discover_tests, nearest_test, TestFlavor};
use super::runners::{build_test_command, TestContext, TestScope};
use crate::language_config::TestConfig;
use crate::syntax::Language;
use std::fs;
use std::path::{Path, PathBuf};

fn ctx<'a>(
    file: &'a Path,
    source: &'a str,
    cursor_line: usize,
    language: Language,
) -> TestContext<'a> {
    TestContext {
        file,
        source,
        cursor_line,
        language: Some(language),
        config: None,
    }
}

// ---------------------------------------------------------------------------
// Rust discovery
// ---------------------------------------------------------------------------

const RUST_SRC: &str = r#"
fn helper() {}

#[test]
fn top_level_test() {
    assert!(true);
}

mod outer {
    mod tests {
        #[test]
        fn nested_test() {}

        #[tokio::test]
        async fn async_test() {}

        #[rstest]
        #[case(1)]
        fn param_test(#[case] n: u32) {}
    }
}
"#;

#[test]
fn rust_discovers_tests_with_module_paths() {
    let tests = discover_tests(Language::Rust, RUST_SRC);
    let names: Vec<(String, Vec<String>)> = tests
        .iter()
        .map(|t| (t.name.clone(), t.namespaces.clone()))
        .collect();
    assert!(names.contains(&("top_level_test".into(), vec![])));
    assert!(names.contains(&("nested_test".into(), vec!["outer".into(), "tests".into()])));
    assert!(names.contains(&("async_test".into(), vec!["outer".into(), "tests".into()])));
}

#[test]
fn rust_rstest_is_parameterized() {
    let tests = discover_tests(Language::Rust, RUST_SRC);
    let param = tests.iter().find(|t| t.name == "param_test").unwrap();
    assert_eq!(param.flavor, TestFlavor::Parameterized);
    let plain = tests.iter().find(|t| t.name == "top_level_test").unwrap();
    assert_eq!(plain.flavor, TestFlavor::Exact);
}

#[test]
fn rust_ignores_non_test_functions() {
    let tests = discover_tests(Language::Rust, RUST_SRC);
    assert!(!tests.iter().any(|t| t.name == "helper"));
}

#[test]
fn rust_attribute_with_comment_between() {
    let src = "#[test]\n// a comment\nfn commented_test() {}\n";
    let tests = discover_tests(Language::Rust, src);
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].name, "commented_test");
}

// ---------------------------------------------------------------------------
// Nearest selection
// ---------------------------------------------------------------------------

#[test]
fn nearest_prefers_containing_then_above_then_below() {
    let tests = discover_tests(Language::Rust, RUST_SRC);
    // Cursor inside top_level_test's body (line 5 of the literal, 0-indexed 4).
    let inside = nearest_test(&tests, 4).unwrap();
    assert_eq!(inside.name, "top_level_test");
    // Cursor after top_level_test but before the mod: picks the test above.
    let above = nearest_test(&tests, 7).unwrap();
    assert_eq!(above.name, "top_level_test");
    // Cursor at the top of the file: falls forward to the first test.
    let below = nearest_test(&tests, 0).unwrap();
    assert_eq!(below.name, "top_level_test");
}

// ---------------------------------------------------------------------------
// Rust command construction
// ---------------------------------------------------------------------------

struct RustProject {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn rust_package() -> RustProject {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src/editor")).unwrap();
    fs::create_dir_all(root.join("src/bin")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    RustProject { _dir: dir, root }
}

#[test]
fn rust_nearest_uses_full_module_path_and_exact() {
    let p = rust_package();
    let file = p.root.join("src/editor/input.rs");
    fs::write(&file, "").unwrap();
    let src = "mod tests {\n    #[test]\n    fn handles_keys() {}\n}\n";
    let inv = build_test_command(TestScope::Nearest, &ctx(&file, src, 2, Language::Rust)).unwrap();
    assert_eq!(
        inv.command,
        "cargo test 'editor::input::tests::handles_keys' -- --exact"
    );
    assert_eq!(inv.cwd, p.root);
}

#[test]
fn rust_file_scope_module_filter_survives_mod_in_name() {
    // Regression: the old implementation ran `.replace("mod", "")` on the
    // path, so src/editor/mode.rs produced the filter `editor::e`.
    let p = rust_package();
    let file = p.root.join("src/editor/mode.rs");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test 'editor::mode::'");
}

#[test]
fn rust_mod_rs_maps_to_parent_module() {
    let p = rust_package();
    let file = p.root.join("src/editor/mod.rs");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test 'editor::'");
}

#[test]
fn rust_integration_test_uses_test_target() {
    // vim-test never emits --test for integration targets (open TODO there).
    let p = rust_package();
    let file = p.root.join("tests/api_test.rs");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test --test 'api_test'");

    let src = "#[test]\nfn hits_endpoint() {}\n";
    let inv = build_test_command(TestScope::Nearest, &ctx(&file, src, 1, Language::Rust)).unwrap();
    assert_eq!(
        inv.command,
        "cargo test --test 'api_test' 'hits_endpoint' -- --exact"
    );
}

#[test]
fn rust_bin_target() {
    let p = rust_package();
    let file = p.root.join("src/bin/tool.rs");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test --bin 'tool'");
}

#[test]
fn rust_lib_root_runs_whole_package() {
    let p = rust_package();
    let file = p.root.join("src/lib.rs");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test");
}

#[test]
fn rust_suite_uses_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    fs::write(ws.join("Cargo.toml"), "[workspace]\nmembers = [\"demo\"]\n").unwrap();
    fs::create_dir_all(ws.join("demo/src")).unwrap();
    fs::write(
        ws.join("demo/Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let file = ws.join("demo/src/lib.rs");
    fs::write(&file, "").unwrap();

    let inv = build_test_command(TestScope::Suite, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test --workspace");
    assert_eq!(inv.cwd, ws);

    // File scope still runs in the package, not the workspace root.
    let inv = build_test_command(TestScope::File, &ctx(&file, "", 0, Language::Rust)).unwrap();
    assert_eq!(inv.cwd, ws.join("demo"));
}

#[test]
fn rust_parameterized_nearest_drops_exact() {
    let p = rust_package();
    let file = p.root.join("src/lib.rs");
    fs::write(&file, "").unwrap();
    let src = "#[rstest]\n#[case(1)]\nfn with_cases(#[case] n: u32) {}\n";
    let inv = build_test_command(TestScope::Nearest, &ctx(&file, src, 2, Language::Rust)).unwrap();
    assert_eq!(inv.command, "cargo test 'with_cases'");
}

// ---------------------------------------------------------------------------
// JavaScript / TypeScript
// ---------------------------------------------------------------------------

const JS_SRC: &str = r#"
describe('math utils', () => {
  it('adds numbers', () => {
    expect(1 + 1).toBe(2);
  });

  describe('edge cases', () => {
    it("doesn't overflow (hopefully?)", () => {});
  });

  it.each([1, 2])('handles %d items', (n) => {});
});
"#;

#[test]
fn js_discovers_nested_describes() {
    let tests = discover_tests(Language::TypeScript, JS_SRC);
    let nested = tests
        .iter()
        .find(|t| t.name.starts_with("doesn't"))
        .unwrap();
    assert_eq!(
        nested.namespaces,
        vec!["math utils".to_string(), "edge cases".to_string()]
    );
    let each = tests.iter().find(|t| t.name.contains("%d")).unwrap();
    assert_eq!(each.flavor, TestFlavor::Parameterized);
}

struct JsProject {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn js_package(package_json: &str) -> JsProject {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("package.json"), package_json).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    JsProject { _dir: dir, root }
}

#[test]
fn js_vitest_nearest_is_anchored_and_escaped() {
    // Regression class: vim-test#625 — quotes/regex chars in test names must
    // survive both regex escaping and shell quoting.
    let p = js_package(r#"{"devDependencies": {"vitest": "^2.0.0"}}"#);
    let file = p.root.join("src/math.test.ts");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(
        TestScope::Nearest,
        &ctx(&file, JS_SRC, 7, Language::TypeScript),
    )
    .unwrap();
    assert_eq!(
        inv.command,
        r#"npx vitest run -t '^math utils edge cases doesn'\''t overflow \(hopefully\?\)$' 'src/math.test.ts'"#
    );
}

#[test]
fn js_each_printf_name_is_truncated_and_unanchored() {
    let p = js_package(r#"{"devDependencies": {"vitest": "^2.0.0"}}"#);
    let file = p.root.join("src/math.test.ts");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(
        TestScope::Nearest,
        &ctx(&file, JS_SRC, 10, Language::TypeScript),
    )
    .unwrap();
    assert_eq!(
        inv.command,
        "npx vitest run -t 'math utils handles' 'src/math.test.ts'"
    );
}

#[test]
fn js_jest_detected_from_config_file() {
    let p = js_package(r#"{"name": "demo"}"#);
    fs::write(p.root.join("jest.config.js"), "module.exports = {}").unwrap();
    let file = p.root.join("src/math.test.ts");
    fs::write(&file, "").unwrap();
    let inv =
        build_test_command(TestScope::File, &ctx(&file, "", 0, Language::TypeScript)).unwrap();
    assert_eq!(inv.command, "npx jest --runTestsByPath 'src/math.test.ts'");
}

#[test]
fn js_monorepo_finds_hoisted_runner_and_package_cwd() {
    // vim-test#272/#490: detection reads vim's cwd, so nested packages fail.
    // Here the runner lives in the workspace root and the file in a package.
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    fs::write(
        ws.join("package.json"),
        r#"{"devDependencies": {"vitest": "1.0.0"}, "workspaces": ["packages/*"]}"#,
    )
    .unwrap();
    fs::create_dir_all(ws.join("packages/app/src")).unwrap();
    fs::write(ws.join("packages/app/package.json"), r#"{"name": "app"}"#).unwrap();
    let file = ws.join("packages/app/src/app.test.ts");
    fs::write(&file, "").unwrap();

    let inv =
        build_test_command(TestScope::File, &ctx(&file, "", 0, Language::TypeScript)).unwrap();
    // cwd is the package, not the workspace; runner found by walking up.
    assert_eq!(inv.cwd, ws.join("packages/app"));
    assert_eq!(inv.command, "npx vitest run 'src/app.test.ts'");
}

#[test]
fn js_bun_detected_from_lockfile() {
    let p = js_package(r#"{"name": "demo"}"#);
    fs::write(p.root.join("bun.lock"), "").unwrap();
    let file = p.root.join("src/x.test.ts");
    fs::write(&file, "").unwrap();
    let inv =
        build_test_command(TestScope::File, &ctx(&file, "", 0, Language::TypeScript)).unwrap();
    assert_eq!(inv.command, "bun test 'src/x.test.ts'");
}

#[test]
fn js_npm_fallback_when_no_runner_found() {
    let p = js_package(r#"{"name": "demo", "scripts": {"test": "./custom-runner.sh"}}"#);
    let file = p.root.join("src/x.test.ts");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(
        TestScope::Nearest,
        &ctx(&file, JS_SRC, 2, Language::TypeScript),
    )
    .unwrap();
    assert_eq!(inv.command, "npm test");
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

const PY_SRC: &str = r#"
import pytest

class TestMath:
    def test_addition(self):
        assert 1 + 1 == 2

    class TestNested:
        def test_deep(self):
            pass

@pytest.mark.parametrize("n", [1, 2])
def test_param(n):
    assert n > 0
"#;

#[test]
fn python_nearest_builds_node_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("pyproject.toml"), "[tool.pytest.ini_options]\n").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    let file = root.join("tests/test_math.py");
    fs::write(&file, "").unwrap();

    let inv =
        build_test_command(TestScope::Nearest, &ctx(&file, PY_SRC, 8, Language::Python)).unwrap();
    assert_eq!(
        inv.command,
        "python3 -m pytest 'tests/test_math.py::TestMath::TestNested::test_deep'"
    );
    assert_eq!(inv.cwd, root);
}

#[test]
fn python_decorated_test_selected_from_decorator_line() {
    let tests = discover_tests(Language::Python, PY_SRC);
    // Cursor on the @pytest.mark.parametrize line (0-indexed 11).
    let t = nearest_test(&tests, 11).unwrap();
    assert_eq!(t.name, "test_param");
}

#[test]
fn python_uv_lock_prefixes_runner() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(root.join("uv.lock"), "").unwrap();
    let file = root.join("test_x.py");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::Suite, &ctx(&file, "", 0, Language::Python)).unwrap();
    assert_eq!(inv.command, "uv run pytest");
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

const GO_SRC: &str = r#"package math

import "testing"

func helper() {}

func TestAdd(t *testing.T) {
    t.Run("with negative numbers", func(t *testing.T) {
        // ...
    })
}

func TestTable(t *testing.T) {
    cases := []struct {
        name string
        n    int
    }{
        {name: "adds two numbers", n: 2},
        {name: "handles (parens)", n: 3},
    }
    for _, tc := range cases {
        t.Run(tc.name, func(t *testing.T) {})
    }
}
"#;

struct GoProject {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn go_module() -> GoProject {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("go.mod"), "module example.com/demo\n").unwrap();
    fs::create_dir_all(root.join("math")).unwrap();
    GoProject { _dir: dir, root }
}

#[test]
fn go_subtest_run_pattern_with_underscores() {
    let p = go_module();
    let file = p.root.join("math/add_test.go");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::Nearest, &ctx(&file, GO_SRC, 8, Language::Go)).unwrap();
    assert_eq!(
        inv.command,
        "go test -run '^TestAdd$/^with_negative_numbers$' ./math"
    );
    assert_eq!(inv.cwd, p.root);
}

#[test]
fn go_table_entry_is_unanchored_and_escaped() {
    // vim-test's escape set misses braces/backslash; ours escapes parens too.
    let p = go_module();
    let file = p.root.join("math/add_test.go");
    fs::write(&file, "").unwrap();
    let inv =
        build_test_command(TestScope::Nearest, &ctx(&file, GO_SRC, 18, Language::Go)).unwrap();
    assert_eq!(
        inv.command,
        r"go test -run '^TestTable$/handles_\(parens\)' ./math"
    );
}

#[test]
fn go_file_scope_unions_top_level_tests() {
    // Tighter than vim-test, which runs the whole package for file scope.
    let p = go_module();
    let file = p.root.join("math/add_test.go");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::File, &ctx(&file, GO_SRC, 0, Language::Go)).unwrap();
    assert_eq!(inv.command, "go test -run '^(TestAdd|TestTable)$' ./math");
}

#[test]
fn go_suite_runs_all_packages() {
    let p = go_module();
    let file = p.root.join("math/add_test.go");
    fs::write(&file, "").unwrap();
    let inv = build_test_command(TestScope::Suite, &ctx(&file, "", 0, Language::Go)).unwrap();
    assert_eq!(inv.command, "go test ./...");
}

// ---------------------------------------------------------------------------
// Config templates
// ---------------------------------------------------------------------------

#[test]
fn config_template_substitutes_placeholders() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("mix.exs"), "").unwrap();
    fs::create_dir_all(root.join("test")).unwrap();
    let file = root.join("test/demo_test.exs");
    fs::write(&file, "").unwrap();

    let cfg = TestConfig {
        suite_command: Some("mix test".into()),
        file_command: Some("mix test {file}".into()),
        nearest_command: Some("mix test {file}:{line}".into()),
        root_markers: vec!["mix.exs".into()],
    };
    let ctx = TestContext {
        file: &file,
        source: "",
        cursor_line: 41,
        language: None,
        config: Some(&cfg),
    };
    let inv = build_test_command(TestScope::Nearest, &ctx).unwrap();
    assert_eq!(inv.command, "mix test test/demo_test.exs:42");
    assert_eq!(inv.cwd, root);
}

#[test]
fn no_runner_yields_helpful_error() {
    let file = PathBuf::from("/tmp/nonexistent.xyz");
    let ctx = TestContext {
        file: &file,
        source: "",
        cursor_line: 0,
        language: None,
        config: None,
    };
    let err = build_test_command(TestScope::File, &ctx).unwrap_err();
    assert!(err.contains("[language.test]"));
}
