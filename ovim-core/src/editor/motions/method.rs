//! Method/function navigation motions: ]m, [m, ]M, [M

use super::Motions;
use crate::buffer::Buffer;
use crate::unicode::GraphemeCol;

impl Motions {
    /// Method navigation: jump to next method/function start
    /// `]m` motion in Vim
    /// Looks for patterns like: fn name(, def name(, function name(, etc.
    pub fn method_forward(buffer: &mut Buffer, count: usize) {
        let total_lines = buffer.line_count();
        let mut current_line = buffer.cursor().line();

        for _ in 0..count {
            current_line += 1;
            while current_line < total_lines {
                if Self::is_method_start(buffer, current_line) {
                    break;
                }
                current_line += 1;
            }
            if current_line >= total_lines {
                current_line = total_lines.saturating_sub(1);
                break;
            }
        }

        // Position cursor at first non-whitespace (strip trailing newline so it
        // isn't counted as whitespace — buffer.line() includes the '\n')
        if let Some(line) = buffer.line_text(current_line) {
            let col = line.chars().take_while(|c| c.is_whitespace()).count();
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol(col));
        } else {
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol::ZERO);
        }
    }

    /// Method navigation: jump to previous method/function start
    /// `[m` motion in Vim
    pub fn method_backward(buffer: &mut Buffer, count: usize) {
        let mut current_line = buffer.cursor().line();

        for _ in 0..count {
            if current_line == 0 {
                break;
            }
            current_line -= 1;
            while current_line > 0 {
                if Self::is_method_start(buffer, current_line) {
                    break;
                }
                current_line -= 1;
            }
            // Check line 0
            if current_line == 0 && !Self::is_method_start(buffer, 0) {
                // No match found, stay at line 0
            }
        }

        // Position cursor at first non-whitespace (strip trailing newline so it
        // isn't counted as whitespace — buffer.line() includes the '\n')
        if let Some(line) = buffer.line_text(current_line) {
            let col = line.chars().take_while(|c| c.is_whitespace()).count();
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol(col));
        } else {
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol::ZERO);
        }
    }

    /// Method navigation: jump to next method/function end
    /// `]M` motion in Vim
    pub fn method_end_forward(buffer: &mut Buffer, count: usize) {
        let total_lines = buffer.line_count();
        let mut current_line = buffer.cursor().line();

        for _ in 0..count {
            current_line += 1;
            while current_line < total_lines {
                if Self::is_method_end(buffer, current_line) {
                    break;
                }
                current_line += 1;
            }
            if current_line >= total_lines {
                current_line = total_lines.saturating_sub(1);
                break;
            }
        }

        // Position at the closing brace.
        // Use char_to_grapheme_col because set_position expects a grapheme index,
        // and chars().position() gives a char index (not byte offset like str::find).
        if let Some(line) = buffer.line_text(current_line) {
            let char_col = line.chars().position(|c| c == '}').unwrap_or(0);
            let grapheme_col =
                crate::unicode::char_to_grapheme_col(&line, crate::unicode::CharCol(char_col));
            buffer.cursor_mut().set_position(current_line, grapheme_col);
        } else {
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol::ZERO);
        }
    }

    /// Method navigation: jump to previous method/function end
    /// `[M` motion in Vim
    pub fn method_end_backward(buffer: &mut Buffer, count: usize) {
        let mut current_line = buffer.cursor().line();

        for _ in 0..count {
            if current_line == 0 {
                break;
            }
            current_line -= 1;
            while current_line > 0 {
                if Self::is_method_end(buffer, current_line) {
                    break;
                }
                current_line -= 1;
            }
        }

        // Position at the closing brace (same conversion as method_end_forward)
        if let Some(line) = buffer.line_text(current_line) {
            let char_col = line.chars().position(|c| c == '}').unwrap_or(0);
            let grapheme_col =
                crate::unicode::char_to_grapheme_col(&line, crate::unicode::CharCol(char_col));
            buffer.cursor_mut().set_position(current_line, grapheme_col);
        } else {
            buffer
                .cursor_mut()
                .set_position(current_line, GraphemeCol::ZERO);
        }
    }

    /// Check if a line is the start of a method/function
    fn is_method_start(buffer: &Buffer, line_idx: usize) -> bool {
        if let Some(line) = buffer.line_text(line_idx) {
            let trimmed = line.trim();
            // Common function definition patterns
            // Rust: fn name(, pub fn name(, async fn name(
            // Python: def name(
            // JavaScript/TypeScript: function name(, async function name(
            // C/C++/Java: type name(, void name(, int name(, etc.

            // Check for Rust-style fn
            if trimmed.contains("fn ") && trimmed.contains('(') {
                return true;
            }
            // Check for Python-style def
            if trimmed.starts_with("def ") && trimmed.contains('(') {
                return true;
            }
            // Check for JavaScript/TypeScript function
            if trimmed.contains("function ") && trimmed.contains('(') {
                return true;
            }
            // Check for C/C++/Java/Go style - identifier followed by ( at start of line
            // Look for pattern: word word( or word( at reasonable indent level
            let indent = line.chars().take_while(|c| c.is_whitespace()).count();
            if indent <= 8
                && trimmed.contains('(')
                && !trimmed.starts_with("if ")
                && !trimmed.starts_with("for ")
                && !trimmed.starts_with("while ")
                && !trimmed.starts_with("switch ")
                && !trimmed.starts_with("match ")
                && !trimmed.starts_with("return ")
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("/*")
                && !trimmed.starts_with("*")
            {
                // Check if line ends with { or has { after )
                if trimmed.ends_with('{') || (trimmed.contains(") {") || trimmed.contains("){")) {
                    return true;
                }
                // Check if next line starts with {
                if let Some(next_line) = buffer.line_text(line_idx + 1) {
                    if next_line.trim().starts_with('{') {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if a line is the end of a method/function
    /// A method end is a closing } at low indentation
    fn is_method_end(buffer: &Buffer, line_idx: usize) -> bool {
        if let Some(line) = buffer.line_text(line_idx) {
            let trimmed = line.trim();
            let indent = line.chars().take_while(|c| c.is_whitespace()).count();
            // A method end is typically a } at indentation <= 4 (or 8 for nested classes)
            // and the line is just the closing brace (possibly with semicolon for C++)
            if indent <= 4 && (trimmed == "}" || trimmed == "};" || trimmed == "},") {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_forward_counts_unicode_indentation_as_characters() {
        let mut buffer = Buffer::new_from_str("start\n\u{3000}\u{3000}\u{3000}int run() {\ntail\n");

        Motions::method_forward(&mut buffer, 1);

        assert_eq!(buffer.cursor().line(), 1);
        assert_eq!(buffer.cursor().col(), GraphemeCol(3));
    }

    #[test]
    fn method_end_forward_counts_unicode_indentation_as_characters() {
        let mut buffer = Buffer::new_from_str("start\nbody\n\u{3000}\u{3000}}\ntail\n");

        Motions::method_end_forward(&mut buffer, 1);

        assert_eq!(buffer.cursor().line(), 2);
        assert_eq!(buffer.cursor().col(), GraphemeCol(2));
    }
}
