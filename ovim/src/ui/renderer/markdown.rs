//! Markdown parser for hover window rendering
//!
//! Parses LSP hover markdown and converts it to styled text spans for ratatui.
//! Supports: **bold**, `inline code`, ```code blocks```, and basic structure.

use crate::syntax::{HighlightGroup, LanguageRegistry, SyntaxHighlighter, Theme};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;

/// Colors for markdown rendering (Catppuccin-inspired)
pub mod colors {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(30, 30, 46);
    pub const TEXT: Color = Color::Rgb(205, 214, 244);
    pub const BORDER: Color = Color::Rgb(137, 180, 250);
    pub const BOLD: Color = Color::Rgb(245, 194, 231);
    pub const CODE_SPAN_BG: Color = Color::Rgb(49, 50, 68);
    pub const CODE_SPAN_FG: Color = Color::Rgb(148, 226, 213);
    pub const CODE_BLOCK_BG: Color = Color::Rgb(24, 24, 37);
    pub const CODE_BLOCK_FG: Color = Color::Rgb(166, 227, 161);
    pub const HEADING: Color = Color::Rgb(245, 194, 231);
    pub const PARAM: Color = Color::Rgb(250, 179, 135);
    pub const RETURN: Color = Color::Rgb(166, 227, 161);
}

/// Parsed markdown element
#[derive(Debug, Clone)]
pub enum MarkdownElement {
    /// Plain text
    Text(String),
    /// Bold text (**text**)
    Bold(String),
    /// Inline code (`code`)
    InlineCode(String),
    /// Code block with optional language
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Display math delimited by `$$...$$` or `\[...\]`.
    DisplayMath(String),
    /// Heading (# Title)
    Heading(#[allow(dead_code)] u8, String),
    /// Horizontal rule (---)
    HorizontalRule,
    /// Line break
    LineBreak,
}

/// Parse markdown text into elements
pub fn parse_markdown(text: &str) -> Vec<MarkdownElement> {
    let mut elements = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang: Option<String> = None;
    let mut code_block_content = String::new();
    let mut math_block: Option<(&'static str, &'static str, String)> = None;

    for line in text.lines() {
        // Handle code blocks
        if math_block.is_none() && line.starts_with("```") {
            if in_code_block {
                // End of code block
                elements.push(MarkdownElement::CodeBlock {
                    language: code_block_lang.take(),
                    code: code_block_content.trim_end().to_string(),
                });
                code_block_content.clear();
                in_code_block = false;
            } else {
                // Start of code block
                in_code_block = true;
                let lang = line.trim_start_matches('`').trim();
                code_block_lang = if lang.is_empty() {
                    None
                } else {
                    Some(lang.to_string())
                };
            }

            continue;
        }

        if in_code_block {
            if !code_block_content.is_empty() {
                code_block_content.push('\n');
            }
            code_block_content.push_str(line);
            continue;
        }

        if let Some((opener, closer, mut content)) = math_block.take() {
            if line.trim() == closer {
                elements.push(MarkdownElement::DisplayMath(content.trim().to_string()));
            } else {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(line);
                math_block = Some((opener, closer, content));
            }
            continue;
        }

        let trimmed = line.trim();
        let delimiter = if trimmed.starts_with("$$") {
            Some(("$$", "$$"))
        } else if trimmed.starts_with("\\[") {
            Some(("\\[", "\\]"))
        } else {
            None
        };
        if let Some((opener, closer)) = delimiter {
            let remainder = trimmed.strip_prefix(opener).unwrap_or_default();
            if let Some(math) = remainder.strip_suffix(closer) {
                if !math.trim().is_empty() {
                    elements.push(MarkdownElement::DisplayMath(math.trim().to_string()));
                }
            } else {
                math_block = Some((opener, closer, remainder.to_string()));
            }
            continue;
        }

        // Handle headings
        if line.starts_with('#') {
            let level = line.chars().take_while(|c| *c == '#').count() as u8;
            let text = line.trim_start_matches('#').trim();
            elements.push(MarkdownElement::Heading(level, text.to_string()));
            continue;
        }

        // Handle horizontal rules
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            elements.push(MarkdownElement::HorizontalRule);
            continue;
        }

        // Handle empty lines
        if line.trim().is_empty() {
            elements.push(MarkdownElement::LineBreak);
            continue;
        }

        // Parse inline elements
        parse_inline_elements(line, &mut elements);
        elements.push(MarkdownElement::LineBreak);
    }

    // Handle unclosed code block
    if in_code_block && !code_block_content.is_empty() {
        elements.push(MarkdownElement::CodeBlock {
            language: code_block_lang,
            code: code_block_content,
        });
    }
    if let Some((opener, _, content)) = math_block {
        elements.push(MarkdownElement::Text(opener.to_string()));
        elements.push(MarkdownElement::LineBreak);
        for line in content.lines() {
            elements.push(MarkdownElement::Text(line.to_string()));
            elements.push(MarkdownElement::LineBreak);
        }
    }

    elements
}

/// Parse inline markdown elements (bold, inline code) from a line
fn parse_inline_elements(line: &str, elements: &mut Vec<MarkdownElement>) {
    let mut current_text = String::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                // Bold text
                chars.next(); // consume second *
                if !current_text.is_empty() {
                    elements.push(MarkdownElement::Text(current_text.clone()));
                    current_text.clear();
                }
                let mut bold_text = String::new();
                let mut closed = false;
                while let Some(bc) = chars.next() {
                    if bc == '*' && chars.peek() == Some(&'*') {
                        chars.next();
                        closed = true;
                        break;
                    }
                    bold_text.push(bc);
                }
                if closed && !bold_text.is_empty() {
                    push_bold_elements(&bold_text, elements);
                } else if !closed {
                    elements.push(MarkdownElement::Text(format!("**{bold_text}")));
                }
            }
            '`' => {
                // Inline code
                if !current_text.is_empty() {
                    elements.push(MarkdownElement::Text(current_text.clone()));
                    current_text.clear();
                }
                let mut code_text = String::new();
                let mut closed = false;
                for cc in chars.by_ref() {
                    if cc == '`' {
                        closed = true;
                        break;
                    }
                    code_text.push(cc);
                }
                if closed && !code_text.is_empty() {
                    elements.push(MarkdownElement::InlineCode(code_text));
                } else if !closed {
                    elements.push(MarkdownElement::Text(format!("`{code_text}")));
                }
            }
            _ => {
                current_text.push(c);
            }
        }
    }

    if !current_text.is_empty() {
        elements.push(MarkdownElement::Text(current_text));
    }
}

/// Preserve inline-code semantics inside a bold run instead of displaying
/// its backticks literally. The surrounding pieces remain bold; the code span
/// uses the stronger code treatment, matching ordinary inline code.
fn push_bold_elements(text: &str, elements: &mut Vec<MarkdownElement>) {
    for (index, part) in text.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        if index % 2 == 0 {
            elements.push(MarkdownElement::Bold(part.to_string()));
        } else {
            elements.push(MarkdownElement::InlineCode(part.to_string()));
        }
    }
}

/// Highlights a code block using tree-sitter syntax highlighting
/// Returns None if language is unknown or highlighting fails
type LineHighlights = Vec<Vec<(Range<usize>, HighlightGroup)>>;

fn highlight_code_block(language: &str, code: &str) -> Option<LineHighlights> {
    let lang = LanguageRegistry::from_info_string(language)?;
    let mut highlighter = SyntaxHighlighter::new(lang).ok()?;
    highlighter.parse(code);
    Some(highlighter.highlights_for_all_lines(code))
}

/// Renders a single code line with syntax highlights
fn render_code_line_with_highlights(
    line: &str,
    highlights: &[(Range<usize>, HighlightGroup)],
    theme: &Theme,
    max_width: usize,
) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        " ",
        Style::default().bg(colors::CODE_BLOCK_BG),
    )); // Leading padding

    let chars: Vec<char> = line.chars().collect();
    let display_width = max_width.saturating_sub(2);

    let mut col = 0;
    while col < chars.len() && col < display_width {
        // Find highlight group for current position
        let group = highlights
            .iter()
            .find(|(range, _)| range.contains(&col))
            .map(|(_, g)| *g);

        // Find consecutive chars with same highlight
        let mut end_col = col + 1;
        while end_col < chars.len() && end_col < display_width {
            let next_group = highlights
                .iter()
                .find(|(range, _)| range.contains(&end_col))
                .map(|(_, g)| *g);
            if next_group != group {
                break;
            }
            end_col += 1;
        }

        // Build styled span
        let text: String = chars[col..end_col].iter().collect();
        let style = if let Some(g) = group {
            Style::default()
                .fg(crate::key_convert::convert_core_color(theme.get_color(g)))
                .bg(colors::CODE_BLOCK_BG)
        } else {
            Style::default()
                .fg(colors::CODE_BLOCK_FG)
                .bg(colors::CODE_BLOCK_BG)
        };
        spans.push(Span::styled(text, style));
        col = end_col;
    }

    if chars.len() > display_width {
        spans.push(Span::styled(
            "...",
            Style::default()
                .fg(colors::CODE_BLOCK_FG)
                .bg(colors::CODE_BLOCK_BG),
        ));
    }
    spans.push(Span::styled(
        " ",
        Style::default().bg(colors::CODE_BLOCK_BG),
    )); // Trailing padding

    Line::from(spans)
}

/// Convert parsed markdown elements to styled ratatui Lines
pub fn render_markdown(
    elements: &[MarkdownElement],
    max_width: usize,
    theme: Option<&Theme>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    let text_style = Style::default().fg(colors::TEXT);
    let bold_style = Style::default()
        .fg(colors::BOLD)
        .add_modifier(Modifier::BOLD);
    let code_style = Style::default()
        .fg(colors::CODE_SPAN_FG)
        .bg(colors::CODE_SPAN_BG);
    let heading_style = Style::default()
        .fg(colors::HEADING)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let code_block_style = Style::default()
        .fg(colors::CODE_BLOCK_FG)
        .bg(colors::CODE_BLOCK_BG);

    for element in elements {
        match element {
            MarkdownElement::Text(text) => {
                // Check for @param and @return annotations
                let styled_text = if text.contains("@param") || text.starts_with("@param") {
                    Span::styled(text.clone(), Style::default().fg(colors::PARAM))
                } else if text.contains("@return") || text.starts_with("@return") {
                    Span::styled(text.clone(), Style::default().fg(colors::RETURN))
                } else {
                    Span::styled(text.clone(), text_style)
                };
                current_spans.push(styled_text);
            }
            MarkdownElement::Bold(text) => {
                current_spans.push(Span::styled(text.clone(), bold_style));
            }
            MarkdownElement::InlineCode(code) => {
                current_spans.push(Span::styled(code.clone(), code_style));
            }
            MarkdownElement::CodeBlock { language, code } => {
                // Flush current line
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                }

                // Try to get syntax highlights if we have a language and theme
                let highlights = language
                    .as_ref()
                    .and_then(|lang| highlight_code_block(lang, code));

                // Add code block lines
                for (line_idx, code_line) in code.lines().enumerate() {
                    // Try to render with syntax highlighting
                    if let (Some(hl), Some(theme)) = (&highlights, theme) {
                        if let Some(line_hl) = hl.get(line_idx) {
                            lines.push(render_code_line_with_highlights(
                                code_line, line_hl, theme, max_width,
                            ));
                            continue;
                        }
                    }

                    // Fallback: plain green style
                    let available = max_width.saturating_sub(2);
                    let truncated = if code_line.chars().count() > available {
                        let prefix: String = code_line
                            .chars()
                            .take(max_width.saturating_sub(5))
                            .collect();
                        format!(" {prefix}... ")
                    } else {
                        format!(" {} ", code_line)
                    };
                    lines.push(Line::from(Span::styled(truncated, code_block_style)));
                }
            }
            MarkdownElement::DisplayMath(math) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                }
                lines.push(Line::from(Span::styled("\\[", text_style)));
                for math_line in math.lines() {
                    lines.push(Line::from(Span::styled(math_line.to_string(), text_style)));
                }
                lines.push(Line::from(Span::styled("\\]", text_style)));
            }
            MarkdownElement::Heading(_, text) => {
                // Flush current line
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                }
                lines.push(Line::from(Span::styled(text.clone(), heading_style)));
            }
            MarkdownElement::HorizontalRule => {
                // Flush current line
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                }
                lines.push(Line::from(Span::styled(
                    "─".repeat(max_width.saturating_sub(2)),
                    Style::default().fg(colors::BORDER),
                )));
            }
            MarkdownElement::LineBreak => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                } else {
                    lines.push(Line::from("")); // Empty line
                }
            }
        }
    }

    // Flush remaining spans
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bold() {
        let elements = parse_markdown("Hello **world**!");
        assert!(elements
            .iter()
            .any(|e| matches!(e, MarkdownElement::Bold(s) if s == "world")));
    }

    #[test]
    fn test_parse_inline_code() {
        let elements = parse_markdown("Use `println!` for output");
        assert!(elements
            .iter()
            .any(|e| matches!(e, MarkdownElement::InlineCode(s) if s == "println!")));
    }

    #[test]
    fn test_parse_inline_code_nested_in_bold() {
        let elements = parse_markdown("**Use `k` while history is focused.**");
        assert!(elements
            .iter()
            .any(|element| matches!(element, MarkdownElement::Bold(text) if text == "Use ")));
        assert!(elements
            .iter()
            .any(|element| matches!(element, MarkdownElement::InlineCode(text) if text == "k")));
        assert!(elements.iter().any(|element| {
            matches!(element, MarkdownElement::Bold(text) if text == " while history is focused.")
        }));
    }

    #[test]
    fn inline_code_does_not_add_padding_for_concealed_backticks() {
        let elements = parse_markdown("Use `code` now");
        let lines = render_markdown(&elements, 80, None);
        let rendered = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "Use code now");
        let code = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "code")
            .expect("inline code span");
        assert_eq!(code.style.bg, Some(colors::CODE_SPAN_BG));
    }

    #[test]
    fn unmatched_inline_delimiters_remain_visible() {
        let elements = parse_markdown("Keep `this and **that");
        assert!(elements.iter().any(
            |element| matches!(element, MarkdownElement::Text(text) if text == "`this and **that")
        ));
    }

    #[test]
    fn test_parse_code_block() {
        let elements = parse_markdown("```rust\nfn main() {}\n```");
        assert!(elements.iter().any(|e| matches!(e,
            MarkdownElement::CodeBlock { language: Some(lang), code }
            if lang == "rust" && code.contains("fn main")
        )));
    }

    #[test]
    fn bold_style_boundary_does_not_split_a_logical_line() {
        let elements = parse_markdown(
            "So C420 **does not produce a contradiction and does not prove existence**. It merely narrows the range.",
        );
        let lines = render_markdown(&elements, 30, None);

        assert_eq!(lines.len(), 1);
        let rendered = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(
            rendered,
            "So C420 does not produce a contradiction and does not prove existence. It merely narrows the range."
        );
    }

    #[test]
    fn parses_multiline_bracket_display_math() {
        let elements = parse_markdown("Before\n\\[\nF(R) \\le \\theta F(2R)\n\\]\nAfter");
        assert!(elements.iter().any(|element| {
            matches!(element, MarkdownElement::DisplayMath(math) if math == "F(R) \\le \\theta F(2R)")
        }));
    }

    #[test]
    fn parses_single_line_dollar_display_math() {
        let elements = parse_markdown("$$x^2 + y^2 = z^2$$");
        assert!(matches!(
            elements.as_slice(),
            [MarkdownElement::DisplayMath(math)] if math == "x^2 + y^2 = z^2"
        ));
    }

    #[test]
    fn math_delimiters_inside_code_blocks_are_not_parsed() {
        let elements = parse_markdown("```latex\n$$x^2$$\n```");
        assert!(elements
            .iter()
            .all(|element| !matches!(element, MarkdownElement::DisplayMath(_))));
    }

    #[test]
    fn test_highlight_code_block_rust() {
        // Should successfully highlight Rust code
        let highlights = highlight_code_block("rust", "let x = 42;");
        assert!(highlights.is_some());
        let hl = highlights.unwrap();
        assert_eq!(hl.len(), 1); // One line
        assert!(!hl[0].is_empty()); // Has some highlights
    }

    #[test]
    fn test_highlight_code_block_unknown_language() {
        // Should return None for unknown language
        let highlights = highlight_code_block("unknownlang12345", "some code");
        assert!(highlights.is_none());
    }

    #[test]
    fn test_render_markdown_with_theme() {
        let elements = parse_markdown("```rust\nlet x = 42;\n```");
        let theme = crate::syntax::Theme::default();
        let lines = render_markdown(&elements, 80, Some(&theme));
        // Should have rendered the code block with syntax highlighting
        assert!(!lines.is_empty());
        // The line should have multiple spans (syntax-highlighted segments)
        assert!(lines[0].spans.len() > 1);
    }

    #[test]
    fn test_render_markdown_without_theme_falls_back() {
        let elements = parse_markdown("```rust\nlet x = 42;\n```");
        let lines = render_markdown(&elements, 80, None);
        // Should still render the code block, just without syntax colors
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_narrow_unicode_code_block_does_not_panic() {
        let elements = parse_markdown("```text\nlet greeting = \"hei 👋 verden\";\n```");
        let lines = render_markdown(&elements, 12, None);
        assert!(!lines.is_empty());
        let rendered = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("..."));
    }
}
