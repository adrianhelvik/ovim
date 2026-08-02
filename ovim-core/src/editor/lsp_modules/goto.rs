//! LSP go-to operations
//!
//! This module handles go-to-definition, go-to-implementation, and go-to-type-definition.
//! These are fundamental LSP navigation features (triggered by 'gd', 'gi', 'gy' in Vim).

use super::super::Editor;
use crate::editor::lsp_slot::GotoLocationResult;
use crate::lsp::uri_from_file_path;
use anyhow::Result;

impl Editor {
    /// Request go-to-definition at current cursor position
    pub fn request_goto_definition(&mut self) {
        self.lsp.intents.goto_definition = true;
    }

    /// Request go-to-definition at current cursor position, opening in a new tab
    pub fn request_goto_definition_new_tab(&mut self) {
        self.lsp.intents.goto_definition_new_tab = true;
    }

    /// Request go-to-implementation at current cursor position
    pub fn request_goto_implementation(&mut self) {
        self.lsp.intents.goto_implementation = true;
    }

    /// Request go-to-implementation at current cursor position, opening in a new tab
    pub fn request_goto_implementation_new_tab(&mut self) {
        self.lsp.intents.goto_implementation_new_tab = true;
    }

    /// Request go-to-type-definition at current cursor position
    pub fn request_goto_type(&mut self) {
        self.lsp.intents.goto_type = true;
    }

    /// Shared setup for goto requests: validates LSP state, resolves file path
    /// and cursor position, ensures document is synced.  Returns the pieces
    /// needed to spawn the actual LSP request, or `None` (with a status message
    /// already set) if a precondition wasn't met.
    async fn prepare_goto_request(&mut self, feature_name: &str) -> Option<GotoPrepared> {
        let lsp = match &self.lsp.state.lsp_manager {
            Some(lsp) => lsp.clone(),
            None => {
                self.set_lsp_status("LSP not available".to_string());
                return None;
            }
        };

        let Some(file_path) = self.buffer().file_path().map(|p| p.to_string()) else {
            self.set_lsp_status(format!("Save file first to use {}", feature_name));
            return None;
        };

        let abs_path = if std::path::Path::new(&file_path).is_absolute() {
            file_path.clone()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(&file_path).to_string_lossy().to_string(),
                Err(_) => {
                    self.set_lsp_status("Failed to resolve file path".to_string());
                    return None;
                }
            }
        };

        let uri = match uri_from_file_path(&abs_path) {
            Some(u) => u,
            None => {
                self.set_lsp_status("Invalid file path".to_string());
                return None;
            }
        };

        let cursor = self.buffer().cursor();
        let line = cursor.line() as u32;
        let character = self.col_to_utf16(cursor.line(), cursor.col().0);

        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => {
                self.set_lsp_status("Language not supported for LSP".to_string());
                return None;
            }
        };

        self.ensure_lsp_document_synced().await;

        let server_ids = lsp.servers_for_document(&language_id, std::path::Path::new(&file_path));

        Some(GotoPrepared {
            lsp,
            uri,
            line,
            character,
            language_id,
            server_ids,
        })
    }

    /// Implementation of goto-definition (optionally in a new tab)
    async fn goto_definition_common(&mut self, new_tab: bool) -> Result<bool> {
        let Some(p) = self.prepare_goto_request("goto-definition").await else {
            return Ok(false);
        };

        crate::lsp_debug!(
            "LSP-REQUEST",
            "goto_definition: line={}, char={}, uri={:?}",
            p.line,
            p.character,
            p.uri
        );

        let lsp = p.lsp;
        let uri = p.uri;
        let line = p.line;
        let character = p.character;
        let language_id = p.language_id;
        let server_ids = p.server_ids;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = if server_ids.len() > 1 {
                lsp.goto_definition_multi(&uri, line, character, &server_ids)
                    .await
            } else {
                lsp.goto_definition(&uri, line, character, &language_id)
                    .await
            };
            let _ = tx.send(result.map(|loc| GotoLocationResult {
                location: loc,
                new_tab,
            }));
        });

        self.lsp.slots.goto_definition.fire(task, rx);
        self.set_lsp_status("Jumping to definition...".to_string());

        Ok(false)
    }

    /// Implementation of goto-definition (same buffer)
    pub(in crate::editor) async fn goto_definition_impl(&mut self) -> Result<bool> {
        self.goto_definition_common(false).await
    }

    /// Implementation of goto-definition (new tab)
    pub(in crate::editor) async fn goto_definition_new_tab_impl(&mut self) -> Result<bool> {
        self.goto_definition_common(true).await
    }

    /// Implementation of goto-implementation (optionally in a new tab)
    async fn goto_implementation_common(&mut self, new_tab: bool) -> Result<bool> {
        let Some(p) = self.prepare_goto_request("goto-implementation").await else {
            return Ok(false);
        };

        let lsp = p.lsp;
        let uri = p.uri;
        let line = p.line;
        let character = p.character;
        let language_id = p.language_id;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = lsp
                .implementation(&uri, line, character, &language_id)
                .await;
            let _ = tx.send(result.map(|loc| GotoLocationResult {
                location: loc,
                new_tab,
            }));
        });

        self.lsp.slots.goto_implementation.fire(task, rx);
        self.set_lsp_status("Jumping to implementation...".to_string());

        Ok(false)
    }

    /// Implementation of goto-implementation (same buffer)
    pub(in crate::editor) async fn goto_implementation_impl(&mut self) -> Result<bool> {
        self.goto_implementation_common(false).await
    }

    /// Implementation of goto-implementation (new tab)
    pub(in crate::editor) async fn goto_implementation_new_tab_impl(&mut self) -> Result<bool> {
        self.goto_implementation_common(true).await
    }

    /// Implementation of goto-type-definition
    pub(in crate::editor) async fn goto_type_impl(&mut self) -> Result<bool> {
        let Some(p) = self.prepare_goto_request("goto-type").await else {
            return Ok(false);
        };

        let lsp = p.lsp;
        let uri = p.uri;
        let line = p.line;
        let character = p.character;
        let language_id = p.language_id;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = lsp
                .type_definition(&uri, line, character, &language_id)
                .await;
            let _ = tx.send(result.map(|loc| GotoLocationResult {
                location: loc,
                new_tab: false,
            }));
        });

        self.lsp.slots.goto_type_definition.fire(task, rx);
        self.set_lsp_status("Jumping to type definition...".to_string());

        Ok(false)
    }
}

/// Intermediate struct holding everything needed to fire a goto LSP request.
struct GotoPrepared {
    lsp: std::sync::Arc<crate::lsp::LspManager>,
    uri: lsp_types::Uri,
    line: u32,
    character: u32,
    language_id: String,
    server_ids: Vec<String>,
}
