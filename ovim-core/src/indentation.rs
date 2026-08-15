//! Indentation policy and visual-column arithmetic.
//!
//! Indentation is measured in terminal columns, never in bytes or characters.
//! A hard tab advances to the next `tab_width` boundary, while `shift_width`
//! controls how far editor indentation commands move. Keeping those concepts
//! separate is what makes combinations such as `tabstop=8 shiftwidth=4` work.

/// Complete indentation policy used by editing operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndentOptions {
    /// Display width of a hard tab (`tabstop`).
    pub tab_width: usize,
    /// Columns in one indentation level (`shiftwidth`).
    pub shift_width: usize,
    /// Insert-mode soft tab stop. `-1` follows `shift_width`; `0` follows
    /// hard-tab behavior; positive values use that many columns.
    pub soft_tab_stop: isize,
    /// Encode indentation using spaces only (`expandtab`).
    pub expand_tab: bool,
    /// Preserve the exact existing whitespace prefix when opening a line
    /// (`copyindent`). Otherwise indentation is canonically reconstructed.
    pub copy_indent: bool,
}

impl Default for IndentOptions {
    fn default() -> Self {
        Self {
            tab_width: 4,
            shift_width: 4,
            soft_tab_stop: -1,
            expand_tab: true,
            copy_indent: false,
        }
    }
}

impl IndentOptions {
    /// Returns a policy with all widths made safe for arithmetic and allocation.
    pub fn normalized(self) -> Self {
        Self {
            tab_width: self.tab_width.max(1),
            shift_width: self.shift_width.max(1),
            soft_tab_stop: self.soft_tab_stop.max(-1),
            ..self
        }
    }

    /// Effective insert-mode soft-tab width.
    pub fn effective_soft_tab_width(self) -> usize {
        let options = self.normalized();
        match options.soft_tab_stop {
            n if n > 0 => n as usize,
            -1 => options.shift_width,
            _ => options.tab_width,
        }
    }

    /// Encodes an absolute indentation width from column zero.
    pub fn encode_indent(self, width: usize) -> String {
        let options = self.normalized();
        if options.expand_tab {
            return " ".repeat(width);
        }

        let tabs = width / options.tab_width;
        let spaces = width % options.tab_width;
        "\t".repeat(tabs) + &" ".repeat(spaces)
    }

    /// Text needed to advance from `column` to the next soft tab stop.
    pub fn tab_text(self, column: usize) -> String {
        let options = self.normalized();

        if options.soft_tab_stop == 0 && !options.expand_tab {
            return "\t".to_string();
        }

        let width = options.effective_soft_tab_width();
        let target = next_stop(column, width);
        encode_gap(column, target, options.expand_tab, options.tab_width)
    }

    /// Text needed to advance between two absolute visual columns according
    /// to this policy, without overshooting the target.
    pub fn gap_text(self, from: usize, to: usize) -> String {
        let options = self.normalized();
        encode_gap(from, to, options.expand_tab, options.tab_width)
    }

    /// Previous insert-mode soft tab stop, clamped to column zero.
    pub fn previous_soft_tab_stop(self, column: usize) -> usize {
        if column == 0 {
            return 0;
        }
        let width = self.effective_soft_tab_width();
        ((column - 1) / width) * width
    }

    /// Next `shift_width` boundary used by insert-mode Ctrl-T.
    pub fn next_indent_stop(self, column: usize) -> usize {
        next_stop(column, self.normalized().shift_width)
    }

    /// Previous `shift_width` boundary used by insert-mode Ctrl-D.
    pub fn previous_indent_stop(self, column: usize) -> usize {
        if column == 0 {
            return 0;
        }
        let width = self.normalized().shift_width;
        ((column - 1) / width) * width
    }
}

/// Number of leading ASCII indentation characters (spaces and hard tabs).
pub fn leading_char_count(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .count()
}

/// Leading indentation slice. Spaces and hard tabs are ASCII, so the character
/// count is also a valid byte boundary.
pub fn leading_str(line: &str) -> &str {
    &line[..leading_char_count(line)]
}

/// Display width of text with hard tabs advancing to actual tab stops.
pub fn visual_width(text: &str, tab_width: usize) -> usize {
    crate::display::display_width(text, tab_width.max(1))
}

/// Display width of a line's leading indentation.
pub fn leading_width(line: &str, tab_width: usize) -> usize {
    visual_width(leading_str(line), tab_width)
}

fn next_stop(column: usize, width: usize) -> usize {
    let width = width.max(1);
    column + (width - column % width)
}

/// Encode the smallest tab/space sequence that advances from `from` to `to`
/// without overshooting. Hard tabs are used only when they land at or before
/// the requested target column.
fn encode_gap(mut from: usize, to: usize, expand_tab: bool, tab_width: usize) -> String {
    if to <= from {
        return String::new();
    }
    if expand_tab {
        return " ".repeat(to - from);
    }

    let tab_width = tab_width.max(1);
    let mut result = String::new();
    while from < to {
        let next_tab = next_stop(from, tab_width);
        if next_tab <= to {
            result.push('\t');
            from = next_tab;
        } else {
            result.push(' ');
            from += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_advance_to_boundaries_in_mixed_prefixes() {
        assert_eq!(visual_width(" \t", 4), 4);
        assert_eq!(visual_width("  \t  ", 4), 6);
        assert_eq!(visual_width("\t\t", 8), 16);
    }

    #[test]
    fn encoded_indent_round_trips_every_width() {
        for tab_width in [2, 4, 8] {
            for expand_tab in [false, true] {
                let options = IndentOptions {
                    tab_width,
                    expand_tab,
                    ..IndentOptions::default()
                };
                for width in 0..32 {
                    let encoded = options.encode_indent(width);
                    assert_eq!(visual_width(&encoded, tab_width), width);
                    if expand_tab {
                        assert!(!encoded.contains('\t'));
                    }
                }
            }
        }
    }

    #[test]
    fn hard_tabs_never_overshoot_soft_stop() {
        let options = IndentOptions {
            tab_width: 8,
            shift_width: 4,
            soft_tab_stop: -1,
            expand_tab: false,
            copy_indent: false,
        };

        assert_eq!(options.tab_text(0), "    ");
        assert_eq!(options.tab_text(2), "  ");
        assert_eq!(options.tab_text(4), "\t");
        assert_eq!(options.tab_text(8), "    ");
    }

    #[test]
    fn soft_tab_stops_move_on_a_grid() {
        let options = IndentOptions::default();
        assert_eq!(options.previous_soft_tab_stop(2), 0);
        assert_eq!(options.previous_soft_tab_stop(4), 0);
        assert_eq!(options.previous_soft_tab_stop(6), 4);
        assert_eq!(options.next_indent_stop(2), 4);
        assert_eq!(options.next_indent_stop(4), 8);
    }
}
