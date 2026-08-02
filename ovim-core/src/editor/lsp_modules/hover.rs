//! LSP hover functionality
//!
//! This module handles hover information display (the "K" command in Vim).
//! It includes hover request, caching, scrolling, and display management.

use super::super::Editor;
use crate::lsp::uri_from_file_path;
use anyhow::{anyhow, Result};

impl Editor {
    /// Request hover info at current cursor position
    pub fn request_hover(&mut self) {
        self.lsp.intents.hover = true;
    }

    /// Get current hover info text
    pub fn hover_info(&self) -> Option<&str> {
        self.lsp.state.hover_info.as_deref()
    }

    /// Get current hover content type (LSP hover or diagnostic)
    pub fn hover_content_type(&self) -> crate::editor::lsp_state::HoverContentType {
        self.lsp.state.hover_content_type
    }

    /// Clear hover info
    pub fn clear_hover(&mut self) {
        self.lsp.state.hover_info = None;
        self.lsp.state.hover_scroll = 0;
        self.lsp.state.hover_h_scroll = 0;
    }

    /// Set hover info directly (used for command output display)
    /// Also switches to HoverPreview mode so the popup is visible
    pub fn set_hover_info(&mut self, info: String) {
        self.lsp.state.hover_info = Some(info);
        self.lsp.state.hover_scroll = 0;
        self.lsp.state.hover_h_scroll = 0;
        self.lsp.state.hover_content_type = crate::editor::lsp_state::HoverContentType::LspHover;
        self.mode = crate::mode::Mode::HoverPreview;
        self.mark_dirty();
    }

    /// Get hover scroll position
    pub fn hover_scroll(&self) -> usize {
        self.lsp.state.hover_scroll
    }

    /// Get the cursor position where hover was triggered
    pub fn hover_position(&self) -> Option<(usize, usize)> {
        self.lsp.state.hover_position
    }

    /// Scroll hover window down
    pub fn scroll_hover_down(&mut self, lines: usize) {
        if self.lsp.state.hover_info.is_some() {
            self.lsp.state.hover_scroll = self.lsp.state.hover_scroll.saturating_add(lines);
        }
    }

    /// Scroll hover window up
    pub fn scroll_hover_up(&mut self, lines: usize) {
        self.lsp.state.hover_scroll = self.lsp.state.hover_scroll.saturating_sub(lines);
    }

    /// Get hover horizontal scroll position
    pub fn hover_h_scroll(&self) -> usize {
        self.lsp.state.hover_h_scroll
    }

    /// Scroll hover window right
    pub fn scroll_hover_right(&mut self, cols: usize) {
        if self.lsp.state.hover_info.is_some() {
            self.lsp.state.hover_h_scroll = self.lsp.state.hover_h_scroll.saturating_add(cols);
        }
    }

    /// Scroll hover window left
    pub fn scroll_hover_left(&mut self, cols: usize) {
        self.lsp.state.hover_h_scroll = self.lsp.state.hover_h_scroll.saturating_sub(cols);
    }

    /// Implementation of hover request
    pub(in crate::editor) async fn hover_impl(&mut self) -> Result<bool> {
        crate::lsp_debug!("LSP-HOVER", "hover_impl() called");
        let lsp = match &self.lsp.state.lsp_manager {
            Some(lsp) => {
                crate::lsp_debug!("LSP-HOVER", "LSP manager found, cloning Arc");
                lsp.clone()
            }
            None => {
                crate::lsp_debug!("LSP-HOVER", "No LSP manager in hover_impl");
                self.set_lsp_status("LSP not available".to_string());
                return Ok(false);
            }
        };

        let Some(file_path_str) = self.buffer().file_path() else {
            self.set_lsp_status("Save file first to use hover".to_string());
            return Ok(false);
        };
        let file_path = file_path_str.to_string();

        // Check hover cache first
        let cursor = self.buffer().cursor();
        let buffer_version = self.buffer().version();
        let cursor_line = cursor.line();
        let cursor_col = cursor.col().0;

        if let Some(ref cache) = self.lsp.state.hover_cache {
            if cache.is_valid(&file_path, cursor_line, cursor_col, buffer_version) {
                crate::lsp_info!("LSP-HOVER", "Cache HIT");
                self.lsp.state.hover_info = Some(cache.hover_text.clone());
                self.lsp.state.hover_scroll = 0;
                self.lsp.state.hover_h_scroll = 0;
                self.lsp.state.hover_position = Some((cursor_line, cursor_col));
                self.lsp.state.hover_content_type =
                    crate::editor::lsp_state::HoverContentType::LspHover;
                self.mode = crate::mode::Mode::HoverPreview;
                self.mark_dirty();
                self.set_lsp_status(String::new());
                return Ok(true);
            }
        }

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
        let character = self.col_to_utf16(cursor.line(), cursor.col().0);

        let language_id = match self.language_id_for_path(&file_path) {
            Some(id) => id,
            None => {
                self.set_lsp_status("Language not supported for LSP".to_string());
                return Ok(false);
            }
        };

        crate::lsp_debug!(
            "LSP-HOVER",
            "Requesting hover: file={}, line={}, col={}, char={}, uri={:?}",
            file_path,
            line,
            cursor.col(),
            character,
            uri
        );

        // Ensure document is synced before making the request
        self.ensure_lsp_document_synced().await;

        // Resolve the server group responsible for this document
        let server_ids = lsp.servers_for_document(&language_id, std::path::Path::new(&file_path));

        // Spawn hover request in background (non-blocking)
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = if server_ids.len() > 1 {
                lsp.hover_multi(&uri, line, character, &server_ids).await
            } else {
                lsp.hover(&uri, line, character, &language_id).await
            };
            let _ = tx
                .send(result.map(|text| crate::editor::lsp_slot::HoverResult { hover_text: text }));
        });

        // Fire into the hover slot (cancels any previous in-flight hover)
        self.lsp.slots.hover.fire(task, rx);

        self.set_lsp_status("Loading hover...".to_string());
        crate::lsp_debug!("LSP-HOVER", "Spawned hover request, waiting for response");

        Ok(false)
    }
}
