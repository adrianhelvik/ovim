//! Classified motion ranges for operator application (OV-00300).
//!
//! Vim's operator model: a motion produces a range plus a classification —
//! charwise or linewise, inclusive or exclusive — and the operator consumes
//! the classified range uniformly. Hand-rolling that logic per operator ×
//! motion combination is how each combination drifts independently
//! (OV-00288/289/290/292/293 were all instances).
//!
//! [`MotionRange::from_exclusive`] encodes vim's exclusive-motion
//! adjustments (`:help exclusive`):
//!
//! 1. If an exclusive motion ends in column 0 of a later line, the end
//!    retreats to the end of the previous line (the motion becomes
//!    inclusive of that line's last character).
//! 2. If rule 1 fired and the start lies at or before the first non-blank
//!    character of its line, the motion becomes **linewise**.
//!
//! New operator+motion combinations should build one of these instead of
//! computing ranges inline; existing ones migrate as they're touched.

use crate::buffer::Buffer;
use crate::unicode::CharCol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wise {
    Charwise,
    Linewise,
}

/// A normalized, buffer-ordered motion range.
///
/// `start` is inclusive; `end` is an EXCLUSIVE char position (one past the
/// last affected char). For `Linewise`, the columns are irrelevant: the
/// range covers whole lines `start.0..=end.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionRange {
    pub start: (usize, CharCol),
    pub end: (usize, CharCol),
    pub wise: Wise,
}

impl MotionRange {
    /// Builds a range from an EXCLUSIVE motion between two positions (in
    /// either order — cursor and motion target), applying vim's exclusive
    /// adjustments described in the module docs.
    pub fn from_exclusive(buffer: &Buffer, a: (usize, CharCol), b: (usize, CharCol)) -> Self {
        let (mut start, mut end) = if (a.0, a.1 .0) <= (b.0, b.1 .0) {
            (a, b)
        } else {
            (b, a)
        };

        let mut wise = Wise::Charwise;

        // Rule 1: exclusive end in column 0 of a later line retreats to the
        // end of the previous line.
        let mut retreated = false;
        if end.1 == CharCol::ZERO && end.0 > start.0 {
            let prev = end.0 - 1;
            end = (prev, CharCol(buffer.line_len(prev)));
            retreated = true;
        }

        // Rule 2: if rule 1 fired and the start is at or before the first
        // non-blank of its line, the whole motion becomes linewise.
        if retreated && start.1 <= buffer.first_non_blank_col(start.0) {
            wise = Wise::Linewise;
            start.1 = CharCol::ZERO;
        }

        Self { start, end, wise }
    }
}

impl Buffer {
    /// Deletes a normalized motion range. Returns the deleted text (for
    /// `Linewise`, whole lines including terminators).
    pub fn delete_motion_range(&mut self, range: MotionRange) -> String {
        match range.wise {
            Wise::Charwise => {
                self.delete_range(range.start.0, range.start.1, range.end.0, range.end.1)
            }
            Wise::Linewise => {
                self.delete_range(range.start.0, CharCol::ZERO, range.end.0 + 1, CharCol::ZERO)
            }
        }
    }

    /// Reads a normalized motion range without mutating. For `Linewise`,
    /// every line carries a terminator (vim's linewise registers always
    /// end in a newline, even when the buffer's last line does not).
    pub fn yank_motion_range(&self, range: MotionRange) -> String {
        match range.wise {
            Wise::Charwise => {
                let start_char = self.rope().line_to_char(range.start.0) + range.start.1 .0;
                let end_char = self.rope().line_to_char(range.end.0) + range.end.1 .0;
                if end_char <= start_char {
                    return String::new();
                }
                self.rope().slice(start_char..end_char).to_string()
            }
            Wise::Linewise => {
                let mut yanked = String::new();
                for line_idx in range.start.0..=range.end.0 {
                    if let Some(line) = self.line_text(line_idx) {
                        yanked.push_str(&line);
                        yanked.push('\n');
                    }
                }
                yanked
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(content: &str) -> Buffer {
        Buffer::new_from_str(content)
    }

    #[test]
    fn plain_exclusive_range_stays_charwise() {
        // } from mid-line to a NON-col-0 position: no adjustment.
        let b = buf("abc def\n");
        let r = MotionRange::from_exclusive(&b, (0, CharCol(0)), (0, CharCol(3)));
        assert_eq!(r.wise, Wise::Charwise);
        assert_eq!(r.start, (0, CharCol(0)));
        assert_eq!(r.end, (0, CharCol(3)));
    }

    #[test]
    fn end_in_col_zero_retreats_to_previous_line_end() {
        // d} from (0,4) on "foo bar" to the blank line (1,0): the end
        // retreats to (0,7) and stays charwise — the blank line survives.
        let b = buf("foo bar\n\nbaz");
        let r = MotionRange::from_exclusive(&b, (0, CharCol(4)), (1, CharCol(0)));
        assert_eq!(r.wise, Wise::Charwise);
        assert_eq!(r.start, (0, CharCol(4)));
        assert_eq!(r.end, (0, CharCol(7)));
    }

    #[test]
    fn retreat_from_line_start_promotes_to_linewise() {
        // d} from (0,0): start at/before first non-blank -> linewise over
        // the paragraph's lines.
        let b = buf("foo bar\n\nbaz");
        let r = MotionRange::from_exclusive(&b, (0, CharCol(0)), (1, CharCol(0)));
        assert_eq!(r.wise, Wise::Linewise);
        assert_eq!(r.start.0, 0);
        assert_eq!(r.end.0, 0);
    }

    #[test]
    fn backward_argument_order_is_normalized() {
        let b = buf("foo\n\nbar baz");
        // d{ from (2,4): cursor passed first, target second.
        let r = MotionRange::from_exclusive(&b, (2, CharCol(4)), (1, CharCol(0)));
        assert_eq!(r.start, (1, CharCol(0)));
        assert_eq!(r.end, (2, CharCol(4)));
        assert_eq!(r.wise, Wise::Charwise);
    }

    #[test]
    fn delete_charwise_retreated_range_keeps_blank_line() {
        let mut b = buf("foo bar\n\nbaz");
        let r = MotionRange::from_exclusive(&b, (0, CharCol(4)), (1, CharCol(0)));
        let deleted = b.delete_motion_range(r);
        assert_eq!(deleted, "bar");
        assert_eq!(b.rope().to_string(), "foo \n\nbaz\n");
    }

    #[test]
    fn delete_linewise_range_takes_whole_lines() {
        let mut b = buf("foo bar\n\nbaz");
        let r = MotionRange::from_exclusive(&b, (0, CharCol(0)), (1, CharCol(0)));
        let deleted = b.delete_motion_range(r);
        assert_eq!(deleted, "foo bar\n");
        assert_eq!(b.rope().to_string(), "\nbaz\n");
    }

    #[test]
    fn yank_linewise_always_terminates_lines() {
        let b = buf("foo\nbar");
        let r = MotionRange {
            start: (0, CharCol::ZERO),
            end: (1, CharCol(3)),
            wise: Wise::Linewise,
        };
        assert_eq!(b.yank_motion_range(r), "foo\nbar\n");
    }
}
