//! Tree-sitter based test discovery.
//!
//! Finds test definitions (and their enclosing namespaces) in a source file
//! by walking the syntax tree, following the neotest model rather than
//! vim-test's regex-plus-indentation scanning. AST ranges give exact nesting,
//! which eliminates the classic bug classes: mis-nested namespaces from odd
//! indentation, string literals that look like test definitions, and module
//! paths reconstructed by brace counting.

use crate::syntax::{Language, SyntaxHighlighter};
use tree_sitter::Node;

/// How a test's name behaves when building a runner filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestFlavor {
    /// Plain test — exact-match filters (`-- --exact`, `^name$`) are safe.
    Exact,
    /// Parameterized (`#[rstest]`, `it.each` printf templates, go table
    /// entries): the runtime test name gets a generated suffix, so filters
    /// must match by prefix / unanchored.
    Parameterized,
}

/// A test definition discovered in a source file.
#[derive(Debug, Clone)]
pub struct DiscoveredTest {
    /// Innermost name: fn name, test string, go func name…
    pub name: String,
    /// Enclosing namespaces, outermost first: rust `mod`s, js `describe`s,
    /// python classes, go parent test funcs / `t.Run` names.
    pub namespaces: Vec<String>,
    /// 0-indexed start line of the definition.
    pub line: usize,
    /// 0-indexed last line of the definition (inclusive).
    pub end_line: usize,
    pub flavor: TestFlavor,
}

/// Discovers all tests in `source` for the given language.
///
/// Returns an empty vec when the language has no discovery support or the
/// grammar fails to load.
pub fn discover_tests(lang: Language, source: &str) -> Vec<DiscoveredTest> {
    let mut highlighter = match SyntaxHighlighter::new(lang) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    highlighter.parse(source);
    let Some(tree) = highlighter.tree() else {
        return Vec::new();
    };
    let root = tree.root_node();

    let mut tests = Vec::new();
    match lang {
        Language::Rust => collect_rust(root, source, &mut Vec::new(), &mut tests),
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            collect_js(root, source, &mut Vec::new(), false, &mut tests)
        }
        Language::Python => collect_python(root, source, &mut Vec::new(), &mut tests),
        Language::Go => collect_go(root, source, &mut Vec::new(), false, &mut tests),
        _ => {}
    }
    tests
}

/// Picks the test nearest to `cursor_line` (0-indexed), vim-test style with
/// a twist: a test containing the cursor wins (innermost), then the closest
/// test above the cursor, then the first test below it. vim-test gives up
/// when the cursor sits above every test; falling forward instead means
/// `<Space>tn` at the top of a file still does something useful.
pub fn nearest_test(tests: &[DiscoveredTest], cursor_line: usize) -> Option<&DiscoveredTest> {
    // Innermost containing test = the one with the greatest start line.
    if let Some(containing) = tests
        .iter()
        .filter(|t| t.line <= cursor_line && cursor_line <= t.end_line)
        .max_by_key(|t| t.line)
    {
        return Some(containing);
    }
    if let Some(above) = tests
        .iter()
        .filter(|t| t.line <= cursor_line)
        .max_by_key(|t| t.line)
    {
        return Some(above);
    }
    tests.iter().min_by_key(|t| t.line)
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

/// Classifies a `#[...]` attribute: is it a test attribute, and does it
/// generate parameterized test names?
fn rust_attr_test_flavor(attr_text: &str) -> Option<TestFlavor> {
    // Attribute text looks like `#[test]`, `#[tokio::test]`,
    // `#[rstest]`, `#[test_case(1, 2)]`, possibly with inner whitespace.
    let inner = attr_text
        .trim()
        .strip_prefix("#[")?
        .trim_start_matches(char::is_whitespace);
    // Path = everything up to `(`, `]`, or whitespace.
    let path_end = inner
        .find(|c: char| c == '(' || c == ']' || c.is_whitespace())
        .unwrap_or(inner.len());
    let path = &inner[..path_end];
    let last_segment = path.rsplit("::").next().unwrap_or(path);
    match last_segment {
        "rstest" | "test_case" | "case" => Some(TestFlavor::Parameterized),
        // `test`, `tokio::test`, `async_std::test`, `actix_rt::test`, …
        "test" => Some(TestFlavor::Exact),
        _ => None,
    }
}

/// Returns the test flavor of a rust `function_item` by scanning its
/// preceding attribute siblings, or `None` if it isn't a test.
fn rust_fn_test_flavor(func: Node, source: &str) -> Option<TestFlavor> {
    let mut flavor: Option<TestFlavor> = None;
    let mut sib = func.prev_sibling();
    while let Some(node) = sib {
        match node.kind() {
            "attribute_item" => {
                if let Some(f) = rust_attr_test_flavor(node_text(node, source)) {
                    // Parameterized (rstest/test_case) wins over plain test:
                    // the generated names carry case suffixes either way.
                    flavor = match (flavor, f) {
                        (Some(TestFlavor::Parameterized), _) | (_, TestFlavor::Parameterized) => {
                            Some(TestFlavor::Parameterized)
                        }
                        _ => Some(TestFlavor::Exact),
                    };
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sib = node.prev_sibling();
    }
    flavor
}

fn collect_rust(
    node: Node,
    source: &str,
    namespaces: &mut Vec<String>,
    out: &mut Vec<DiscoveredTest>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string());
                if let Some(name) = name {
                    namespaces.push(name);
                    collect_rust(child, source, namespaces, out);
                    namespaces.pop();
                } else {
                    collect_rust(child, source, namespaces, out);
                }
            }
            "function_item" => {
                if let Some(flavor) = rust_fn_test_flavor(child, source) {
                    if let Some(name) = child.child_by_field_name("name") {
                        out.push(DiscoveredTest {
                            name: node_text(name, source).to_string(),
                            namespaces: namespaces.clone(),
                            line: child.start_position().row,
                            end_line: child.end_position().row,
                            flavor,
                        });
                    }
                }
            }
            _ => collect_rust(child, source, namespaces, out),
        }
    }
}

// ---------------------------------------------------------------------------
// JavaScript / TypeScript
// ---------------------------------------------------------------------------

/// Peels a call callee down to its leftmost identifier, noting whether the
/// chain goes through `.each` on the way:
/// `it` → (it, false), `it.only` → (it, false),
/// `it.each([...])` (call) → (it, true), `describe.each\`…\`` → (describe, true).
fn js_callee_info<'a>(node: Node, source: &'a str) -> Option<(&'a str, bool)> {
    match node.kind() {
        "identifier" => Some((node_text(node, source), false)),
        "member_expression" => {
            let is_each = node
                .child_by_field_name("property")
                .is_some_and(|p| node_text(p, source) == "each");
            let (base, inner_each) = node
                .child_by_field_name("object")
                .and_then(|obj| js_callee_info(obj, source))?;
            Some((base, is_each || inner_each))
        }
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(|f| js_callee_info(f, source)),
        _ => None,
    }
}

/// Extracts the first string-ish argument of a call. Returns the string
/// content and whether it contains `${…}` interpolation (a name that is
/// dynamic even without `.each`).
fn js_first_string_arg(call: Node, source: &str) -> Option<(String, bool)> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arg = args.named_children(&mut cursor).next()?;
    match arg.kind() {
        "string" => {
            // Concatenate string_fragment children (skips the quotes and
            // resolves nothing — escape sequences stay literal, matching
            // what jest/vitest print for simple names).
            let mut content = String::new();
            let mut c2 = arg.walk();
            for part in arg.named_children(&mut c2) {
                if part.kind() == "string_fragment" || part.kind() == "escape_sequence" {
                    content.push_str(node_text(part, source));
                }
            }
            Some((content, false))
        }
        "template_string" => {
            let raw = node_text(arg, source);
            let content = raw.trim_matches('`').to_string();
            // `${expr}` interpolations make the runtime name dynamic.
            let interpolated = content.contains("${");
            Some((content, interpolated))
        }
        _ => None,
    }
}

/// jest `test.each` printf tokens: %s %d %i %f %j %o %p %#  (%% is literal).
fn js_has_printf_token(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'%' {
            match bytes[i + 1] {
                b'%' => i += 1, // literal percent, skip both
                b's' | b'd' | b'i' | b'f' | b'j' | b'o' | b'p' | b'#' => return true,
                _ => {}
            }
        }
        i += 1;
    }
    false
}

fn collect_js(
    node: Node,
    source: &str,
    namespaces: &mut Vec<String>,
    // True when any enclosing describe was `.each`-parameterized or had an
    // interpolated name: every runtime test name under it carries generated
    // segments, so filters must truncate/unanchor (an anchored filter with a
    // literal `%s` placeholder would match zero tests).
    dynamic_ancestor: bool,
    out: &mut Vec<DiscoveredTest>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some((base, is_each)) = child
                .child_by_field_name("function")
                .and_then(|f| js_callee_info(f, source))
            {
                let is_test = matches!(base, "it" | "test");
                let is_namespace = matches!(base, "describe" | "suite" | "context");
                if is_test || is_namespace {
                    if let Some((name, interpolated)) = js_first_string_arg(child, source) {
                        // A printf token only makes the name dynamic when the
                        // call is actually `.each` — a plain it('uses %d')
                        // has a literal percent in its runtime name.
                        let dynamic = interpolated || (is_each && js_has_printf_token(&name));
                        if is_test {
                            out.push(DiscoveredTest {
                                name,
                                namespaces: namespaces.clone(),
                                line: child.start_position().row,
                                end_line: child.end_position().row,
                                flavor: if dynamic || dynamic_ancestor {
                                    TestFlavor::Parameterized
                                } else {
                                    TestFlavor::Exact
                                },
                            });
                            // Tests don't nest further; still recurse in case
                            // of unconventional nesting inside the callback.
                            collect_js(child, source, namespaces, dynamic_ancestor, out);
                            continue;
                        }
                        namespaces.push(name);
                        collect_js(child, source, namespaces, dynamic_ancestor || dynamic, out);
                        namespaces.pop();
                        continue;
                    }
                }
            }
        }
        collect_js(child, source, namespaces, dynamic_ancestor, out);
    }
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

fn collect_python(
    node: Node,
    source: &str,
    namespaces: &mut Vec<String>,
    out: &mut Vec<DiscoveredTest>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string());
                if let Some(name) = name {
                    namespaces.push(name);
                    collect_python(child, source, namespaces, out);
                    namespaces.pop();
                } else {
                    collect_python(child, source, namespaces, out);
                }
            }
            "function_definition" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let name_text = node_text(name, source);
                    if name_text.starts_with("test") {
                        out.push(DiscoveredTest {
                            name: name_text.to_string(),
                            namespaces: namespaces.clone(),
                            line: definition_start_line(child),
                            end_line: child.end_position().row,
                            flavor: TestFlavor::Exact,
                        });
                    }
                }
                collect_python(child, source, namespaces, out);
            }
            _ => collect_python(child, source, namespaces, out),
        }
    }
}

/// A decorated python test's logical start is its outermost decorator, so a
/// cursor on `@pytest.mark.parametrize` still selects the test below it.
fn definition_start_line(func: Node) -> usize {
    let mut start = func.start_position().row;
    if let Some(parent) = func.parent() {
        if parent.kind() == "decorated_definition" {
            start = parent.start_position().row;
        }
    }
    start
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

fn go_string_content(node: Node, source: &str) -> String {
    node_text(node, source)
        .trim_matches('"')
        .trim_matches('`')
        .to_string()
}

fn collect_go(
    node: Node,
    source: &str,
    namespaces: &mut Vec<String>,
    // True inside a test function that calls `t.Run` with a non-literal
    // name (table-driven usage). Only then do `name: "..."` keyed elements
    // become subtest entries — otherwise an unrelated struct literal like
    // `Person{name: "Alice"}` would produce a phantom test.
    table_mode: bool,
    out: &mut Vec<DiscoveredTest>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string());
                let is_test = name
                    .as_deref()
                    .is_some_and(|n| n.starts_with("Test") || n.starts_with("Example"));
                if let Some(name) = name.filter(|_| is_test) {
                    out.push(DiscoveredTest {
                        name: name.clone(),
                        namespaces: namespaces.clone(),
                        line: child.start_position().row,
                        end_line: child.end_position().row,
                        flavor: TestFlavor::Exact,
                    });
                    let fn_table_mode = go_has_dynamic_t_run(child, source);
                    namespaces.push(name);
                    collect_go(child, source, namespaces, fn_table_mode, out);
                    namespaces.pop();
                } else {
                    collect_go(child, source, namespaces, false, out);
                }
            }
            "call_expression" => {
                // t.Run("subtest name", func(t *testing.T) { ... })
                let subtest_name = go_t_run_name(child, source);
                if let Some(name) = subtest_name {
                    // Only meaningful inside a test function.
                    if !namespaces.is_empty() {
                        out.push(DiscoveredTest {
                            name: name.clone(),
                            namespaces: namespaces.clone(),
                            line: child.start_position().row,
                            end_line: child.end_position().row,
                            flavor: TestFlavor::Exact,
                        });
                        namespaces.push(name);
                        collect_go(child, source, namespaces, table_mode, out);
                        namespaces.pop();
                        continue;
                    }
                }
                collect_go(child, source, namespaces, table_mode, out);
            }
            "keyed_element" => {
                // Table-driven test entry: `{name: "adds two numbers", ...}`.
                // The generated subtest name comes from t.Run(tc.name, …), so
                // treat the entry as a parameterized leaf under the current
                // test function.
                if table_mode && !namespaces.is_empty() {
                    if let Some(name) = go_table_entry_name(child, source) {
                        out.push(DiscoveredTest {
                            name,
                            namespaces: namespaces.clone(),
                            line: child.start_position().row,
                            end_line: child.end_position().row,
                            flavor: TestFlavor::Parameterized,
                        });
                    }
                }
                collect_go(child, source, namespaces, table_mode, out);
            }
            _ => collect_go(child, source, namespaces, table_mode, out),
        }
    }
}

/// True if the function contains a `t.Run(<expr>, …)` call whose first
/// argument is not a string literal — the signature of table-driven tests
/// (`t.Run(tc.name, …)`).
fn go_has_dynamic_t_run(func: Node, source: &str) -> bool {
    let mut cursor = func.walk();
    let mut stack = vec![func];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" && go_is_t_run(node, source) {
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut c = args.walk();
                if let Some(first) = args.named_children(&mut c).next() {
                    if first.kind() != "interpreted_string_literal"
                        && first.kind() != "raw_string_literal"
                    {
                        return true;
                    }
                }
            }
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

/// Is this call `t.Run(...)`?
fn go_is_t_run(call: Node, source: &str) -> bool {
    let Some(func) = call.child_by_field_name("function") else {
        return false;
    };
    if func.kind() != "selector_expression" {
        return false;
    }
    let (Some(operand), Some(field)) = (
        func.child_by_field_name("operand"),
        func.child_by_field_name("field"),
    ) else {
        return false;
    };
    operand.kind() == "identifier"
        && node_text(operand, source) == "t"
        && node_text(field, source) == "Run"
}

/// Matches `t.Run("name", …)` and returns the subtest name.
fn go_t_run_name(call: Node, source: &str) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    if func.kind() != "selector_expression" {
        return None;
    }
    let operand = func.child_by_field_name("operand")?;
    let field = func.child_by_field_name("field")?;
    if operand.kind() != "identifier" || node_text(field, source) != "Run" {
        return None;
    }
    // vim-test only matches `t.Run`; accept the conventional receiver only,
    // to avoid claiming unrelated `.Run(...)` calls.
    if node_text(operand, source) != "t" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let first = args.named_children(&mut cursor).next()?;
    if first.kind() == "interpreted_string_literal" || first.kind() == "raw_string_literal" {
        Some(go_string_content(first, source))
    } else {
        None
    }
}

/// Matches a `name: "..."` keyed element in a composite literal.
fn go_table_entry_name(keyed: Node, source: &str) -> Option<String> {
    let mut cursor = keyed.walk();
    let mut children = keyed.named_children(&mut cursor);
    let key = children.next()?;
    let value = children.next()?;
    let key_text = node_text(key, source);
    if key_text != "name" {
        return None;
    }
    let value_text = node_text(value, source);
    if value_text.starts_with('"') || value_text.starts_with('`') {
        Some(value_text.trim_matches('"').trim_matches('`').to_string())
    } else {
        None
    }
}
