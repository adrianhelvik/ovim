//! Text object handling in normal mode.
//!
//! Handles text objects after an operator with 'i' (inner) or 'a' (around) prefix:
//! diw, daw, di", da", di{, da{, dip, dap, dit, dat, dif, daf, dii, dai, etc.

use crate::editor::input::helpers;
use crate::editor::{
    CursorPos, Editor, Operator, PendingChangeRepeat, RegisterType, TextObjectRange,
    TextObjectType, TextObjects,
};
use crate::mode::Mode;
use crate::repeat_action::{CaseTransform, RepeatAction};
use crate::{KeyCode, KeyEvent};
use anyhow::Result;

/// Try to handle a text object after operator + 'i' or 'a'.
///
/// Returns `Ok(true)` if the key was handled, `Ok(false)` otherwise.
pub fn try_handle(editor: &mut Editor, key_event: KeyEvent) -> Result<bool> {
    let text_obj_type = match editor.pending_command() {
        Some('i') | Some('a') => editor.pending_command().unwrap(),
        _ => return Ok(false),
    };

    let operator = match editor.pending_operator() {
        Some(op) => op,
        None => return Ok(false),
    };

    editor.clear_pending_command();
    editor.clear_pending_operator();
    editor.clear_count();

    let result = match key_event.code {
        KeyCode::Char('w') => {
            if text_obj_type == 'i' {
                TextObjects::inner_word(editor.buffer())
            } else {
                TextObjects::around_word(editor.buffer())
            }
        }
        KeyCode::Char('W') => {
            if text_obj_type == 'i' {
                TextObjects::inner_big_word(editor.buffer())
            } else {
                TextObjects::around_big_word(editor.buffer())
            }
        }
        KeyCode::Char('p') => {
            if text_obj_type == 'i' {
                TextObjects::inner_paragraph(editor.buffer())
            } else {
                TextObjects::around_paragraph(editor.buffer())
            }
        }
        KeyCode::Char('s') => {
            if text_obj_type == 'i' {
                TextObjects::inner_sentence(editor.buffer())
            } else {
                TextObjects::around_sentence(editor.buffer())
            }
        }
        KeyCode::Char('"') | KeyCode::Char('\'') | KeyCode::Char('`') => {
            let quote = match key_event.code {
                KeyCode::Char(c) => c,
                _ => unreachable!(),
            };
            TextObjects::quoted_string(editor.buffer(), quote, text_obj_type == 'a')
        }
        KeyCode::Char('(') | KeyCode::Char(')') | KeyCode::Char('b') => {
            TextObjects::paired_delimiters(editor.buffer(), '(', ')', text_obj_type == 'a')
        }
        KeyCode::Char('[') | KeyCode::Char(']') => {
            TextObjects::paired_delimiters(editor.buffer(), '[', ']', text_obj_type == 'a')
        }
        KeyCode::Char('{') | KeyCode::Char('}') | KeyCode::Char('B') => {
            TextObjects::paired_delimiters(editor.buffer(), '{', '}', text_obj_type == 'a')
        }
        KeyCode::Char('<') | KeyCode::Char('>') => {
            TextObjects::paired_delimiters(editor.buffer(), '<', '>', text_obj_type == 'a')
        }
        KeyCode::Char('t') => TextObjects::tag(editor.buffer(), text_obj_type == 'a'),
        KeyCode::Char('i') => {
            let tab_width = editor.options.tab_width;
            if text_obj_type == 'i' {
                TextObjects::inner_indent(editor.buffer(), tab_width)
            } else {
                TextObjects::around_indent(editor.buffer(), tab_width)
            }
        }
        KeyCode::Char('f') => {
            if text_obj_type == 'i' {
                TextObjects::inner_function(editor.buffer())
            } else {
                TextObjects::around_function(editor.buffer())
            }
        }
        _ => {
            // Unknown text object
            return Ok(true);
        }
    };

    // Determine the TextObjectType for semantic repeat
    let inner = text_obj_type == 'i';
    let object_type: TextObjectType = match key_event.code {
        KeyCode::Char('w') => TextObjectType::Word { inner, big: false },
        KeyCode::Char('W') => TextObjectType::Word { inner, big: true },
        KeyCode::Char('"') | KeyCode::Char('\'') | KeyCode::Char('`') => {
            let quote = match key_event.code {
                KeyCode::Char(c) => c,
                _ => unreachable!(),
            };
            TextObjectType::Quote { char: quote, inner }
        }
        KeyCode::Char('(') | KeyCode::Char(')') | KeyCode::Char('b') => TextObjectType::Paired {
            open: '(',
            close: ')',
            inner,
        },
        KeyCode::Char('[') | KeyCode::Char(']') => TextObjectType::Paired {
            open: '[',
            close: ']',
            inner,
        },
        KeyCode::Char('{') | KeyCode::Char('}') | KeyCode::Char('B') => TextObjectType::Paired {
            open: '{',
            close: '}',
            inner,
        },
        KeyCode::Char('<') | KeyCode::Char('>') => TextObjectType::Paired {
            open: '<',
            close: '>',
            inner,
        },
        KeyCode::Char('p') => TextObjectType::Paragraph { inner },
        KeyCode::Char('s') => TextObjectType::Sentence { inner },
        KeyCode::Char('t') => TextObjectType::Tag { inner },
        KeyCode::Char('i') => TextObjectType::Indent {
            inner,
            tab_width: editor.options.tab_width,
        },
        KeyCode::Char('f') => TextObjectType::Function { inner },
        _ => unreachable!("text object key should be validated before object_type mapping"),
    };

    if let Some(range) = result {
        match operator {
            Operator::Delete => {
                apply_delete_operator(editor, range, object_type)?;
            }
            Operator::Yank => {
                apply_yank_operator(editor, range, key_event.code)?;
            }
            Operator::Change => {
                apply_change_operator(editor, range, object_type)?;
            }
            Operator::Lowercase => {
                apply_case_operator(editor, range, object_type, CaseTransform::Lower)?;
            }
            Operator::Uppercase => {
                apply_case_operator(editor, range, object_type, CaseTransform::Upper)?;
            }
            Operator::ToggleCase => {
                apply_case_operator(editor, range, object_type, CaseTransform::Toggle)?;
            }
            Operator::Fold => {
                let start_line = range.start_line.min(range.end_line);
                let end_line = range.start_line.max(range.end_line);
                editor
                    .buffer_mut()
                    .fold_manager_mut()
                    .create_fold(start_line, end_line);
            }
            Operator::Indent | Operator::Dedent | Operator::AutoIndent => {
                // Don't make sense with text objects
            }
        }
    }

    Ok(true)
}

fn apply_delete_operator(
    editor: &mut Editor,
    range: TextObjectRange,
    object_type: TextObjectType,
) -> Result<()> {
    let cursor_before = editor.cursor_position();

    let deleted = TextObjects::yank_range(editor.buffer(), range)?;

    // Pattern B: record() + push_recorded_undo() + set_repeat_action()
    let ((), edits) = editor.buffer_mut().record(|buf| {
        buf.delete_range(
            range.start_line,
            range.start_col,
            range.end_line,
            range.end_col,
        );
        buf.set_cursor_char_col(range.start_line, range.start_col);
    });
    let cursor_after = editor.cursor_position();
    if !edits.is_empty() {
        // Paragraph text objects (dip/dap) are linewise — store the register as
        // Line so a subsequent `p` pastes it as whole new lines, mirroring the
        // yank path's `p`-key branch. Otherwise a Character register splices the
        // paragraph into the middle of the current line.
        let reg_type = if matches!(object_type, TextObjectType::Paragraph { .. }) {
            RegisterType::Line
        } else {
            RegisterType::Character
        };
        editor.delete_to_register_with_type(deleted, reg_type);
        editor.push_recorded_undo(edits, cursor_before, cursor_after);
        editor.set_repeat_action(RepeatAction::DeleteTextObject { object_type });
    }
    helpers::clamp_cursor_to_buffer(editor);

    Ok(())
}

fn apply_yank_operator(
    editor: &mut Editor,
    range: TextObjectRange,
    key_code: KeyCode,
) -> Result<()> {
    let yanked = TextObjects::yank_range(editor.buffer(), range)?;
    let reg_type = if key_code == KeyCode::Char('p') {
        RegisterType::Line
    } else {
        RegisterType::Character
    };
    editor.yank_to_register_with_type(yanked, reg_type);
    if reg_type == RegisterType::Line {
        editor.set_yank_flash_lines(range.start_line, range.end_line);
    } else {
        // range cols are char-space (CharCol); flash range takes grapheme
        // cols — convert per line (OV-00299).
        let start_text = editor
            .buffer()
            .line_text(range.start_line)
            .unwrap_or_default()
            .to_string();
        let end_text = editor
            .buffer()
            .line_text(range.end_line)
            .unwrap_or_default()
            .to_string();
        let start_grapheme = crate::unicode::char_to_grapheme_col(&start_text, range.start_col);
        let end_grapheme = crate::unicode::char_to_grapheme_col(&end_text, range.end_col);
        editor.set_yank_flash_range(
            range.start_line,
            start_grapheme,
            range.end_line,
            end_grapheme,
        );
    }
    Ok(())
}

fn apply_change_operator(
    editor: &mut Editor,
    range: TextObjectRange,
    object_type: TextObjectType,
) -> Result<()> {
    let cursor = editor.buffer().cursor();
    let cursor_before = CursorPos::new(cursor.line(), cursor.col());

    let deleted = TextObjects::yank_range(editor.buffer(), range)?;

    let ((), edits) = editor.buffer_mut().record(|buf| {
        buf.delete_range(
            range.start_line,
            range.start_col,
            range.end_line,
            range.end_col,
        );
        buf.set_cursor_char_col(range.start_line, range.start_col);
    });
    if edits.is_empty() {
        return Ok(());
    }
    editor.delete_to_register(deleted);
    let cursor_after = editor.cursor_position();
    let delete_token = editor.push_recorded_undo(edits, cursor_before, cursor_after);
    editor.set_pending_change_repeat(PendingChangeRepeat {
        delete_action: RepeatAction::DeleteTextObject { object_type },
        linewise: false,
        delete_token: Some(delete_token),
    });

    let new_cursor = editor.buffer().cursor();
    let new_cursor_pos = CursorPos::new(new_cursor.line(), new_cursor.col());
    editor.start_change_building(new_cursor_pos);
    editor.set_mode(Mode::Insert);

    Ok(())
}

fn apply_case_operator(
    editor: &mut Editor,
    range: TextObjectRange,
    object_type: TextObjectType,
    transform: CaseTransform,
) -> Result<()> {
    let text = TextObjects::yank_range(editor.buffer(), range)?;

    let transformed = transform.apply_to(&text);

    if transformed != text {
        editor.record_operation(
            |buf| {
                buf.delete_range(
                    range.start_line,
                    range.start_col,
                    range.end_line,
                    range.end_col,
                );
                buf.insert_text_at(range.start_line, range.start_col, &transformed);

                // Keep cursor behavior consistent with prior path: land at end
                // of the transformed text. Tracked in char-space (CharCol).
                let mut final_line = range.start_line;
                let mut final_col = range.start_col;
                for ch in transformed.chars() {
                    if ch == '\n' {
                        final_line += 1;
                        final_col = crate::unicode::CharCol::ZERO;
                    } else {
                        final_col += 1;
                    }
                }
                buf.set_cursor_char_col(final_line, final_col);
            },
            Some(RepeatAction::ChangeCaseTextObject {
                object_type,
                transform,
            }),
        );
    }

    Ok(())
}
