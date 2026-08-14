//! LSP workspace edit application
//!
//! This module handles applying text edits and workspace edits from LSP responses.
//! Used by rename, code actions, formatting, and organize imports.

use super::super::{Change, Editor};
use crate::lsp::uri_to_file_path;
use anyhow::Result;
use std::path::PathBuf;

/// Extract `TextEdit` values from a slice of `OneOf<TextEdit, AnnotatedTextEdit>`.
pub(in crate::editor) fn extract_text_edits(
    edits: &[lsp_types::OneOf<lsp_types::TextEdit, lsp_types::AnnotatedTextEdit>],
) -> Vec<lsp_types::TextEdit> {
    edits
        .iter()
        .map(|e| match e {
            lsp_types::OneOf::Left(edit) => edit.clone(),
            lsp_types::OneOf::Right(annot_edit) => annot_edit.text_edit.clone(),
        })
        .collect()
}

impl Editor {
    /// Apply LSP text edits to the current buffer.
    ///
    /// Ordering and position resolution are handled by
    /// [`Editor::apply_text_edits_to_buffer`], the single implementation of
    /// the LSP `TextEdit[]` application rules (OV-00332).
    pub(in crate::editor) fn apply_lsp_edits(&mut self, edits: Vec<lsp_types::TextEdit>) {
        let cursor_before = self.cursor_position();
        let (_all_applied, recorded_edits) = self
            .buffer_mut()
            .record(|buf| Self::apply_text_edits_to_buffer(buf, &edits));

        if !recorded_edits.is_empty() {
            let cursor_after = self.cursor_position();
            self.push_recorded_undo(recorded_edits, cursor_before, cursor_after);
        }

        // LSP-applied edits are still edits: ensure we sync back to the server so
        // diagnostics and other LSP features refresh.
        self.invalidate_hover_cache();
        self.mark_buffer_modified_force_send();
        self.request_diagnostics_refresh();
    }

    /// Apply a workspace edit (used for rename, organize imports, etc.)
    pub fn apply_workspace_edit(&mut self, edit: lsp_types::WorkspaceEdit) -> Result<bool> {
        let mut all_applied = true;
        let mut modified_files = Vec::new();
        let mut discarded_files = Vec::new();

        // LSP spec: when `document_changes` is present, `changes` is ignored.
        // `document_changes` is the newer, more powerful format that supports
        // versioned edits and resource operations.
        if let Some(document_changes) = edit.document_changes {
            match document_changes {
                lsp_types::DocumentChanges::Edits(edits) => {
                    for text_doc_edit in edits {
                        if !self.apply_text_document_edit(
                            &text_doc_edit,
                            &mut modified_files,
                            &mut discarded_files,
                        ) {
                            all_applied = false;
                        }
                    }
                }
                lsp_types::DocumentChanges::Operations(ops) => {
                    for op in ops {
                        match op {
                            lsp_types::DocumentChangeOperation::Edit(text_doc_edit) => {
                                if !self.apply_text_document_edit(
                                    &text_doc_edit,
                                    &mut modified_files,
                                    &mut discarded_files,
                                ) {
                                    all_applied = false;
                                }
                            }
                            lsp_types::DocumentChangeOperation::Op(resource_op) => {
                                let cursor_before = self.cursor_position();
                                let (applied, undo_change) =
                                    Self::apply_resource_op(resource_op, cursor_before);
                                if !applied {
                                    all_applied = false;
                                } else if let Some(change) = undo_change {
                                    self.push_resource_undo_change(change);
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(changes) = edit.changes {
            // Fallback: deprecated `changes` field (still widely used by older servers)
            for (uri, text_edits) in changes {
                if !self.apply_uri_edits(
                    &uri,
                    None,
                    text_edits,
                    &mut modified_files,
                    &mut discarded_files,
                ) {
                    all_applied = false;
                }
            }
        }

        let mut status_parts = Vec::new();
        if !modified_files.is_empty() {
            status_parts.push(if modified_files.len() == 1 {
                format!("Modified {}", modified_files[0])
            } else {
                format!("Modified {} files", modified_files.len())
            });
        }
        for file in &discarded_files {
            status_parts.push(format!("edit for {} discarded: document changed", file));
        }
        if !status_parts.is_empty() {
            self.set_lsp_status(status_parts.join("; "));
        }

        Ok(all_applied)
    }

    /// Apply one document's edits from `DocumentChanges`, honoring the
    /// `OptionalVersionedTextDocumentIdentifier` version (OV-00330).
    fn apply_text_document_edit(
        &mut self,
        text_doc_edit: &lsp_types::TextDocumentEdit,
        modified_files: &mut Vec<String>,
        discarded_files: &mut Vec<String>,
    ) -> bool {
        let text_edits = extract_text_edits(&text_doc_edit.edits);
        self.apply_uri_edits(
            &text_doc_edit.text_document.uri,
            text_doc_edit.text_document.version,
            text_edits,
            modified_files,
            discarded_files,
        )
    }

    /// Apply a batch of text edits to the document identified by `uri`.
    ///
    /// `version` is the server's `OptionalVersionedTextDocumentIdentifier`
    /// version — the LSP staleness mechanism. When it no longer matches our
    /// view of the document, the document's edits are skipped and tracked in
    /// `discarded_files` (OV-00330, version-guard leg).
    ///
    /// Buffers that were loaded purely to receive this edit (hidden, clean
    /// before the edit) are written straight to disk afterwards so a
    /// multi-file rename can't silently lose edits on quit (OV-00331).
    fn apply_uri_edits(
        &mut self,
        uri: &lsp_types::Uri,
        version: Option<i32>,
        text_edits: Vec<lsp_types::TextEdit>,
        modified_files: &mut Vec<String>,
        discarded_files: &mut Vec<String>,
    ) -> bool {
        if let Some(version) = version {
            if !self.workspace_edit_version_current(uri, version) {
                let file_name = uri_to_file_path(uri)
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("document")
                    .to_string();
                discarded_files.push(file_name);
                return false;
            }
        }

        let Some(buffer_index) = self.find_or_load_buffer_index_by_uri(uri) else {
            return false;
        };
        // A buffer that was clean before this edit and isn't visible anywhere
        // in the UI exists only to carry this workspace edit — write it
        // through to disk below. Buffers the user has open (or has unrelated
        // unsaved changes in) stay in-memory modified; saving is their call.
        let was_clean = !self.buffer_index_is_modified(buffer_index);
        Self::track_modified_file(uri, modified_files);
        let mut applied = self.apply_lsp_edits_to_buffer_index(buffer_index, text_edits);
        // Write through ONLY on full success: a partial apply (some edits
        // dropped as invalid) must never be persisted to disk — it stays
        // in-memory modified where the :qa guard covers it and the failure
        // is reported upstream (external review finding on OV-00331/332).
        if applied
            && was_clean
            && self.buffer_index_is_modified(buffer_index)
            && !self.buffer_is_open_in_ui(buffer_index)
            && !self.write_through_workspace_edit_buffer(buffer_index)
        {
            applied = false;
        }
        applied
    }

    /// OV-00330 (version-guard leg): returns false when a versioned document
    /// edit no longer matches our view of the document.
    ///
    /// Only the current file's LSP document version is tracked synchronously
    /// on the editor side (`lsp.state.current_file_lsp_version`, refreshed on
    /// each sync tick). `LspManager::get_document_version` is async and this
    /// apply path is sync, so for OTHER files the edit is accepted unchecked
    /// rather than blocking the editor thread — the current file is where
    /// staleness bites (the user typing while a slow rename resolves).
    fn workspace_edit_version_current(&self, uri: &lsp_types::Uri, version: i32) -> bool {
        let Some(edit_path) = uri_to_file_path(uri) else {
            return true;
        };
        let Some(current_path) = self.buffer().file_path() else {
            return true;
        };
        let current_path = std::path::Path::new(current_path);
        let is_current_file = current_path == edit_path
            || match (current_path.canonicalize(), edit_path.canonicalize()) {
                (Ok(current), Ok(edit)) => current == edit,
                _ => false,
            };
        if !is_current_file {
            return true;
        }
        // A local edit marks sync dirty BEFORE the version counter advances
        // (the bump happens on the next sync tick), and server workspace
        // edits are drained ahead of the sync step in the tick. A versioned
        // edit arriving in that window would pass a bare version compare
        // while targeting an older rope — dirty sync state means our
        // content is newer than any version the server can know about, so
        // reject (external review finding on OV-00330).
        if self.lsp_document_is_modified() == Some(true) {
            return false;
        }
        let known_version = self.lsp.state.current_file_lsp_version;
        if known_version <= 0 {
            // Version unknown (no manager yet / never synced) — don't guess.
            // This keeps the common unversioned/unsynced path working.
            return true;
        }
        version == known_version
    }

    /// Track a modified file by URI into the list.
    fn track_modified_file(uri: &lsp_types::Uri, modified_files: &mut Vec<String>) {
        if let Some(path) = uri_to_file_path(uri) {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            if !modified_files.contains(&file_name) {
                modified_files.push(file_name);
            }
        }
    }

    fn snapshot_paths(paths: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        paths
            .iter()
            .map(|path| (path.clone(), Change::snapshot_file(path)))
            .collect()
    }

    fn build_resource_undo_change(
        before: Vec<(PathBuf, Option<Vec<u8>>)>,
        after: Vec<(PathBuf, Option<Vec<u8>>)>,
        cursor: crate::change::CursorPos,
    ) -> Option<Change> {
        let mut snapshots = Vec::new();
        for ((path, before_bytes), (_, after_bytes)) in before.into_iter().zip(after) {
            if before_bytes != after_bytes {
                snapshots.push(Change::resource_snapshot(path, before_bytes, after_bytes));
            }
        }

        if snapshots.is_empty() {
            None
        } else {
            Some(Change::resource_op(snapshots, cursor, cursor))
        }
    }

    fn push_resource_undo_change(&mut self, change: Change) {
        self.buffer_mut()
            .change_manager_mut()
            .push_undo_change_preserving_repeat(change);
    }

    /// Apply a resource operation (create, rename, delete).
    fn apply_resource_op(
        resource_op: lsp_types::ResourceOp,
        cursor: crate::change::CursorPos,
    ) -> (bool, Option<Change>) {
        match resource_op {
            lsp_types::ResourceOp::Create(create_file) => {
                let Some(file_path) = uri_to_file_path(&create_file.uri) else {
                    return (false, None);
                };
                let paths = vec![file_path.clone()];
                let before = Self::snapshot_paths(&paths);

                let should_create = create_file
                    .options
                    .as_ref()
                    .map(|opts| {
                        if file_path.exists() {
                            opts.overwrite.unwrap_or(false)
                        } else {
                            true
                        }
                    })
                    .unwrap_or(!file_path.exists());

                let applied = !should_create || std::fs::write(&file_path, "").is_ok();
                if !applied {
                    return (false, None);
                }
                let after = Self::snapshot_paths(&paths);
                (
                    true,
                    Self::build_resource_undo_change(before, after, cursor),
                )
            }
            lsp_types::ResourceOp::Rename(rename_file) => {
                let Some(old_path) = uri_to_file_path(&rename_file.old_uri) else {
                    return (false, None);
                };
                let Some(new_path) = uri_to_file_path(&rename_file.new_uri) else {
                    return (false, None);
                };
                let paths = vec![old_path.clone(), new_path.clone()];
                let before = Self::snapshot_paths(&paths);

                if let Some(parent) = new_path.parent() {
                    if !parent.exists() && std::fs::create_dir_all(parent).is_err() {
                        return (false, None);
                    }
                }

                if std::fs::rename(&old_path, &new_path).is_err() {
                    return (false, None);
                }
                let after = Self::snapshot_paths(&paths);
                (
                    true,
                    Self::build_resource_undo_change(before, after, cursor),
                )
            }
            lsp_types::ResourceOp::Delete(delete_file) => {
                let Some(file_path) = uri_to_file_path(&delete_file.uri) else {
                    return (false, None);
                };
                let paths = vec![file_path.clone()];
                let before = Self::snapshot_paths(&paths);

                let applied = !file_path.exists() || std::fs::remove_file(&file_path).is_ok();
                if !applied {
                    return (false, None);
                }
                let after = Self::snapshot_paths(&paths);
                (
                    true,
                    Self::build_resource_undo_change(before, after, cursor),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::str::FromStr;

    fn file_uri(path: &std::path::Path) -> lsp_types::Uri {
        lsp_types::Uri::from_str(&format!(
            "file://{}",
            path.canonicalize().unwrap().to_string_lossy()
        ))
        .expect("uri")
    }

    fn replace_edit(start_char: u32, end_char: u32, new_text: &str) -> lsp_types::TextEdit {
        lsp_types::TextEdit {
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
            new_text: new_text.to_string(),
        }
    }

    // lsp_types::Uri has interior mutability per clippy, but WorkspaceEdit's
    // `changes` field is defined as HashMap<Uri, _> by the protocol crate —
    // we just construct what the API demands.
    #[allow(clippy::mutable_key_type)]
    fn changes_edit(
        uri: lsp_types::Uri,
        edits: Vec<lsp_types::TextEdit>,
    ) -> lsp_types::WorkspaceEdit {
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, edits);
        lsp_types::WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }
    }

    fn versioned_edit(
        uri: lsp_types::Uri,
        version: Option<i32>,
        edits: Vec<lsp_types::TextEdit>,
    ) -> lsp_types::WorkspaceEdit {
        lsp_types::WorkspaceEdit {
            document_changes: Some(lsp_types::DocumentChanges::Edits(vec![
                lsp_types::TextDocumentEdit {
                    text_document: lsp_types::OptionalVersionedTextDocumentIdentifier {
                        uri,
                        version,
                    },
                    edits: edits.into_iter().map(lsp_types::OneOf::Left).collect(),
                },
            ])),
            ..Default::default()
        }
    }

    /// OV-00331: a multi-file WorkspaceEdit loads unopened files into hidden
    /// buffers. Those buffers exist only to carry the edit — they must be
    /// written to disk as part of the apply, or `:qa` throws the edits away
    /// ("Modified 5 files" → 4 files keep the old name on disk).
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn workspace_edit_writes_hidden_buffer_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        let hidden = dir.path().join("hidden.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");
        fs::write(&hidden, "fn helper() {}\n").expect("write hidden");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");

        let edit = changes_edit(file_uri(&hidden), vec![replace_edit(3, 9, "renamed")]);
        let applied = editor.apply_workspace_edit(edit).expect("apply");
        assert!(applied, "workspace edit should fully apply");

        assert_eq!(
            fs::read_to_string(&hidden).expect("read hidden"),
            "fn renamed() {}\n",
            "hidden buffer must be written through to disk"
        );
        assert!(
            !editor.any_buffer_modified(),
            "written-through hidden buffer must not linger as modified"
        );
    }

    /// OV-00331: buffers the user has open stay in-memory modified — saving
    /// is their call. Only hidden edit-carrier buffers get write-through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn workspace_edit_leaves_current_buffer_unwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");

        let edit = changes_edit(file_uri(&opened), vec![replace_edit(3, 7, "renamed")]);
        let applied = editor.apply_workspace_edit(edit).expect("apply");
        assert!(applied);

        assert_eq!(
            fs::read_to_string(&opened).expect("read opened"),
            "fn main() {}\n",
            "the user's open buffer must not be auto-saved"
        );
        assert!(editor.is_modified());
    }

    /// OV-00331: a hidden buffer that already carried the user's own unsaved
    /// changes must NOT be auto-saved — write-through would silently commit
    /// unrelated half-finished edits. The `:qa` guard covers it instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn workspace_edit_skips_write_through_for_user_modified_hidden_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        let hidden = dir.path().join("hidden.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");
        fs::write(&hidden, "fn helper() {}\n").expect("write hidden");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");

        let hidden_uri = file_uri(&hidden);
        let index = editor
            .find_or_load_buffer_index_by_uri(&hidden_uri)
            .expect("load hidden");
        editor.buffers[index].insert_text_at(
            0,
            crate::unicode::CharCol(0),
            "// user work in progress\n",
        );

        let edit = changes_edit(hidden_uri, vec![replace_edit(3, 9, "renamed")]);
        editor.apply_workspace_edit(edit).expect("apply");

        assert_eq!(
            fs::read_to_string(&hidden).expect("read hidden"),
            "fn helper() {}\n",
            "user-modified hidden buffer must not be auto-saved"
        );
        assert!(editor.any_buffer_modified());
    }

    /// OV-00330 (version-guard leg): a versioned document edit whose version
    /// no longer matches our view of the document is stale — the spec's
    /// staleness mechanism. It must be skipped, not spliced into newer text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stale_versioned_edit_is_discarded_with_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");
        editor.lsp.state.current_file_lsp_version = 3;

        let edit = versioned_edit(
            file_uri(&opened),
            Some(5),
            vec![replace_edit(3, 7, "stale")],
        );
        let applied = editor.apply_workspace_edit(edit).expect("apply");

        assert!(!applied, "stale versioned edit must not report success");
        assert_eq!(
            editor.buffer().rope().to_string(),
            "fn main() {}\n",
            "stale edit must not touch the buffer"
        );
        assert!(
            editor.lsp_status().contains("discarded: document changed"),
            "status must explain the discard, got: {:?}",
            editor.lsp_status()
        );
    }

    /// External review on OV-00331: write-through must never clobber a file
    /// that changed on disk after the hidden buffer was loaded. The edit
    /// stays in-memory modified, where the :qa guard protects it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn write_through_refuses_when_disk_changed_after_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        let target = dir.path().join("target.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");
        fs::write(&target, "fn helper() {}\n").expect("write target");

        let mut editor = Editor::default();
        editor.open_file(&target).expect("open target.rs");
        editor.open_file(&opened).expect("open opened.rs"); // target now hidden

        // External change lands after the buffer snapshot.
        fs::write(&target, "fn external_truth() {}\n").expect("external write");
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&target)
            .expect("open for mtime")
            .set_modified(newer)
            .expect("bump mtime");

        let edit = changes_edit(file_uri(&target), vec![replace_edit(3, 9, "renamed")]);
        let applied = editor.apply_workspace_edit(edit).expect("apply");

        assert!(!applied, "refused write-through must surface as failure");
        assert_eq!(
            fs::read_to_string(&target).expect("read target"),
            "fn external_truth() {}\n",
            "the external on-disk content must survive"
        );
        assert!(
            editor.any_buffer_modified(),
            "the unpersisted edit must keep the buffer modified so :qa protects it"
        );
    }

    /// External review on OV-00331/332: a partially-invalid TextEdit[] must
    /// never be persisted — dropping some edits and writing the rest to disk
    /// silently ships a half-applied change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn partial_apply_is_not_written_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        let target = dir.path().join("target.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");
        fs::write(&target, "fn helper() {}\n").expect("write target");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");

        let out_of_bounds = lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(99, 0),
                lsp_types::Position::new(99, 1),
            ),
            new_text: "nope".to_string(),
        };
        let edit = changes_edit(
            file_uri(&target),
            vec![replace_edit(3, 9, "renamed"), out_of_bounds],
        );
        let applied = editor.apply_workspace_edit(edit).expect("apply");

        assert!(!applied, "partial apply must report failure");
        assert_eq!(
            fs::read_to_string(&target).expect("read target"),
            "fn helper() {}\n",
            "a partial result must never reach disk"
        );
    }

    /// External review on OV-00330: a versioned edit arriving while local
    /// edits are still unsynced targets an older rope even when the version
    /// numbers match (the editor-side counter only advances on the next sync
    /// tick) — dirty sync state must reject it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn versioned_edit_is_discarded_while_sync_dirty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");
        editor.lsp.state.current_file_lsp_version = 5;

        // Local mutation marks sync dirty; the version counter hasn't
        // advanced yet — exactly the race window.
        editor
            .buffer_mut()
            .insert_text_at(0, crate::unicode::CharCol(0), "x");
        editor.mark_buffer_modified();

        let edit = versioned_edit(
            file_uri(&opened),
            Some(5),
            vec![replace_edit(4, 8, "stale")],
        );
        let applied = editor.apply_workspace_edit(edit).expect("apply");

        assert!(
            !applied,
            "matching version number must not bypass the dirty-sync rejection"
        );
        assert_eq!(
            editor.buffer().rope().to_string(),
            "xfn main() {}\n",
            "the newer local content must be untouched"
        );
    }

    /// OV-00330: matching and absent versions keep applying — the guard must
    /// not regress the common paths.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn matching_and_unversioned_edits_still_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = dir.path().join("opened.rs");
        fs::write(&opened, "fn main() {}\n").expect("write opened");

        let mut editor = Editor::default();
        editor.open_file(&opened).expect("open opened.rs");
        editor.lsp.state.current_file_lsp_version = 3;

        let edit = versioned_edit(
            file_uri(&opened),
            Some(3),
            vec![replace_edit(3, 7, "fresh")],
        );
        let applied = editor.apply_workspace_edit(edit).expect("apply");
        assert!(applied);
        assert_eq!(editor.buffer().rope().to_string(), "fn fresh() {}\n");

        let edit = versioned_edit(file_uri(&opened), None, vec![replace_edit(3, 8, "newer")]);
        let applied = editor.apply_workspace_edit(edit).expect("apply");
        assert!(applied);
        assert_eq!(editor.buffer().rope().to_string(), "fn newer() {}\n");
    }
}
