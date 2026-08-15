//! EditorConfig discovery and indentation resolution.
//!
//! Configuration files are collected from the edited file's directory toward
//! the filesystem root, stopping at `root = true`. Matching sections are then
//! applied from the outermost file inward and in source order within each file.
//! This gives nearby and later sections the standard EditorConfig precedence.

use crate::indentation::IndentOptions;
use globset::GlobBuilder;
use std::collections::HashMap;
use std::path::Path;

const SUPPORTED_PROPERTIES: [&str; 3] = ["indent_style", "indent_size", "tab_width"];

#[derive(Debug, Default)]
struct ConfigFile {
    root: bool,
    sections: Vec<Section>,
}

#[derive(Debug)]
struct Section {
    pattern: String,
    properties: Vec<(String, String)>,
}

/// Resolve indentation for `file_path`, using `defaults` when no matching
/// EditorConfig property exists. Malformed or unreadable configuration is
/// ignored locally so opening a file can never be blocked by style metadata.
pub fn resolve_indent_options(file_path: &Path, defaults: IndentOptions) -> IndentOptions {
    let Some(file_name) = file_path.file_name() else {
        return defaults.normalized();
    };
    let Some(mut directory) = file_path.parent().map(Path::to_path_buf) else {
        return defaults.normalized();
    };

    let mut configs = Vec::new();
    loop {
        let config_path = directory.join(".editorconfig");
        if let Ok(source) = std::fs::read_to_string(&config_path) {
            let config = parse(&source);
            let is_root = config.root;
            configs.push((directory.clone(), config));
            if is_root {
                break;
            }
        }

        let Some(parent) = directory.parent() else {
            break;
        };
        if parent == directory {
            break;
        }
        directory = parent.to_path_buf();
    }

    configs.reverse();
    let mut effective = HashMap::<String, String>::new();
    for (config_dir, config) in configs {
        let relative = file_path
            .strip_prefix(&config_dir)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");
        let basename = file_name.to_string_lossy();

        for section in config.sections {
            if !pattern_matches(&section.pattern, &relative, &basename) {
                continue;
            }
            for (key, value) in section.properties {
                if value.eq_ignore_ascii_case("unset") {
                    effective.remove(&key);
                } else {
                    effective.insert(key, value);
                }
            }
        }
    }

    apply_properties(defaults, &effective)
}

fn parse(source: &str) -> ConfigFile {
    let mut config = ConfigFile::default();
    let mut current_section: Option<usize> = None;

    for raw_line in source.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(pattern) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            config.sections.push(Section {
                pattern: pattern.trim().to_string(),
                properties: Vec::new(),
            });
            current_section = Some(config.sections.len() - 1);
            continue;
        }

        let Some((key, value)) = split_property(line) else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if let Some(index) = current_section {
            if SUPPORTED_PROPERTIES.contains(&key.as_str()) {
                config.sections[index].properties.push((key, value));
            }
        } else if key == "root" {
            config.root = value.eq_ignore_ascii_case("true");
        }
    }

    config
}

fn pattern_matches(pattern: &str, relative: &str, basename: &str) -> bool {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    let candidate = if pattern.contains('/') {
        relative
    } else {
        basename
    };

    let pattern = expand_numeric_ranges(pattern);
    GlobBuilder::new(&pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map(|glob| glob.compile_matcher().is_match(candidate))
        .unwrap_or(false)
}

fn split_property(line: &str) -> Option<(&str, &str)> {
    let separator = match (line.find('='), line.find(':')) {
        (Some(equals), Some(colon)) => equals.min(colon),
        (Some(equals), None) => equals,
        (None, Some(colon)) => colon,
        (None, None) => return None,
    };
    Some((&line[..separator], &line[separator + 1..]))
}

/// `globset` handles EditorConfig's string alternatives (`{rs,py}`) but not
/// numeric ranges. Translate bounded `{1..3}` forms into ordinary alternatives.
fn expand_numeric_ranges(pattern: &str) -> String {
    let mut expanded = pattern.to_string();
    let mut search_from = 0;

    while let Some(relative_open) = expanded[search_from..].find('{') {
        let open = search_from + relative_open;
        let Some(relative_close) = expanded[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + relative_close;
        let contents = &expanded[open + 1..close];
        let Some((start, end)) = contents.split_once("..") else {
            search_from = close + 1;
            continue;
        };
        let (Ok(start), Ok(end)) = (start.parse::<i64>(), end.parse::<i64>()) else {
            search_from = close + 1;
            continue;
        };
        if start.abs_diff(end) > 1_000 {
            search_from = close + 1;
            continue;
        }

        let step = if start <= end { 1 } else { -1 };
        let mut values = Vec::new();
        let mut value = start;
        loop {
            values.push(value.to_string());
            if value == end {
                break;
            }
            value += step;
        }
        let replacement = format!("{{{}}}", values.join(","));
        expanded.replace_range(open..=close, &replacement);
        search_from = open + replacement.len();
    }

    expanded
}

fn apply_properties(
    defaults: IndentOptions,
    properties: &HashMap<String, String>,
) -> IndentOptions {
    let mut options = defaults.normalized();

    let indent_style = properties
        .get("indent_style")
        .map(|style| style.to_ascii_lowercase());
    if let Some(style) = indent_style.as_deref() {
        match style {
            "space" => options.expand_tab = true,
            "tab" => options.expand_tab = false,
            _ => {}
        }
    }

    let tab_width = properties
        .get("tab_width")
        .and_then(|value| parse_width(value));
    let indent_size = properties.get("indent_size");

    if let Some(width) = tab_width {
        options.tab_width = width;
    }
    if let Some(value) = indent_size {
        if value.eq_ignore_ascii_case("tab") {
            options.shift_width = options.tab_width;
            options.soft_tab_stop = -1;
        } else if let Some(width) = parse_width(value) {
            options.shift_width = width;
            options.soft_tab_stop = -1;
            if tab_width.is_none() {
                // EditorConfig defines tab_width to default to indent_size.
                options.tab_width = width;
            }
        }
    } else if indent_style.as_deref() == Some("tab") {
        options.shift_width = options.tab_width;
        options.soft_tab_stop = -1;
    }

    options.normalized()
}

fn parse_width(value: &str) -> Option<usize> {
    value
        .parse::<usize>()
        .ok()
        .filter(|width| (1..=16).contains(width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn nearest_files_and_later_sections_win() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            &dir.path().join(".editorconfig"),
            "root = true\n[*]\nindent_style = space\nindent_size = 4\n[*.rs]\nindent_size = 2\n",
        );
        write(
            &dir.path().join("src/.editorconfig"),
            "[*.rs]\nindent_style = tab\ntab_width = 8\n",
        );
        let file = dir.path().join("src/main.rs");
        write(&file, "fn main() {}\n");

        let options = resolve_indent_options(&file, IndentOptions::default());
        assert!(!options.expand_tab);
        assert_eq!(options.shift_width, 2);
        assert_eq!(options.tab_width, 8);
    }

    #[test]
    fn slashless_patterns_match_at_any_depth_and_slashes_are_relative() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            &dir.path().join(".editorconfig"),
            "root=true\n[*.rs]\nindent_size=2\n[src/*.rs]\nindent_size=3\n",
        );
        let direct = dir.path().join("src/main.rs");
        let nested = dir.path().join("deep/main.rs");
        write(&direct, "");
        write(&nested, "");

        assert_eq!(
            resolve_indent_options(&direct, IndentOptions::default()).shift_width,
            3
        );
        assert_eq!(
            resolve_indent_options(&nested, IndentOptions::default()).shift_width,
            2
        );
    }

    #[test]
    fn unset_removes_an_inherited_property() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            &dir.path().join(".editorconfig"),
            "root=true\n[*]\nindent_size=2\n[*.txt]\nindent_size=unset\n",
        );
        let file = dir.path().join("notes.txt");
        write(&file, "");

        let defaults = IndentOptions {
            shift_width: 6,
            ..IndentOptions::default()
        };
        assert_eq!(resolve_indent_options(&file, defaults).shift_width, 6);
    }

    #[test]
    fn indent_size_sets_implicit_tab_width_and_tab_uses_explicit_width() {
        let mut properties = HashMap::from([("indent_size".to_string(), "2".to_string())]);
        let options = apply_properties(IndentOptions::default(), &properties);
        assert_eq!(options.shift_width, 2);
        assert_eq!(options.tab_width, 2);

        properties.insert("indent_size".to_string(), "tab".to_string());
        properties.insert("tab_width".to_string(), "8".to_string());
        let options = apply_properties(IndentOptions::default(), &properties);
        assert_eq!(options.shift_width, 8);
        assert_eq!(options.tab_width, 8);

        let properties = HashMap::from([
            ("indent_style".to_string(), "tab".to_string()),
            ("tab_width".to_string(), "6".to_string()),
        ]);
        let options = apply_properties(IndentOptions::default(), &properties);
        assert_eq!(options.shift_width, 6);
        assert_eq!(options.tab_width, 6);
    }

    #[test]
    fn supports_braces_numeric_ranges_colon_properties_and_utf8_bom() {
        assert!(pattern_matches("*.{rs,py}", "src/main.rs", "main.rs"));
        assert!(pattern_matches("file{1..3}.txt", "file2.txt", "file2.txt"));
        assert!(pattern_matches("file{3..1}.txt", "file2.txt", "file2.txt"));

        let config = parse("\u{feff}root: true\n[*.rs]\nindent_size: 2\n");
        assert!(config.root);
        assert_eq!(
            config.sections[0].properties,
            vec![("indent_size".to_string(), "2".to_string())]
        );
    }
}
