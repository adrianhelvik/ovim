//! Modeline parsing for per-file editor settings
//!
//! Supports vim-style modelines like:
//! ```text
//! // vim: set tabstop=4 shiftwidth=4 expandtab:
//! # vim: ts=4 sw=4 et:
//! /* vim: tw=80 */
//! ```
//!
//! Searches the first and last 5 lines of a file for modeline directives.

use std::collections::HashMap;

/// Maximum number of lines to search at start/end of file
const MODELINE_SEARCH_LINES: usize = 5;

/// Parsed modeline options
#[derive(Debug, Clone, Default)]
pub struct Modeline {
    /// Raw options parsed from the modeline (key -> value, empty string for boolean flags)
    pub options: HashMap<String, String>,
}

impl Modeline {
    /// Parse modelines from file content
    /// Searches first and last 5 lines for vim: directives
    pub fn parse(content: &str) -> Option<Self> {
        let lines: Vec<&str> = content.lines().collect();
        let mut options = HashMap::new();

        // Search first N lines
        for line in lines.iter().take(MODELINE_SEARCH_LINES) {
            if let Some(opts) = extract_modeline_options(line) {
                for (k, v) in opts {
                    options.insert(k, v);
                }
            }
        }

        // Search last N lines (if file is long enough)
        if lines.len() > MODELINE_SEARCH_LINES {
            let start = lines.len().saturating_sub(MODELINE_SEARCH_LINES);
            for line in lines.iter().skip(start) {
                if let Some(opts) = extract_modeline_options(line) {
                    for (k, v) in opts {
                        options.insert(k, v);
                    }
                }
            }
        }

        if options.is_empty() {
            None
        } else {
            Some(Modeline { options })
        }
    }

    /// Get an option value, checking both long and short names
    pub fn get(&self, long_name: &str, short_name: &str) -> Option<&String> {
        self.options
            .get(long_name)
            .or_else(|| self.options.get(short_name))
    }

    /// Check if a boolean option is set (handles "option" and "nooption")
    pub fn get_bool(&self, long_name: &str, short_name: &str) -> Option<bool> {
        // Check for positive form
        if self.options.contains_key(long_name) || self.options.contains_key(short_name) {
            return Some(true);
        }

        // Check for negative form (nooption)
        let no_long = format!("no{}", long_name);
        let no_short = format!("no{}", short_name);
        if self.options.contains_key(&no_long) || self.options.contains_key(&no_short) {
            return Some(false);
        }

        None
    }

    /// Get an integer option value
    pub fn get_int(&self, long_name: &str, short_name: &str) -> Option<usize> {
        self.get(long_name, short_name).and_then(|v| v.parse().ok())
    }

    /// Get a signed integer option value.
    pub fn get_signed_int(&self, long_name: &str, short_name: &str) -> Option<isize> {
        self.get(long_name, short_name).and_then(|v| v.parse().ok())
    }
}

/// Extract modeline options from a single line
/// Returns None if no modeline found, Some(HashMap) with parsed options otherwise
fn extract_modeline_options(line: &str) -> Option<HashMap<String, String>> {
    // Find vim:, vi:, or ex: pattern
    let modeline_start = find_modeline_start(line)?;

    // Extract the options part (everything after "vim:" until end or closing delimiter)
    let options_str = extract_options_string(&line[modeline_start..]);

    // Parse individual options
    Some(parse_options(&options_str))
}

/// Find the start position of a modeline directive in a line
/// Returns the byte offset after "vim:", "vi:", or "ex:"
fn find_modeline_start(line: &str) -> Option<usize> {
    // Look for vim:, vi:, or ex: (case insensitive for the prefix)
    let patterns = ["vim:", "vi:", "ex:", "Vim:", "VIM:"];

    for pattern in patterns {
        if let Some(pos) = line.find(pattern) {
            // Make sure it's not part of a larger word (check char before)
            let valid_start = if pos == 0 {
                true
            } else {
                let prev_char = line[..pos].chars().last().unwrap_or(' ');
                !prev_char.is_alphanumeric() && prev_char != '_'
            };

            if valid_start {
                return Some(pos + pattern.len());
            }
        }
    }

    None
}

/// Extract the options string from the modeline
/// Handles various formats: "set opt1 opt2:", "opt1 opt2", etc.
fn extract_options_string(s: &str) -> String {
    let s = s.trim();

    // Skip optional "set" keyword
    let s = if s.starts_with("set ") || s.starts_with("se ") {
        s[s.find(' ').unwrap_or(0)..].trim_start()
    } else {
        s
    };

    // Find the end delimiter (: or end of meaningful content)
    // Handle comment closers like */ or -->
    let end_markers = [":", "*/", "-->", "]]", "=#"];
    let mut end_pos = s.len();

    for marker in end_markers {
        if let Some(pos) = s.find(marker) {
            if pos < end_pos {
                end_pos = pos;
            }
        }
    }

    s[..end_pos].trim().to_string()
}

/// Parse individual options from an options string
/// Handles: "tabstop=4", "expandtab", "noexpandtab", "ts=4 sw=4"
fn parse_options(s: &str) -> HashMap<String, String> {
    let mut options = HashMap::new();

    // Split by whitespace or commas
    for part in s.split(|c: char| c.is_whitespace() || c == ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if let Some(eq_pos) = part.find('=') {
            // key=value format
            let key = part[..eq_pos].trim().to_string();
            let value = part[eq_pos + 1..].trim().to_string();
            if !key.is_empty() {
                options.insert(key, value);
            }
        } else {
            // Boolean flag (option or nooption)
            options.insert(part.to_string(), String::new());
        }
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_c_style_modeline() {
        let content = "// vim: set tabstop=4 shiftwidth=4 expandtab:\nfn main() {}";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_int("tabstop", "ts"), Some(4));
        assert_eq!(modeline.get_int("shiftwidth", "sw"), Some(4));
        assert_eq!(modeline.get_bool("expandtab", "et"), Some(true));
    }

    #[test]
    fn test_parse_short_names() {
        let content = "# vim: ts=2 sw=2 sts=-1 et noci:\n";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_int("tabstop", "ts"), Some(2));
        assert_eq!(modeline.get_int("shiftwidth", "sw"), Some(2));
        assert_eq!(modeline.get_bool("expandtab", "et"), Some(true));
        assert_eq!(modeline.get_signed_int("softtabstop", "sts"), Some(-1));
        assert_eq!(modeline.get_bool("copyindent", "ci"), Some(false));
    }

    #[test]
    fn test_parse_negated_option() {
        let content = "# vim: noexpandtab ts=8:\n";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_bool("expandtab", "et"), Some(false));
        assert_eq!(modeline.get_int("tabstop", "ts"), Some(8));
    }

    #[test]
    fn test_parse_multiline_comment() {
        let content = "/* vim: set tw=80 */\ncode here";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_int("textwidth", "tw"), Some(80));
    }

    #[test]
    fn test_parse_python_style() {
        let content = "#!/usr/bin/env python\n# vim: ts=4 sw=4 et:\n";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_int("tabstop", "ts"), Some(4));
    }

    #[test]
    fn test_no_modeline() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        assert!(Modeline::parse(content).is_none());
    }

    #[test]
    fn test_modeline_at_end() {
        let mut content = String::new();
        for i in 0..20 {
            content.push_str(&format!("line {}\n", i));
        }
        content.push_str("// vim: ts=2:\n");

        let modeline = Modeline::parse(&content).unwrap();
        assert_eq!(modeline.get_int("tabstop", "ts"), Some(2));
    }

    #[test]
    fn test_vim_in_word_ignored() {
        // "vim:" inside a word should be ignored
        let content = "// This is neovim: not a modeline\n";
        assert!(Modeline::parse(content).is_none());
    }

    #[test]
    fn test_without_set_keyword() {
        let content = "// vim: ts=4 sw=4\n";
        let modeline = Modeline::parse(content).unwrap();

        assert_eq!(modeline.get_int("tabstop", "ts"), Some(4));
        assert_eq!(modeline.get_int("shiftwidth", "sw"), Some(4));
    }
}
