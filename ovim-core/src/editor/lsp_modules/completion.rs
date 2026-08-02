//! LSP completion functionality
//!
//! This module handles code completion requests and application.
//! Completions are typically triggered by Ctrl+N or automatically in insert mode.

use super::super::Editor;
use crate::lsp::uri_from_file_path;
use crate::unicode::{byte_offset_for_grapheme, grapheme_at_index, grapheme_indices};
use anyhow::{anyhow, Result};
use std::collections::HashSet;

impl Editor {
    /// Request completion at current cursor position
    pub fn request_completion(&mut self) {
        self.lsp.intents.completion = true;
    }

    pub(crate) fn completion_trigger_context(&self) -> (usize, String) {
        let cursor = self.buffer().cursor();
        let line_idx = cursor.line();
        let cursor_col = cursor.col();

        let line_text = self
            .buffer()
            .line_text(line_idx)
            .unwrap_or_default()
            .to_string();

        completion_trigger_context_from_line(&line_text, cursor_col.0)
    }

    /// Derives the completion prefix from textEdit ranges when available.
    ///
    /// Uses the most common `textEdit.range.start.character` across items
    /// (majority vote) to determine where the completion token starts, then
    /// reads the text from that column to the cursor as the prefix.
    /// Falls back to word-boundary heuristic when no textEdit is present.
    pub(crate) fn derive_completion_prefix(
        &self,
        items: &[lsp_types::CompletionItem],
    ) -> (usize, String) {
        // Try to derive trigger_col from the textEdit ranges.
        // Use majority vote on range.start.character to handle multi-server
        // scenarios where different servers may have different ranges.
        let start_char = text_edit_majority_start(items);
        if let Some(utf16_start) = start_char {
            let line_idx = self.buffer().cursor().line();
            let cursor_col = self.buffer().cursor_char_col();

            // utf16_to_col returns char col — correct for delete_range
            let trigger_col = self.utf16_to_col(line_idx, utf16_start);

            // Sanity: trigger_col must be at or before cursor
            if trigger_col <= cursor_col {
                let line_text = self
                    .buffer()
                    .line_text(line_idx)
                    .unwrap_or_default()
                    .to_string();

                // Extract prefix using char indices
                let prefix: String = line_text
                    .chars()
                    .skip(trigger_col.0)
                    .take(cursor_col.0 - trigger_col.0)
                    .collect();
                return (trigger_col.0, prefix);
            }
        }

        // Fallback: word-boundary heuristic
        self.completion_trigger_context()
    }

    /// Returns the current prefix text from the stored trigger_col to cursor.
    /// Used for ongoing filtering while the completion menu is visible,
    /// so we don't re-derive the trigger column from word boundaries.
    pub(crate) fn completion_prefix_from_trigger_col(&self) -> String {
        let trigger_col = self.completion_menu().trigger_col(); // char col (bare usize)
        let cursor_col = self.buffer().cursor_char_col();

        if trigger_col > cursor_col.0 {
            return String::new();
        }

        let line_text = self
            .buffer()
            .line_text(self.buffer().cursor().line())
            .unwrap_or_default()
            .to_string();

        // Extract prefix using char indices
        line_text
            .chars()
            .skip(trigger_col)
            .take(cursor_col.0 - trigger_col)
            .collect()
    }

    /// Implementation of completion request
    pub(in crate::editor) async fn completion_impl(&mut self) -> Result<bool> {
        let lsp = match &self.lsp.state.lsp_manager {
            Some(lsp) => lsp.clone(),
            None => {
                self.set_lsp_status("LSP not available".to_string());
                return Ok(false);
            }
        };

        let Some(file_path) = self.buffer().file_path().map(|s| s.to_string()) else {
            self.set_lsp_status("Save file first to use completion".to_string());
            return Ok(false);
        };

        let abs_path = if std::path::Path::new(&file_path).is_absolute() {
            file_path.clone()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(&file_path).to_string_lossy().to_string(),
                Err(_) => {
                    self.set_lsp_status("Failed to resolve file path".to_string());
                    return Ok(false);
                }
            }
        };

        let uri = uri_from_file_path(&abs_path).ok_or_else(|| anyhow!("Invalid file path"))?;

        let cursor = self.buffer().cursor();
        let line = cursor.line() as u32;
        let col = cursor.col().0;
        let character = self.col_to_utf16(cursor.line(), col);
        let raw_trigger_char = {
            let line_text = self
                .buffer()
                .line_text(cursor.line())
                .unwrap_or_default()
                .to_string();
            if col > 0 {
                if col >= 2 {
                    let g1 = grapheme_at_index(&line_text, col.saturating_sub(1));
                    let g2 = grapheme_at_index(&line_text, col.saturating_sub(2));
                    if g2 == Some(":") && g1 == Some(":") {
                        Some(':')
                    } else if g2 == Some("-") && g1 == Some(">") {
                        Some('>')
                    } else {
                        match g1 {
                            Some(".") => Some('.'),
                            _ => None,
                        }
                    }
                } else {
                    match grapheme_at_index(&line_text, col.saturating_sub(1)) {
                        Some(".") => Some('.'),
                        _ => None,
                    }
                }
            } else {
                None
            }
        };

        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => {
                self.set_lsp_status("Language not supported for LSP".to_string());
                return Ok(false);
            }
        };

        // Sync document content to LSP on the main thread before spawning.
        // This avoids the spawned task racing with send_lsp_changes_if_modified()
        // over the debouncer — a source of timing bugs where stale content
        // was sent from the background task, overwriting newer content.
        self.ensure_lsp_document_synced().await;

        // Resolve the server group responsible for this document.
        let server_ids = lsp.servers_for_document(&language_id, std::path::Path::new(&file_path));

        // No LSP servers registered yet — server may still be initializing.
        if server_ids.is_empty() {
            // Only set "waiting" status if there isn't already a more specific
            // error (e.g., "LSP: rust-analyzer not found in PATH").
            if !self.lsp_status().starts_with("LSP:") {
                self.set_lsp_status("LSP: waiting for server...".to_string());
            }
            return Err(anyhow!("No LSP servers ready for {}", language_id));
        }

        let buffer_version_usize = self.buffer().version();

        // Spawn completion request in background (non-blocking).
        // Document sync already happened above via ensure_lsp_document_synced().
        // The task only makes the LSP request — no debouncer interaction.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let language_id = language_id.to_string();
        let file_path_for_task = file_path.clone();
        let task = tokio::spawn(async move {
            let mut supported_triggers: HashSet<char> = lsp
                .completion_trigger_characters_for_servers(&server_ids)
                .await
                .into_iter()
                .collect();
            for ch in crate::lsp::fallback_completion_trigger_characters(&language_id) {
                supported_triggers.insert(*ch);
            }
            let trigger_char = filter_supported_trigger(raw_trigger_char, &supported_triggers);

            let result = if server_ids.len() > 1 {
                lsp.completion_multi(&uri, line, character, &server_ids, trigger_char)
                    .await
            } else {
                lsp.completion(&uri, line, character, &language_id, trigger_char)
                    .await
            };
            let task_result = result.map(|items| crate::editor::lsp_slot::CompletionResult {
                items,
                file_path: file_path_for_task,
                buffer_version: buffer_version_usize,
                synced_content: None,
                synced_lsp_version: None,
            });

            let _ = tx.send(task_result);
        });

        self.lsp.slots.completion.fire(task, rx);

        self.set_lsp_status("Requesting completions...".to_string());
        Ok(true)
    }
}

fn filter_supported_trigger(trigger: Option<char>, supported: &HashSet<char>) -> Option<char> {
    let ch = trigger?;
    if supported.contains(&ch) {
        Some(ch)
    } else {
        None
    }
}

/// Whether `c` is part of a completion-prefix keyword.
///
/// Looser than the Vim motion-word definition: hyphens count, so a Tailwind
/// class like `w-1/2` is treated as one prefix when filtering completions and
/// the menu doesn't collapse mid-token. Motion code (`dw`, `ciw`, etc.) keeps
/// the strict alnum+`_` rule. Mirrored in `is_completion_ident_char` in
/// `ovim-core/src/editor/input/insert_mode.rs`.
fn is_completion_keyword_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn completion_trigger_context_from_line(line_text: &str, cursor_col: usize) -> (usize, String) {
    let cursor_byte = byte_offset_for_grapheme(line_text, cursor_col).unwrap_or(line_text.len());
    let before_cursor = &line_text[..cursor_byte.min(line_text.len())];

    let mut start_byte = before_cursor.len();
    let graphemes: Vec<(usize, &str)> = grapheme_indices(before_cursor).collect();
    for (byte_offset, grapheme) in graphemes.into_iter().rev() {
        let is_ident = grapheme.chars().all(is_completion_keyword_char);
        if !is_ident {
            break;
        }
        start_byte = byte_offset;
    }

    let trigger_col = line_text[..start_byte.min(line_text.len())].chars().count();
    let trigger_prefix = line_text[start_byte.min(cursor_byte)..cursor_byte].to_string();

    (trigger_col, trigger_prefix)
}

/// Returns the most common `textEdit.range.start.character` (UTF-16) across
/// completion items. Uses majority vote to handle multi-server scenarios.
///
/// The tally is a `BTreeMap` (not a `HashMap`) and the tie-break is explicit:
/// `HashMap` iteration order is randomized per process, so a popularity tie —
/// common when two servers both contribute (e.g. Java + JDTLS extensions) —
/// otherwise produced a non-deterministic trigger column across runs. On a tie
/// we prefer the *lower* start column (the longer replacement range), which is
/// what a single server's edits would do. (OV-00267)
fn text_edit_majority_start(items: &[lsp_types::CompletionItem]) -> Option<u32> {
    use std::collections::BTreeMap;

    let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
    for item in items {
        let start = match &item.text_edit {
            Some(lsp_types::CompletionTextEdit::Edit(edit)) => edit.range.start.character,
            Some(lsp_types::CompletionTextEdit::InsertAndReplace(ir)) => ir.insert.start.character,
            None => continue,
        };
        *counts.entry(start).or_default() += 1;
    }

    counts
        .into_iter()
        .max_by(|(start_a, count_a), (start_b, count_b)| {
            // Higher count wins; on a tie, the lower start column wins.
            count_a.cmp(count_b).then_with(|| start_b.cmp(start_a))
        })
        .map(|(start, _)| start)
}

#[cfg(test)]
mod tests {
    use super::{completion_trigger_context_from_line, text_edit_majority_start};
    use crate::unicode::grapheme_at_index;

    #[test]
    fn completion_trigger_context_basic_word() {
        let (col, prefix) = completion_trigger_context_from_line("foobar", 6);
        assert_eq!(col, 0);
        assert_eq!(prefix, "foobar");
    }

    #[test]
    fn completion_trigger_context_after_dot() {
        let (col, prefix) = completion_trigger_context_from_line("foo.", 4);
        assert_eq!(col, 4);
        assert_eq!(prefix, "");
    }

    #[test]
    fn completion_trigger_context_member_prefix() {
        let (col, prefix) = completion_trigger_context_from_line("foo.bar", 7);
        assert_eq!(col, 4);
        assert_eq!(prefix, "bar");
    }

    #[test]
    fn completion_trigger_context_double_colon() {
        let (col, prefix) = completion_trigger_context_from_line("foo::bar", 8);
        assert_eq!(col, 5);
        assert_eq!(prefix, "bar");
    }

    #[test]
    fn completion_trigger_context_underscore_digits() {
        let (col, prefix) = completion_trigger_context_from_line("__x1", 4);
        assert_eq!(col, 0);
        assert_eq!(prefix, "__x1");
    }

    // Tailwind classes contain hyphens; the fallback scanner must keep them
    // as part of the prefix so filtering matches the LSP's view of the token.
    #[test]
    fn completion_trigger_context_hyphenated_prefix() {
        let (col, prefix) = completion_trigger_context_from_line("class=\"bg-wh", 12);
        assert_eq!(col, 7);
        assert_eq!(prefix, "bg-wh");
    }

    #[test]
    fn completion_trigger_context_trailing_hyphen() {
        let (col, prefix) = completion_trigger_context_from_line("w-", 2);
        assert_eq!(col, 0);
        assert_eq!(prefix, "w-");
    }

    #[test]
    fn trigger_char_detection_dot() {
        let line = "s.";
        assert_eq!(grapheme_at_index(line, 1), Some("."));
    }

    fn item_with_text_edit(
        label: &str,
        start_char: u32,
        end_char: u32,
    ) -> lsp_types::CompletionItem {
        lsp_types::CompletionItem {
            label: label.to_string(),
            text_edit: Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: start_char,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: end_char,
                    },
                },
                new_text: label.to_string(),
            })),
            ..Default::default()
        }
    }

    #[test]
    fn majority_start_single_server() {
        let items = vec![
            item_with_text_edit("bg-white", 11, 16),
            item_with_text_edit("bg-black", 11, 16),
            item_with_text_edit("bg-red-500", 11, 16),
        ];
        assert_eq!(text_edit_majority_start(&items), Some(11));
    }

    #[test]
    fn majority_start_multi_server_picks_most_common() {
        // 3 items from Tailwind (start=11), 1 from TypeScript (start=14)
        let items = vec![
            item_with_text_edit("bg-white", 11, 16),
            item_with_text_edit("bg-black", 11, 16),
            item_with_text_edit("bg-red-500", 11, 16),
            item_with_text_edit("white", 14, 16),
        ];
        assert_eq!(text_edit_majority_start(&items), Some(11));
    }

    #[test]
    fn majority_start_tie_prefers_lower_column_deterministically() {
        // Two servers, equal popularity (2 items each) at start=8 and start=11.
        // The result must be stable across runs (no HashMap order dependence)
        // and pick the lower column. (OV-00267)
        let items = vec![
            item_with_text_edit("alpha", 11, 16),
            item_with_text_edit("beta", 8, 16),
            item_with_text_edit("gamma", 11, 16),
            item_with_text_edit("delta", 8, 16),
        ];
        assert_eq!(text_edit_majority_start(&items), Some(8));
        // Reversed input order — same answer.
        let items_rev: Vec<_> = items.into_iter().rev().collect();
        assert_eq!(text_edit_majority_start(&items_rev), Some(8));
    }

    #[test]
    fn majority_start_no_text_edits() {
        let items = vec![lsp_types::CompletionItem {
            label: "foo".to_string(),
            ..Default::default()
        }];
        assert_eq!(text_edit_majority_start(&items), None);
    }

    #[test]
    fn majority_start_empty() {
        assert_eq!(text_edit_majority_start(&[]), None);
    }
}
