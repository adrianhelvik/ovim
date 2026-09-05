use crate::buffer::Buffer;
use crate::change::CursorPos;

/// A low-level buffer edit using absolute rope char offsets.
/// Unambiguous — no line/col clamping, no newline confusion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Text was inserted at an absolute char offset.
    Insert { offset: usize, text: String },
    /// Text was deleted starting at an absolute char offset.
    Delete { offset: usize, text: String },
}

impl Edit {
    /// Produces the inverse edit (for undo).
    /// Insert ↔ Delete with the same offset and text.
    pub fn inverse(&self) -> Edit {
        match self {
            Edit::Insert { offset, text } => Edit::Delete {
                offset: *offset,
                text: text.clone(),
            },
            Edit::Delete { offset, text } => Edit::Insert {
                offset: *offset,
                text: text.clone(),
            },
        }
    }

    /// Applies this edit to a buffer, updating tree-sitter, highlights, and
    /// version counters so derived state stays consistent with the rope.
    pub fn apply(&self, buffer: &mut Buffer) {
        match self {
            Edit::Insert { offset, text } => {
                let pos = (*offset).min(buffer.rope().len_chars());
                let line = buffer.rope().char_to_line(pos);
                let col = pos - buffer.rope().line_to_char(line);
                buffer.insert_text_at(line, crate::unicode::CharCol(col), text);
            }
            Edit::Delete { offset, text } => {
                let end = offset + text.chars().count();
                buffer.delete_char_range(*offset, end);
            }
        }
    }

    /// Returns the absolute char offset where this edit occurs.
    pub fn offset(&self) -> usize {
        match self {
            Edit::Insert { offset, .. } | Edit::Delete { offset, .. } => *offset,
        }
    }

    /// Returns the text involved in this edit.
    pub fn text(&self) -> &str {
        match self {
            Edit::Insert { text, .. } | Edit::Delete { text, .. } => text,
        }
    }
}

/// Returns the text inserted by an edit sequence that survives its later edits.
///
/// Offsets are character offsets in the successive document states. Unknown
/// original characters are represented by `None`, so deleting existing text
/// cannot accidentally add it to the result. Rebase to the smallest offset to
/// keep storage proportional to the edited span, not the document prefix.
pub fn surviving_inserted_text(edits: &[Edit]) -> String {
    let Some(origin) = edits.iter().map(Edit::offset).min() else {
        return String::new();
    };
    let mut span = Vec::new();
    for edit in edits {
        let start = edit.offset() - origin;
        match edit {
            Edit::Insert { text, .. } => {
                span.resize(span.len().max(start), None);
                span.splice(start..start, text.chars().map(Some));
            }
            Edit::Delete { text, .. } => {
                let end = start + text.chars().count();
                span.resize(span.len().max(end), None);
                span.drain(start..end);
            }
        }
    }
    span.into_iter().flatten().collect()
}

/// A mechanical undo record. Reversal is trivial — no semantic interpretation.
#[derive(Clone, Debug)]
pub enum UndoEntry {
    /// Single edit
    Single(Edit),
    /// Atomic group of edits (undo all or none)
    Group {
        edits: Vec<Edit>,
        cursor_before: CursorPos,
        cursor_after: CursorPos,
    },
}

impl UndoEntry {
    /// Undoes this entry by applying inverse edits in reverse order.
    pub fn undo(&self, buffer: &mut Buffer) {
        match self {
            UndoEntry::Single(edit) => {
                edit.inverse().apply(buffer);
            }
            UndoEntry::Group { edits, .. } => {
                // Apply inverses in reverse order to correctly undo
                for edit in edits.iter().rev() {
                    edit.inverse().apply(buffer);
                }
            }
        }
    }

    /// Redoes this entry by applying edits in forward order.
    pub fn redo(&self, buffer: &mut Buffer) {
        match self {
            UndoEntry::Single(edit) => {
                edit.apply(buffer);
            }
            UndoEntry::Group { edits, .. } => {
                for edit in edits {
                    edit.apply(buffer);
                }
            }
        }
    }

    /// Returns the cursor position before this change was made.
    pub fn cursor_before(&self) -> Option<CursorPos> {
        match self {
            UndoEntry::Single(_) => None,
            UndoEntry::Group { cursor_before, .. } => Some(*cursor_before),
        }
    }

    /// Returns the cursor position after this change was made.
    pub fn cursor_after(&self) -> Option<CursorPos> {
        match self {
            UndoEntry::Single(_) => None,
            UndoEntry::Group { cursor_after, .. } => Some(*cursor_after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_insert_inverse_is_delete() {
        let edit = Edit::Insert {
            offset: 5,
            text: "hello".to_string(),
        };
        let inv = edit.inverse();
        assert_eq!(
            inv,
            Edit::Delete {
                offset: 5,
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn test_edit_delete_inverse_is_insert() {
        let edit = Edit::Delete {
            offset: 3,
            text: "world".to_string(),
        };
        let inv = edit.inverse();
        assert_eq!(
            inv,
            Edit::Insert {
                offset: 3,
                text: "world".to_string()
            }
        );
    }

    #[test]
    fn test_edit_inverse_round_trip() {
        let edit = Edit::Insert {
            offset: 0,
            text: "test".to_string(),
        };
        assert_eq!(edit.inverse().inverse(), edit);
    }

    #[test]
    fn test_edit_apply_insert() {
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit = Edit::Insert {
            offset: 5,
            text: " beautiful".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello beautiful world\n");
    }

    #[test]
    fn test_edit_apply_delete() {
        let mut buffer = Buffer::new_from_str("hello beautiful world\n");
        let edit = Edit::Delete {
            offset: 5,
            text: " beautiful".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_edit_apply_insert_then_inverse_restores() {
        let mut buffer = Buffer::new_from_str("hello\n");
        let edit = Edit::Insert {
            offset: 5,
            text: " world".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");

        edit.inverse().apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello\n");
    }

    #[test]
    fn test_edit_apply_delete_then_inverse_restores() {
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit = Edit::Delete {
            offset: 5,
            text: " world".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello\n");

        edit.inverse().apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_edit_insert_at_beginning() {
        let mut buffer = Buffer::new_from_str("world\n");
        let edit = Edit::Insert {
            offset: 0,
            text: "hello ".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_edit_insert_at_end() {
        let mut buffer = Buffer::new_from_str("hello\n");
        // Insert before the trailing newline
        let edit = Edit::Insert {
            offset: 5,
            text: " world".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_edit_offset_and_text_accessors() {
        let insert = Edit::Insert {
            offset: 10,
            text: "abc".to_string(),
        };
        assert_eq!(insert.offset(), 10);
        assert_eq!(insert.text(), "abc");

        let delete = Edit::Delete {
            offset: 5,
            text: "xyz".to_string(),
        };
        assert_eq!(delete.offset(), 5);
        assert_eq!(delete.text(), "xyz");
    }

    #[test]
    fn test_undo_entry_single_undo() {
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit = Edit::Insert {
            offset: 5,
            text: " beautiful".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello beautiful world\n");

        let entry = UndoEntry::Single(edit);
        entry.undo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_undo_entry_single_redo() {
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit = Edit::Insert {
            offset: 5,
            text: " beautiful".to_string(),
        };
        let entry = UndoEntry::Single(edit);
        entry.redo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello beautiful world\n");
    }

    #[test]
    fn test_undo_entry_group_undo_reverses_order() {
        // Simulate: delete "world" at offset 6, then insert "rust" at offset 6
        // Buffer: "hello world\n" -> "hello \n" -> "hello rust\n"
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit1 = Edit::Delete {
            offset: 6,
            text: "world".to_string(),
        };
        let edit2 = Edit::Insert {
            offset: 6,
            text: "rust".to_string(),
        };

        edit1.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello \n");
        edit2.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello rust\n");

        let entry = UndoEntry::Group {
            edits: vec![edit1, edit2],
            cursor_before: CursorPos::new(0, crate::unicode::GraphemeCol(6)),
            cursor_after: CursorPos::new(0, crate::unicode::GraphemeCol(9)),
        };

        entry.undo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_undo_entry_group_redo() {
        let mut buffer = Buffer::new_from_str("hello world\n");
        let edit1 = Edit::Delete {
            offset: 6,
            text: "world".to_string(),
        };
        let edit2 = Edit::Insert {
            offset: 6,
            text: "rust".to_string(),
        };

        let entry = UndoEntry::Group {
            edits: vec![edit1, edit2],
            cursor_before: CursorPos::new(0, crate::unicode::GraphemeCol(6)),
            cursor_after: CursorPos::new(0, crate::unicode::GraphemeCol(9)),
        };

        entry.redo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hello rust\n");
    }

    #[test]
    fn test_undo_entry_group_undo_then_redo_round_trip() {
        let mut buffer = Buffer::new_from_str("line1\nline2\nline3\n");

        // Simulate joining line1 and line2: delete "\nline2" at appropriate offset, insert " line2"
        // "line1\nline2\nline3\n" -> delete range covering "\nline2" -> insert " line2"
        let edit1 = Edit::Delete {
            offset: 5,
            text: "\nline2".to_string(),
        };
        let edit2 = Edit::Insert {
            offset: 5,
            text: " line2".to_string(),
        };

        edit1.apply(&mut buffer);
        edit2.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "line1 line2\nline3\n");

        let entry = UndoEntry::Group {
            edits: vec![edit1, edit2],
            cursor_before: CursorPos::new(0, crate::unicode::GraphemeCol(0)),
            cursor_after: CursorPos::new(0, crate::unicode::GraphemeCol(5)),
        };

        // Undo should restore original
        entry.undo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "line1\nline2\nline3\n");

        // Redo should re-apply
        entry.redo(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "line1 line2\nline3\n");
    }

    #[test]
    fn test_edit_delete_multibyte_chars() {
        // "café\n" — 'é' is 2 bytes but 1 char
        let mut buffer = Buffer::new_from_str("café\n");
        let edit = Edit::Delete {
            offset: 3,
            text: "é".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "caf\n");
    }

    #[test]
    fn test_edit_delete_emoji() {
        // "hi🦀bye\n" — '🦀' is 4 bytes but 1 char
        let mut buffer = Buffer::new_from_str("hi🦀bye\n");
        let edit = Edit::Delete {
            offset: 2,
            text: "🦀".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "hibye\n");
    }

    #[test]
    fn test_edit_delete_umlaut_undo_redo_roundtrip() {
        let mut buffer = Buffer::new_from_str("über\n");
        let edit = Edit::Delete {
            offset: 0,
            text: "ü".to_string(),
        };
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "ber\n");

        // Undo (inverse = insert)
        edit.inverse().apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "über\n");

        // Redo (apply again)
        edit.apply(&mut buffer);
        assert_eq!(buffer.rope().to_string(), "ber\n");
    }

    #[test]
    fn test_undo_entry_cursor_accessors() {
        use crate::unicode::GraphemeCol;
        let before = CursorPos::new(5, GraphemeCol(10));
        let after = CursorPos::new(5, GraphemeCol(15));
        let entry = UndoEntry::Group {
            edits: vec![],
            cursor_before: before,
            cursor_after: after,
        };
        assert_eq!(entry.cursor_before(), Some(before));
        assert_eq!(entry.cursor_after(), Some(after));

        let single = UndoEntry::Single(Edit::Insert {
            offset: 0,
            text: String::new(),
        });
        assert_eq!(single.cursor_before(), None);
        assert_eq!(single.cursor_after(), None);
    }
}
