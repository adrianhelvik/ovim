//! Multi-buffer management, file loading, and buffer switching

use super::Editor;
use crate::buffer::{Buffer, BufferId};
use crate::change::Change;
use anyhow::Result;

/// Returns true if the path looks like a scratch buffer (e.g., `[LspInfo]`).
/// Scratch buffer paths use the `[Title]` convention and should not pollute
/// the `%` (current file) or `#` (alternate file) registers.
fn is_scratch_path(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| f.starts_with('[') && f.ends_with(']'))
        .unwrap_or(false)
}

impl Editor {
    /// Gets a reference to the current buffer
    pub fn buffer(&self) -> &Buffer {
        &self.buffers[self.current_buffer_index]
    }

    pub fn language_catalog(&self) -> std::sync::Arc<crate::language_catalog::LanguageCatalog> {
        self.language_catalog.clone()
    }

    pub fn language_id_for_path(&self, path: &str) -> Option<String> {
        if let Some(language) = self.language_catalog.detect(path) {
            let id = if language.lsp_language_id == language.config.id {
                crate::syntax::LanguageRegistry::get_lsp_language_id(path)
                    .unwrap_or(&language.lsp_language_id)
            } else {
                &language.lsp_language_id
            };
            return Some(id.to_string());
        }
        crate::syntax::LanguageRegistry::get_lsp_language_id(path).map(str::to_string)
    }

    /// Gets a buffer by ID (index)
    pub fn get_buffer(&self, id: usize) -> Option<&Buffer> {
        self.buffers.get(id)
    }

    /// Finds the current index for a stable buffer ID.
    pub fn find_buffer_index_by_id(&self, buffer_id: BufferId) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.id() == buffer_id)
    }

    /// Gets a buffer by stable buffer ID.
    pub fn get_buffer_by_id(&self, buffer_id: BufferId) -> Option<&Buffer> {
        let idx = self.find_buffer_index_by_id(buffer_id)?;
        self.buffers.get(idx)
    }

    /// Gets a mutable buffer by stable buffer ID.
    pub fn get_buffer_by_id_mut(&mut self, buffer_id: BufferId) -> Option<&mut Buffer> {
        let idx = self.find_buffer_index_by_id(buffer_id)?;
        self.buffers.get_mut(idx)
    }

    /// Gets a mutable reference to the current buffer
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current_buffer_index]
    }

    /// Sets the current buffer file path and updates the % register
    /// to keep register-based file operations in sync with the buffer path.
    pub fn set_file_path(&mut self, path: String) {
        self.buffer_mut().set_file_path(path.clone());
        self.registers.set_current_file(path);
    }

    /// Gets a reference to a buffer by index.
    pub fn buffer_at(&self, index: usize) -> Option<&Buffer> {
        self.buffers.get(index)
    }

    /// Adds a new buffer and returns its index.
    pub fn push_buffer(&mut self, buf: Buffer) -> usize {
        let mut buf = buf;
        buf.set_language_catalog(self.language_catalog.clone());
        self.buffers.push(buf);
        self.buffers.len() - 1
    }

    /// Gets the number of open buffers
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    /// Gets the current buffer index (0-based)
    pub fn current_buffer_index(&self) -> usize {
        self.current_buffer_index
    }

    /// Gets a list of all buffer names (file paths or "[No Name]")
    pub fn buffer_names(&self) -> Vec<String> {
        self.buffers
            .iter()
            .map(|buf| {
                buf.file_path()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "[No Name]".to_string())
            })
            .collect()
    }

    /// Lists all buffers with their index and status
    pub fn list_buffers(&self) -> String {
        let mut result = String::new();
        for (i, buf) in self.buffers.iter().enumerate() {
            let current_marker = if i == self.current_buffer_index {
                "%"
            } else {
                " "
            };
            let modified_marker = if buf.is_modified() { "+" } else { " " };
            let name = buf.file_path().unwrap_or("[No Name]");
            result.push_str(&format!(
                "{}{} {}: {}\n",
                current_marker,
                modified_marker,
                i + 1,
                name
            ));
        }
        result
    }

    /// Switches to a buffer by index (0-based)
    pub fn switch_to_buffer(&mut self, index: usize) {
        if index < self.buffers.len() && index != self.current_buffer_index {
            // Save current file to alternate file register (skip scratch buffers)
            if let Some(current_path) = self.buffer().file_path() {
                if !is_scratch_path(current_path) {
                    self.registers.set_alternate_file(current_path.to_string());
                }
            }

            self.current_buffer_index = index;
            self.lsp.state.needs_lsp_init = true;

            // Clear buffer-local marks (a-z) when switching files
            self.nav.marks.clear();

            // Clear LSP UI state (hover, completions, etc.)
            self.clear_lsp_state();

            // Update current file register (skip scratch buffers like [LspInfo])
            if let Some(new_path) = self.buffer().file_path() {
                if !is_scratch_path(new_path) {
                    self.registers.set_current_file(new_path.to_string());
                }
            }

            // Refresh per-file diagnostic caches (counts + current_file_diagnostics)
            self.request_diagnostics_refresh();
        }
    }

    /// Switches to the next buffer
    pub fn next_buffer(&mut self) {
        if self.buffers.len() > 1 {
            // BUG FIX #4: Save old file path for didClose before switching
            let old_file_path = self.buffer().file_path().map(|s| s.to_string());

            // Save current file to alternate file register (skip scratch buffers)
            if let Some(current_path) = old_file_path.as_ref() {
                if !is_scratch_path(current_path) {
                    self.registers.set_alternate_file(current_path.to_string());
                }
            }

            self.current_buffer_index = (self.current_buffer_index + 1) % self.buffers.len();
            self.lsp.state.needs_lsp_init = true;

            // Clear buffer-local marks (a-z) when switching files
            self.nav.marks.clear();

            // Clear LSP UI state (hover, completions, etc.)
            self.clear_lsp_state();

            // Update current file register (skip scratch buffers like [LspInfo])
            if let Some(new_path) = self.buffer().file_path() {
                if !is_scratch_path(new_path) {
                    self.registers.set_current_file(new_path.to_string());
                }
            }

            // Refresh per-file diagnostic caches after file switch
            self.request_diagnostics_refresh();

            // Mark that we need to send didClose for the old file
            if old_file_path.is_some() {
                self.lsp.state.pending_did_close_file = old_file_path;
            }
        }
    }

    /// Switches to the previous buffer
    pub fn prev_buffer(&mut self) {
        if self.buffers.len() > 1 {
            // BUG FIX #4: Save old file path for didClose before switching
            let old_file_path = self.buffer().file_path().map(|s| s.to_string());

            // Save current file to alternate file register (skip scratch buffers)
            if let Some(current_path) = old_file_path.as_ref() {
                if !is_scratch_path(current_path) {
                    self.registers.set_alternate_file(current_path.to_string());
                }
            }

            self.current_buffer_index = if self.current_buffer_index == 0 {
                self.buffers.len() - 1
            } else {
                self.current_buffer_index - 1
            };
            self.lsp.state.needs_lsp_init = true;

            // Clear buffer-local marks (a-z) when switching files
            self.nav.marks.clear();

            // Clear LSP UI state (hover, completions, etc.)
            self.clear_lsp_state();

            // Update current file register (skip scratch buffers like [LspInfo])
            if let Some(new_path) = self.buffer().file_path() {
                if !is_scratch_path(new_path) {
                    self.registers.set_current_file(new_path.to_string());
                }
            }

            // Refresh per-file diagnostic caches after file switch
            self.request_diagnostics_refresh();

            // Mark that we need to send didClose for the old file
            if old_file_path.is_some() {
                self.lsp.state.pending_did_close_file = old_file_path;
            }
        }
    }

    /// Deletes the current buffer and switches to another if available
    /// Returns true if the editor should quit (no more buffers)
    pub fn delete_current_buffer(&mut self) -> bool {
        if self.buffers.len() == 1 {
            // Last buffer - quit the editor
            return true;
        }

        // Remove current buffer (track sync state)
        if let Some(path) = self.buffer().file_path().map(|s| s.to_string()) {
            self.lsp.state.document_sync.remove(&path);
        }

        // Remove current buffer
        self.buffers.remove(self.current_buffer_index);

        // Adjust index if we were at the end
        if self.current_buffer_index >= self.buffers.len() {
            self.current_buffer_index = self.buffers.len() - 1;
        }

        self.lsp.state.needs_lsp_init = true;
        false
    }

    /// Adds a new buffer and switches to it
    pub fn add_buffer(&mut self, mut buffer: Buffer) {
        buffer.set_language_catalog(self.language_catalog.clone());
        self.buffers.push(buffer);
        self.current_buffer_index = self.buffers.len() - 1;
        self.lsp.state.needs_lsp_init = true;
    }

    /// Opens a scratch buffer with the given content and title
    /// The buffer is read-only and has no file path
    pub fn open_scratch_buffer(&mut self, title: &str, content: &str) {
        let mut buffer = Buffer::new_from_str(content);
        buffer.set_read_only(true);
        // Use a special naming convention for scratch buffers
        // This won't be saved to disk since there's no actual file path
        buffer.set_file_path(format!("[{}]", title));
        self.add_buffer(buffer);
        // Don't need LSP for scratch buffers
        self.lsp.state.needs_lsp_init = false;
        self.mark_dirty();
    }

    /// Finds the index of a buffer with the given file path
    /// Returns None if no buffer has that file path
    pub(crate) fn find_buffer_by_path(&self, file_path: &str) -> Option<usize> {
        // Normalize paths for comparison
        let target_path = std::path::Path::new(file_path).canonicalize().ok()?;

        for (index, buffer) in self.buffers.iter().enumerate() {
            if let Some(buf_path) = buffer.file_path() {
                if let Ok(buf_canonical) = std::path::Path::new(buf_path).canonicalize() {
                    if target_path == buf_canonical {
                        return Some(index);
                    }
                }
            }
        }
        None
    }

    /// Finds or loads a buffer by URI, returning its index
    /// Does NOT switch to the buffer (unlike open_file)
    /// Returns None if the URI cannot be converted to a path or loading fails
    pub(crate) fn find_or_load_buffer_index_by_uri(
        &mut self,
        uri: &lsp_types::Uri,
    ) -> Option<usize> {
        // Convert URI to file path
        let file_path = crate::lsp::uri_to_file_path(uri)?;
        let path_str = file_path.to_str()?;

        // Check if buffer is already open
        if let Some(index) = self.find_buffer_by_path(path_str) {
            return Some(index);
        }

        // Load the file into a new buffer (don't switch to it)
        let buffer = Buffer::load_file(&file_path).ok()?;
        self.buffers.push(buffer);
        // Note: We intentionally don't change current_buffer_index here
        // to avoid switching away from the user's current file

        Some(self.buffers.len() - 1)
    }

    /// Applies LSP text edits to a specific buffer by index
    /// Returns true if successful, false if index is invalid
    pub(crate) fn apply_lsp_edits_to_buffer_index(
        &mut self,
        buffer_index: usize,
        edits: Vec<lsp_types::TextEdit>,
    ) -> bool {
        if buffer_index >= self.buffers.len() {
            return false;
        }

        // Sort edits in reverse order (bottom to top) to avoid position invalidation
        let mut sorted_edits = edits;
        sorted_edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then(b.range.start.character.cmp(&a.range.start.character))
        });

        let (cursor_before, cursor_after, recorded_edits, file_path) = {
            let buffer = &mut self.buffers[buffer_index];
            let cursor_before =
                crate::change::CursorPos::new(buffer.cursor().line(), buffer.cursor().col());

            let ((), recorded_edits) = buffer.record(|buf| {
                for edit in sorted_edits {
                    let start_line = edit.range.start.line as usize;
                    // Convert UTF-16 positions to character positions
                    let start_col =
                        Self::utf16_to_col_for_buffer(buf, start_line, edit.range.start.character);
                    let end_line = edit.range.end.line as usize;
                    let end_col =
                        Self::utf16_to_col_for_buffer(buf, end_line, edit.range.end.character);

                    // Delete the range
                    if start_line != end_line || start_col != end_col {
                        buf.delete_range(start_line, start_col, end_line, end_col);
                    }

                    // Insert new text. Normalize CR variants so LSP-supplied
                    // CRLF/CR doesn't survive in the rope (OV-00251).
                    if !edit.new_text.is_empty() {
                        let new_text = crate::buffer::normalize_for_buffer(&edit.new_text);
                        buf.insert_text_at(start_line, start_col, new_text.as_ref());
                    }
                }
            });

            let cursor_after =
                crate::change::CursorPos::new(buffer.cursor().line(), buffer.cursor().col());
            let file_path = buffer.file_path().map(|s| s.to_string());
            (cursor_before, cursor_after, recorded_edits, file_path)
        };

        if recorded_edits.is_empty() {
            return true;
        }

        // LSP-applied edits should be undoable but should not become dot-repeat
        // templates, so we push directly to undo/redo stacks without touching
        // last_change/last_repeat_action.
        let change = Change::recorded(recorded_edits, cursor_before, cursor_after);
        {
            let cm = self.buffers[buffer_index].change_manager_mut();
            cm.push_undo_change_preserving_repeat(change);
        }

        // Ensure the edited document is re-synced to LSP. We do NOT set
        // `did_open_sent = true` here unconditionally — workspace edits may
        // touch files we've never opened (find_or_load_buffer_index_by_uri
        // can load a fresh buffer), and claiming "already opened" would skip
        // the required didOpen. Leaving the flag untouched lets the sync
        // planner route through DocumentSyncRequestAction::DidOpen for
        // never-opened files while still firing didChange for opened ones.
        // (OV-00231)
        if let Some(file_path) = file_path {
            let state = self.lsp.state.document_sync.entry(file_path).or_default();
            state.mark_modified();
        }

        if buffer_index == self.current_buffer_index {
            self.invalidate_hover_cache();
            self.request_diagnostics_refresh();
        }

        true
    }

    /// Helper to convert UTF-16 offset to char column for a specific buffer
    pub(crate) fn utf16_to_col_for_buffer(
        buffer: &Buffer,
        line: usize,
        utf16_offset: u32,
    ) -> crate::unicode::CharCol {
        if let Some(line_text) = buffer.line_text(line) {
            let line_str = line_text.to_string();
            let mut col = 0;
            let mut utf16_pos = 0u32;

            for ch in line_str.chars() {
                if utf16_pos >= utf16_offset {
                    break;
                }
                utf16_pos += ch.len_utf16() as u32;
                col += 1;
            }
            crate::unicode::CharCol(col)
        } else {
            crate::unicode::CharCol(utf16_offset as usize)
        }
    }

    /// Opens a file, switching to existing buffer if already open
    /// or creating a new buffer if not
    pub fn open_file<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;

        // Check if file is already open
        if let Some(index) = self.find_buffer_by_path(path_str) {
            // Switch to existing buffer (and run file-switch side effects)
            self.switch_to_buffer(index);
            return Ok(());
        }

        // File not open, load it.
        // Buffer::load_file always canonicalizes via normalize_path(), so
        // file_path() is always Some. The unwrap_or is a defensive fallback
        // that is unreachable in practice.
        let buffer = Buffer::load_file(path)?;
        let resolved_path = buffer.file_path().unwrap_or(path_str).to_string();
        self.add_buffer(buffer);
        self.registers.set_current_file(resolved_path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::str::FromStr;

    #[test]
    fn is_scratch_path_detects_bracket_names() {
        // Absolute paths with bracket filenames are scratch buffers
        assert!(is_scratch_path("/some/path/[LspInfo]"));
        assert!(is_scratch_path("/Users/foo/[Diagnostics]"));
        assert!(is_scratch_path("[Scratch]"));

        // Regular file paths are not scratch buffers
        assert!(!is_scratch_path("/some/path/main.rs"));
        assert!(!is_scratch_path("file.txt"));
        assert!(!is_scratch_path("/path/to/[partial"));
        assert!(!is_scratch_path("/path/to/partial]"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn open_file_updates_current_file_register() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("main.rs");

        fs::write(&file, "hello\n").expect("write file");
        let expected_path = file
            .canonicalize()
            .expect("canonicalize")
            .to_string_lossy()
            .to_string();

        let mut editor = Editor::default();
        editor.open_file(&file).expect("open file");

        assert_eq!(editor.registers().get(Some('%')), expected_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn scratch_buffer_does_not_update_percent_register() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("main.rs");
        fs::write(&file, "hello\n").expect("write file");

        let expected_path = file
            .canonicalize()
            .expect("canonicalize")
            .to_string_lossy()
            .to_string();

        let mut editor = Editor::default();
        editor.open_file(&file).expect("open file");
        assert_eq!(editor.registers().get(Some('%')), expected_path);

        // Opening a scratch buffer should NOT overwrite %
        editor.open_scratch_buffer("LspInfo", "some info");
        assert_eq!(editor.registers().get(Some('%')), expected_path);

        // Switching back to the real file should preserve %
        editor.switch_to_buffer(0);
        assert_eq!(editor.registers().get(Some('%')), expected_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn scratch_buffer_does_not_update_alternate_register() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("main.rs");
        fs::write(&file, "hello\n").expect("write");

        let expected_path = file.canonicalize().unwrap().to_string_lossy().to_string();

        let mut editor = Editor::default();
        editor.open_file(&file).expect("open file");
        // Set # to a known value
        editor
            .registers_mut()
            .set_alternate_file(expected_path.clone());

        // Open scratch buffer — it should NOT overwrite #
        editor.open_scratch_buffer("Scratch", "scratch content");
        assert_eq!(editor.registers().get(Some('#')), expected_path);

        // Switching from scratch to real file should NOT set # to scratch path
        editor.switch_to_buffer(0);
        assert_eq!(editor.registers().get(Some('#')), expected_path);
    }

    /// OV-00231: When a workspace edit modifies a file the user has not
    /// opened, `find_or_load_buffer_index_by_uri` loads it from disk and
    /// `apply_lsp_edits_to_buffer_index` records the change. The sync state
    /// for that file must NOT be marked `did_open_sent = true` — no didOpen
    /// has actually been sent. Otherwise the next sync tick fires didChange
    /// without a preceding didOpen (LSP protocol violation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn workspace_edit_on_unopened_file_does_not_claim_did_open_sent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        let untouched = dir.path().join("untouched.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");
        fs::write(&untouched, "fn helper() {}\n").expect("write untouched");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");

        // Simulate the workspace_edits.rs path: locate-or-load, then apply.
        let untouched_uri = lsp_types::Uri::from_str(&format!(
            "file://{}",
            untouched.canonicalize().unwrap().to_string_lossy()
        ))
        .expect("uri");

        let buffer_index = editor
            .find_or_load_buffer_index_by_uri(&untouched_uri)
            .expect("load untouched buffer");

        let edit = lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 3,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 9,
                },
            },
            new_text: "renamed".to_string(),
        };

        let applied = editor.apply_lsp_edits_to_buffer_index(buffer_index, vec![edit]);
        assert!(applied, "apply should succeed");

        let untouched_path = untouched
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let state = editor
            .lsp
            .state
            .document_sync
            .get(&untouched_path)
            .expect("sync state for edited file should exist");
        assert!(
            !state.did_open_sent,
            "did_open_sent must remain false for files we never sent didOpen for"
        );
        assert!(
            state.is_modified(),
            "the edit must still be queued for sync"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn next_prev_buffer_skip_scratch_for_registers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("main.rs");
        fs::write(&file, "hello\n").expect("write");

        let expected_path = file
            .canonicalize()
            .expect("canonicalize")
            .to_string_lossy()
            .to_string();

        let mut editor = Editor::default();
        editor.open_file(&file).expect("open file");
        editor.open_scratch_buffer("Info", "info");

        // We're now on the scratch buffer (index 1).
        // next_buffer should cycle to file (index 0) and update %
        editor.next_buffer();
        assert_eq!(editor.registers().get(Some('%')), expected_path);

        // prev_buffer back to scratch — % should remain the real file
        editor.prev_buffer();
        assert_eq!(editor.registers().get(Some('%')), expected_path);
    }
}
