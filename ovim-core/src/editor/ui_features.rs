//! UI features: completion menu, file tree, quickfix, location list, substitute confirmation

use super::{
    CompletionMenu, CursorPos, Editor, FileTree, InsertEntryMode, LocationList, Mode,
    PathCompletionState, QuickfixEntry, QuickfixList,
};
use crate::unicode::{CharCol, GraphemeCol};

impl Editor {
    /// Gets a reference to the completion menu
    pub fn completion_menu(&self) -> &CompletionMenu {
        &self.completion_menu
    }

    /// Gets a mutable reference to the completion menu
    pub fn completion_menu_mut(&mut self) -> &mut CompletionMenu {
        &mut self.completion_menu
    }

    /// Gets a reference to the path completion state
    pub fn path_completion(&self) -> &PathCompletionState {
        &self.ui_panels.path_completion
    }

    /// Gets a mutable reference to the path completion state
    pub fn path_completion_mut(&mut self) -> &mut PathCompletionState {
        &mut self.ui_panels.path_completion
    }

    /// Hides the completion menu
    pub fn hide_completion_menu(&mut self) {
        self.completion_menu.hide();
    }

    /// Selects the next completion item
    pub fn completion_next(&mut self) {
        self.completion_menu.select_next();
    }

    /// Selects the previous completion item
    pub fn completion_previous(&mut self) {
        self.completion_menu.select_previous();
    }

    /// Accepts the currently selected completion from the menu
    pub fn accept_completion(&mut self) {
        if let Some(item) = self.completion_menu.selected_item().cloned() {
            self.accept_completion_item(&item);
        }
        self.hide_completion_menu();
    }

    /// Accepts a completion by index from available_completions (used by picker)
    pub fn accept_completion_at(&mut self, index: usize) {
        if let Some(item) = self.lsp.state.available_completions.get(index).cloned() {
            self.accept_completion_item(&item);
        }
        self.lsp.state.available_completions.clear();
        self.hide_completion_menu();
    }

    /// Core completion application: uses textEdit range when available,
    /// falls back to trigger_col..cursor deletion.
    fn accept_completion_item(&mut self, item: &lsp_types::CompletionItem) {
        use crate::unicode::CharCol;

        // The item's textEdit range indexes the buffer snapshot the server
        // responded to. If the buffer has been edited since (user kept
        // typing while the menu was open — the menu only re-filters locally
        // during the LSP round trip), applying that range verbatim splices
        // at shifted offsets: deleted text survives, or the edit eats
        // neighboring characters. Fall back to trigger_col..cursor
        // replacement in that case; trigger_col tracks the live buffer
        // (OV-00327).
        let ranges_are_current = self
            .completion_menu
            .items_buffer_version()
            .is_some_and(|v| v == self.buffer().version());

        // Extract the main edit: prefer textEdit (with proper range) over
        // insertText/label (which only knows about the trigger column).
        let (
            replace_start_line,
            replace_start_col,
            replace_end_line,
            replace_end_col,
            text_to_insert,
        ): (usize, CharCol, usize, CharCol, String) = if let Some(text_edit) =
            item.text_edit.as_ref().filter(|_| ranges_are_current)
        {
            let (range, new_text) = match text_edit {
                lsp_types::CompletionTextEdit::Edit(edit) => (edit.range, edit.new_text.clone()),
                lsp_types::CompletionTextEdit::InsertAndReplace(ir) => {
                    // On accept/confirm, use the replace range (broader)
                    (ir.replace, ir.new_text.clone())
                }
            };
            let start_line = range.start.line as usize;
            let start_col = self.utf16_to_col(start_line, range.start.character);
            let end_line = range.end.line as usize;
            let end_col = self.utf16_to_col(end_line, range.end.character);
            (start_line, start_col, end_line, end_col, new_text)
        } else {
            // Fallback: delete trigger..cursor and insert insertText/label
            let cursor_line = self.buffer().cursor().line();
            let cursor_col = self.buffer().cursor_char_col();
            // trigger_col is stored as bare usize in CompletionMenu (char col).
            let trigger_col = CharCol(self.completion_menu.trigger_col());
            let text = if let Some(ref insert_text) = item.insert_text {
                insert_text.clone()
            } else {
                item.label.clone()
            };
            (cursor_line, trigger_col, cursor_line, cursor_col, text)
        };

        // Normalize CR variants in the LSP-supplied insertion text so the
        // rope stays LF-only and the cursor-positioning logic below (which
        // splits on `\n`) sees the canonical form (OV-00251).
        let text_to_insert = if text_to_insert.contains('\r') {
            crate::buffer::normalize_for_buffer(&text_to_insert).into_owned()
        } else {
            text_to_insert
        };

        // Collect additional text edits (e.g., auto-imports) before mutating.
        // Skipped when stale: their ranges target the same outdated snapshot
        // as the main textEdit (OV-00327).
        let additional_edits: Vec<lsp_types::TextEdit> = if ranges_are_current {
            item.additional_text_edits.clone().unwrap_or_default()
        } else {
            Vec::new()
        };

        let cursor_line = self.buffer().cursor().line();
        let cursor_col = self.buffer().cursor().col().0;
        let cursor_before = CursorPos::new(cursor_line, GraphemeCol(cursor_col));

        // Break the insert-mode undo here, Vim-style: finalize whatever the
        // user typed before accepting completion as its own `Recorded` entry,
        // then restart the session after completion edits land. Pause/resume
        // would retain pre-completion edits whose absolute char offsets no
        // longer match the rope once the completion text is inserted, and
        // concatenating them with post-completion edits in a single Recorded
        // corrupts the buffer on undo (and again on redo).
        let restart_entry_mode: Option<InsertEntryMode> = if self.buffer().is_recording() {
            let entry_mode = self
                .buffer()
                .change_manager()
                .current_builder
                .as_ref()
                .map(|b| b.entry_mode().clone());
            self.finalize_change_building();
            entry_mode
        } else {
            None
        };

        let ((), edits) = self.buffer_mut().record(|buf| {
            // Apply main completion edit
            if replace_start_line != replace_end_line || replace_start_col != replace_end_col {
                buf.delete_range(
                    replace_start_line,
                    replace_start_col,
                    replace_end_line,
                    replace_end_col,
                );
            }
            if !text_to_insert.is_empty() {
                buf.insert_text_at(replace_start_line, replace_start_col, &text_to_insert);
            }

            // Position cursor at the end of inserted text
            let insert_lines: Vec<&str> = text_to_insert.split('\n').collect();
            let (end_line, end_col): (usize, CharCol) = if insert_lines.len() > 1 {
                (
                    replace_start_line + insert_lines.len() - 1,
                    CharCol(insert_lines.last().map_or(0, |l| l.chars().count())),
                )
            } else {
                (
                    replace_start_line,
                    replace_start_col + text_to_insert.chars().count(),
                )
            };
            buf.set_cursor_char_col(end_line, end_col);
        });
        if !edits.is_empty() {
            let cursor_after = self.cursor_position();
            self.push_recorded_undo(edits, cursor_before, cursor_after);
        }

        // Apply additional text edits (auto-imports, etc.) in reverse order
        // to avoid position invalidation
        if !additional_edits.is_empty() {
            // Pre-compute UTF-16 → char column conversions before mutating
            let mut converted: Vec<(usize, CharCol, usize, CharCol, String)> = additional_edits
                .iter()
                .map(|edit| {
                    let sl = edit.range.start.line as usize;
                    let sc = self.utf16_to_col(sl, edit.range.start.character);
                    let el = edit.range.end.line as usize;
                    let ec = self.utf16_to_col(el, edit.range.end.character);
                    (sl, sc, el, ec, edit.new_text.clone())
                })
                .collect();
            // Sort reverse (bottom-to-top) to avoid position invalidation
            converted.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

            let cursor_before = self.cursor_position();
            let ((), edits) = self.buffer_mut().record(|buf| {
                for (sl, sc, el, ec, new_text) in &converted {
                    if sl != el || sc != ec {
                        buf.delete_range(*sl, *sc, *el, *ec);
                    }
                    if !new_text.is_empty() {
                        let normalized = crate::buffer::normalize_for_buffer(new_text);
                        buf.insert_text_at(*sl, *sc, normalized.as_ref());
                    }
                }
            });
            if !edits.is_empty() {
                let cursor_after = self.cursor_position();
                self.push_recorded_undo(edits, cursor_before, cursor_after);
            }
        }

        // Restart the insert-mode session so any further typing forms a fresh
        // Recorded entry at post-completion offsets, decoupled from anything
        // that was finalized above.
        if let Some(entry_mode) = restart_entry_mode {
            let cursor_after = self.cursor_position();
            self.start_change_building(cursor_after);
            self.set_change_entry_mode(entry_mode);
        }
    }

    /// Gets the inlay hints for the current file
    pub fn inlay_hints(&self) -> &[lsp_types::InlayHint] {
        &self.lsp.state.inlay_hints
    }

    /// Gets the file tree
    pub fn file_tree(&self) -> &FileTree {
        &self.ui_panels.file_tree
    }

    /// Gets mutable file tree
    pub fn file_tree_mut(&mut self) -> &mut FileTree {
        &mut self.ui_panels.file_tree
    }

    /// Open a directory as an explorer workspace without assigning the
    /// directory path to the current text buffer.
    pub fn open_directory(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        let root = path
            .canonicalize()
            .map_err(|error| anyhow::anyhow!("Could not open directory: {error}"))?;
        if !root.is_dir() {
            anyhow::bail!("Not a directory: {}", path.display());
        }
        self.ui_panels.file_tree.open(&root);
        self.set_mode(Mode::FileTree);
        Ok(())
    }

    /// Opens the file tree explorer at the project root
    pub fn open_file_tree(&mut self) {
        use crate::language_config::find_project_root;
        use git2::Repository;

        let file_path = self.buffer().file_path().map(|s| s.to_string());

        let root = if let Some(ref file_path) = file_path {
            let path = std::path::Path::new(file_path);

            // Try git root first (most reliable for project boundary)
            if let Ok(repo) = Repository::discover(path) {
                if let Some(workdir) = repo.workdir() {
                    workdir.to_path_buf()
                } else {
                    // Fallback: use language-specific markers
                    find_project_root(
                        path,
                        &["Cargo.toml".into(), "package.json".into(), ".git".into()],
                    )
                }
            } else {
                // Not in git repo - use language markers or parent
                find_project_root(path, &["Cargo.toml".into(), "package.json".into()])
            }
        } else {
            std::env::current_dir().unwrap_or_default()
        };

        self.ui_panels.file_tree.open(&root);
    }

    /// Toggles the file tree with reveal semantics:
    /// - Tree closed → open, reveal current file, enter FileTree mode
    /// - Tree open + buffer focused (Normal) → reveal current file, enter FileTree mode
    /// - Tree open + tree focused (FileTree) → close tree, enter Normal mode
    pub fn toggle_file_tree(&mut self) {
        if self.mode() == Mode::FileTree {
            // Focused on tree → close it
            self.ui_panels.file_tree.close();
            self.set_mode(Mode::Normal);
        } else {
            // Not focused → open/reveal + focus
            if !self.ui_panels.file_tree.is_visible() {
                self.open_file_tree();
            }
            if self.options.file_tree_reveal {
                if let Some(path) = self.buffer().file_path().map(|s| s.to_string()) {
                    self.ui_panels
                        .file_tree
                        .reveal_path(std::path::Path::new(&path));
                }
            }
            self.set_mode(Mode::FileTree);
        }
    }

    /// Opens the file selected in the file tree while keeping the docked tree
    /// visible, or toggles directory expansion.
    pub fn open_file_from_tree(&mut self) {
        if let Some(node) = self.ui_panels.file_tree.selected_node() {
            if node.is_dir() {
                // Toggle directory expansion
                self.ui_panels.file_tree.toggle_selected();
            } else {
                // Open file (checks for existing buffer)
                let path = node.path().to_path_buf();
                if let Ok(()) = self.open_file(&path) {
                    self.set_mode(Mode::Normal);
                    self.ui_panels.file_tree.close();
                }
            }
        }
    }

    /// Gets the quickfix list
    pub fn quickfix_list(&self) -> &QuickfixList {
        &self.ui_panels.quickfix_list
    }

    /// Gets mutable quickfix list
    pub fn quickfix_list_mut(&mut self) -> &mut QuickfixList {
        &mut self.ui_panels.quickfix_list
    }

    /// Sets the quickfix list entries
    pub fn set_quickfix_list(&mut self, entries: Vec<QuickfixEntry>, title: String) {
        self.ui_panels.quickfix_list.set_entries(entries, title);
    }

    /// Opens the quickfix window
    pub fn open_quickfix_window(&mut self) {
        self.ui_panels.quickfix_window_open = true;
    }

    /// Closes the quickfix window
    pub fn close_quickfix_window(&mut self) {
        self.ui_panels.quickfix_window_open = false;
    }

    /// Toggles the quickfix window
    pub fn toggle_quickfix_window(&mut self) {
        self.ui_panels.quickfix_window_open = !self.ui_panels.quickfix_window_open;
    }

    /// Whether the quickfix window is open
    pub fn is_quickfix_window_open(&self) -> bool {
        self.ui_panels.quickfix_window_open
    }

    /// Jumps to the current quickfix entry
    pub fn jump_to_quickfix_entry(&mut self) {
        // Extract values first to avoid borrow issues
        let (path, lnum, qcol) = if let Some(entry) = self.ui_panels.quickfix_list.current_entry() {
            (entry.filename.clone(), entry.lnum, entry.col)
        } else {
            return;
        };

        if let Some(path) = path {
            // Open file (checks for existing buffer)
            if let Ok(()) = self.open_file(&path) {
                // Move cursor to the location
                if lnum > 0 {
                    let line = lnum.saturating_sub(1);
                    let col = if qcol > 0 { qcol.saturating_sub(1) } else { 0 };
                    self.buffer_mut()
                        .cursor_mut()
                        .set_position(line, GraphemeCol(col));
                }
            }
        }
    }

    /// Sets a pending `:make` background job.
    pub fn set_pending_make(&mut self, pending: super::PendingMake) {
        self.build.pending_make = Some(pending);
    }

    /// Polls for a completed `:make` job. Returns true if results were applied.
    pub fn poll_pending_make(&mut self) -> bool {
        let pending = match self.build.pending_make.take() {
            Some(p) => p,
            None => return false,
        };

        match pending.receiver.try_recv() {
            Ok(result) => {
                // Store raw output for :TestOutput / :MakeOutput
                self.build.last_make_output = Some(result.output.clone());

                let entries = crate::commands::parse_compiler_output(&result.output);
                let entry_count = entries.len();
                let title = format!(":make {}", pending.command);
                self.set_quickfix_list(entries, title);

                if entry_count > 0 {
                    self.ui_panels.quickfix_list.first();
                    self.jump_to_quickfix_entry();
                    self.open_quickfix_window();
                    self.set_status_message(format!("{} error(s)/warning(s)", entry_count));
                } else if result.success {
                    self.close_quickfix_window();
                    self.set_status_message("Build succeeded — no errors".to_string());
                } else {
                    self.set_status_message("Build failed (no parseable errors)".to_string());
                }
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Still running — put it back
                self.build.pending_make = Some(pending);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.set_status_message("Make job failed (thread panicked)".to_string());
                true
            }
        }
    }

    /// Drains completed background git refresh results. Returns true if any applied.
    pub fn poll_git_refresh(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.git_refresh_rx.try_recv() {
            // Apply to the buffer whose file path matches the refresh result.
            let matching = self
                .buffers
                .iter()
                .position(|b| b.file_path() == Some(&result.path));
            if let Some(idx) = matching {
                if let Some(status) = result.status {
                    self.buffers[idx].set_git_status(status);
                }
                if let Some(blame) = result.blame {
                    self.buffers[idx].set_git_blame(blame);
                }
                changed = true;
            }
        }
        changed
    }

    /// Spawns a background git status (and optionally blame) refresh for `path`.
    pub fn spawn_git_refresh(&self, path: &str, blame_enabled: bool) {
        let path = path.to_string();
        let tx = self.git_refresh_tx.clone();
        tokio::task::spawn_blocking(move || {
            let status = crate::git::GitStatus::from_file(&path).ok();
            let blame = if blame_enabled {
                crate::git::GitBlame::from_file(&path)
                    .ok()
                    .filter(|b| !b.is_empty())
            } else {
                None
            };
            let _ = tx.blocking_send(super::GitRefreshResult {
                path,
                status,
                blame,
            });
        });
    }

    /// Gets the location list
    pub fn location_list(&self) -> &LocationList {
        &self.ui_panels.location_list
    }

    /// Gets mutable location list
    pub fn location_list_mut(&mut self) -> &mut LocationList {
        &mut self.ui_panels.location_list
    }

    /// Sets the location list entries
    pub fn set_location_list(&mut self, entries: Vec<QuickfixEntry>, title: String) {
        self.ui_panels.location_list.set_entries(entries, title);
    }

    /// Opens the location list window
    pub fn open_location_window(&mut self) {
        self.ui_panels.location_window_open = true;
    }

    /// Closes the location list window
    pub fn close_location_window(&mut self) {
        self.ui_panels.location_window_open = false;
    }

    /// Toggles the location list window
    pub fn toggle_location_window(&mut self) {
        self.ui_panels.location_window_open = !self.ui_panels.location_window_open;
    }

    /// Whether the location list window is open
    pub fn is_location_window_open(&self) -> bool {
        self.ui_panels.location_window_open
    }

    /// Jumps to the current location list entry
    pub fn jump_to_location_entry(&mut self) {
        // Extract values first to avoid borrow issues
        let (path, lnum, lcol) = if let Some(entry) = self.ui_panels.location_list.current_entry() {
            (entry.filename.clone(), entry.lnum, entry.col)
        } else {
            return;
        };

        if let Some(path) = path {
            // Open file (checks for existing buffer)
            if let Ok(()) = self.open_file(&path) {
                // Move cursor to the location
                if lnum > 0 {
                    let line = lnum.saturating_sub(1);
                    let col = if lcol > 0 { lcol.saturating_sub(1) } else { 0 };
                    self.buffer_mut()
                        .cursor_mut()
                        .set_position(line, GraphemeCol(col));
                }
            }
        }
    }

    // ========== Substitute Confirmation ==========

    /// Starts substitute confirmation mode with the given matches
    pub fn start_substitute_confirm(
        &mut self,
        matches: Vec<(usize, usize, usize, String)>,
        pattern: regex::Regex,
    ) {
        self.editing.substitute_matches = matches;
        self.editing.substitute_match_index = 0;
        self.editing.substitute_pattern = Some(pattern);
        if !self.editing.substitute_matches.is_empty() {
            self.mode = Mode::SubstituteConfirm;
            // Move cursor to first match
            let (line, col, _, _) = self.editing.substitute_matches[0];
            self.buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
    }

    /// Gets the current substitute match info (line, start_col, end_col, replacement)
    pub fn current_substitute_match(&self) -> Option<&(usize, usize, usize, String)> {
        self.editing
            .substitute_matches
            .get(self.editing.substitute_match_index)
    }

    /// Gets the substitute pattern for highlighting
    pub fn substitute_pattern(&self) -> Option<&regex::Regex> {
        self.editing.substitute_pattern.as_ref()
    }

    /// Confirms the current substitution and moves to the next
    pub fn confirm_substitute(&mut self) {
        if let Some((line, start_col, end_col, replacement)) = self
            .editing
            .substitute_matches
            .get(self.editing.substitute_match_index)
            .cloned()
        {
            // Perform the substitution
            let cursor_before = self.cursor_position();
            let ((), edits) = self.buffer_mut().record(|buf| {
                // Always perform delete + insert for confirmed matches so undo
                // round-trips exactly what the user confirmed.
                // Phase-15 debt: substitute_matches tuple stores char cols.
                buf.delete_range(line, CharCol(start_col), line, CharCol(end_col));
                if !replacement.is_empty() {
                    buf.insert_text_at(line, CharCol(start_col), &replacement);
                }
            });
            if !edits.is_empty() {
                let cursor_after = self.cursor_position();
                self.push_recorded_undo(edits, cursor_before, cursor_after);
            }

            // Matches were collected as absolute offsets against the ORIGINAL
            // buffer. A confirmed replacement invalidates the later offsets two
            // ways — without adjustment the next replacement lands at a stale
            // position and corrupts the buffer:
            // - same-line, same length class: later matches on the line shift by
            //   the length delta (e.g. `:s/a/XX/gc` on "aaa");
            // - multi-line replacement (`\r`): the rest of the line moves onto a
            //   new line `newlines` further down, and every later line shifts
            //   down by `newlines` as well.
            let next = self.editing.substitute_match_index + 1;
            let newlines = replacement.matches('\n').count();
            if newlines == 0 {
                let delta =
                    replacement.chars().count() as isize - (end_col as isize - start_col as isize);
                if delta != 0 {
                    for m in self.editing.substitute_matches.iter_mut().skip(next) {
                        if m.0 == line {
                            m.1 = (m.1 as isize + delta).max(0) as usize;
                            m.2 = (m.2 as isize + delta).max(0) as usize;
                        }
                    }
                }
            } else {
                // Chars after the last '\n' — the prefix now preceding the
                // remainder of the original line on its new (final) line.
                let last_seg_len = replacement.chars().rev().take_while(|&c| c != '\n').count();
                for m in self.editing.substitute_matches.iter_mut().skip(next) {
                    if m.0 == line {
                        // Later matches on the confirmed line sit after end_col;
                        // they land on the replacement's final line.
                        let len = m.2 - m.1;
                        m.0 = line + newlines;
                        m.1 = last_seg_len + m.1.saturating_sub(end_col);
                        m.2 = m.1 + len;
                    } else if m.0 > line {
                        m.0 += newlines;
                    }
                }
            }

            self.editing.substitute_match_index += 1;
            if self.editing.substitute_match_index >= self.editing.substitute_matches.len() {
                self.end_substitute_confirm();
            } else {
                // Move cursor to next match
                let (next_line, next_col, _, _) =
                    self.editing.substitute_matches[self.editing.substitute_match_index];
                self.buffer_mut()
                    .cursor_mut()
                    .set_position(next_line, GraphemeCol(next_col));
            }
        }
    }

    /// Skips the current match and moves to the next
    pub fn skip_substitute(&mut self) {
        self.editing.substitute_match_index += 1;
        if self.editing.substitute_match_index >= self.editing.substitute_matches.len() {
            self.end_substitute_confirm();
        } else {
            // Move cursor to next match
            let (line, col, _, _) =
                self.editing.substitute_matches[self.editing.substitute_match_index];
            self.buffer_mut()
                .cursor_mut()
                .set_position(line, GraphemeCol(col));
        }
    }

    /// Confirms all remaining substitutions
    pub fn confirm_all_substitutes(&mut self) {
        while self.editing.substitute_match_index < self.editing.substitute_matches.len() {
            self.confirm_substitute();
        }
    }

    /// Confirms current and quits
    pub fn confirm_substitute_and_quit(&mut self) {
        self.confirm_substitute();
        self.end_substitute_confirm();
    }

    /// Ends substitute confirmation mode
    pub fn end_substitute_confirm(&mut self) {
        self.editing.substitute_matches.clear();
        self.editing.substitute_match_index = 0;
        self.editing.substitute_pattern = None;
        self.mode = Mode::Normal;
    }

    // ==================== LSP Manager Panel ====================

    pub fn lsp_manager_panel(&self) -> Option<&super::LspManagerPanel> {
        self.lsp.ui.lsp_manager_panel.as_ref()
    }

    pub fn lsp_manager_panel_mut(&mut self) -> Option<&mut super::LspManagerPanel> {
        self.lsp.ui.lsp_manager_panel.as_mut()
    }

    pub fn open_lsp_manager(&mut self) {
        let running = self.get_running_lsp_servers();
        self.lsp.ui.lsp_manager_panel = Some(super::LspManagerPanel::new(running));
        self.mode = Mode::LspManager;
        // Ensure install channel exists
        if self.lsp.ui.install_progress_tx.is_none() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            self.lsp.ui.install_progress_tx = Some(tx);
            self.lsp.ui.install_progress_rx = Some(rx);
        }
    }

    pub fn close_lsp_manager(&mut self) {
        self.lsp.ui.lsp_manager_panel = None;
        self.mode = Mode::Normal;
    }

    /// Trigger LSP server install for a language.
    /// Enqueues a pending install request to be picked up by the event loop.
    pub fn request_lsp_install(&mut self, language_id: &str) {
        use super::lsp_manager_panel::{InstallStatus, PendingInstallRequest};
        use crate::language_config::LanguageRegistry;

        let Some(registry) = LanguageRegistry::try_get() else {
            self.set_status_message("Language registry not initialized".to_string());
            return;
        };

        let Some(lang) = registry.get_by_id(language_id) else {
            self.set_status_message(format!("Unknown language: {language_id}"));
            return;
        };

        let Some(lsp) = &lang.lsp else {
            self.set_status_message(format!("No LSP configured for {}", lang.name));
            return;
        };

        let Some(auto_install) = &lsp.auto_install else {
            if let Some(hint) = &lsp.install_hint {
                self.set_status_message(hint.clone());
            } else {
                self.set_status_message(format!("No install method for {}", lang.name));
            }
            return;
        };

        // Set installing status in panel
        if let Some(panel) = &mut self.lsp.ui.lsp_manager_panel {
            panel.active_installs.insert(
                language_id.to_string(),
                InstallStatus::Installing("Starting...".to_string()),
            );
        }

        // Queue the request for the event loop to spawn
        self.lsp.ui.pending_installs.push(PendingInstallRequest {
            language_id: language_id.to_string(),
            language_name: lang.name.clone(),
            auto_install_config: auto_install.clone(),
            lsp_command: lsp.command.clone(),
        });
    }

    /// Trigger LSP server uninstall for a language
    pub fn request_lsp_uninstall(&mut self, language_id: &str) {
        use crate::language_config::LanguageRegistry;

        let Some(registry) = LanguageRegistry::try_get() else {
            return;
        };
        let Some(lang) = registry.get_by_id(language_id) else {
            return;
        };
        let Some(lsp) = &lang.lsp else { return };

        // Determine uninstall command based on auto_install config
        let hint = if let Some(auto) = &lsp.auto_install {
            match &auto.method {
                crate::language_config::InstallMethod::Npm { global, .. } => {
                    let packages = auto.method.npm_packages();
                    if packages.is_empty() {
                        return self.set_status_message(format!(
                            "No uninstall method configured for {}",
                            lang.name
                        ));
                    }
                    let flag = if *global { " -g" } else { "" };
                    format!("Run: npm uninstall{flag} {}", packages.join(" "))
                }
                crate::language_config::InstallMethod::Cargo { package, .. } => {
                    format!("Run: cargo uninstall {package}")
                }
                crate::language_config::InstallMethod::Shell { command } => {
                    format!("Installed via shell. Remove manually: {command}")
                }
                crate::language_config::InstallMethod::Github { install_path, .. } => {
                    format!("Remove: {install_path}")
                }
            }
        } else if let Some(hint) = &lsp.install_hint {
            format!("Manual removal needed. Install method: {hint}")
        } else {
            format!("No uninstall method for {}", lang.name)
        };

        self.set_status_message(hint);
    }

    /// Poll install progress channel and update panel state
    pub fn poll_install_progress(&mut self) -> bool {
        use super::lsp_manager_panel::InstallStatus;

        let Some(rx) = &mut self.lsp.ui.install_progress_rx else {
            return false;
        };

        let mut updated = false;
        while let Ok(progress) = rx.try_recv() {
            if let Some(panel) = &mut self.lsp.ui.lsp_manager_panel {
                panel
                    .active_installs
                    .insert(progress.language_id.clone(), progress.status.clone());

                // On success, rebuild entries to reflect new state
                if matches!(progress.status, InstallStatus::Success) {
                    let running = self.lsp.state.running_server_languages();
                    panel.update_running_servers(running);
                }
            }
            updated = true;
        }
        updated
    }

    /// Drain pending install requests (called by event loop)
    pub fn take_pending_installs(
        &mut self,
    ) -> Vec<super::lsp_manager_panel::PendingInstallRequest> {
        std::mem::take(&mut self.lsp.ui.pending_installs)
    }

    /// Get the install progress sender (for spawning background tasks)
    pub fn install_progress_tx(
        &self,
    ) -> Option<&tokio::sync::mpsc::UnboundedSender<super::lsp_manager_panel::InstallProgress>>
    {
        self.lsp.ui.install_progress_tx.as_ref()
    }

    /// Get language IDs of currently running LSP servers
    fn get_running_lsp_servers(&self) -> Vec<String> {
        self.lsp.state.running_server_languages()
    }
}

#[cfg(test)]
mod file_tree_tests {
    use super::*;

    #[test]
    fn opening_a_directory_focuses_the_tree_without_naming_the_buffer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main.rs"), "fn main() {}").unwrap();
        let mut editor = Editor::new();

        editor.open_directory(directory.path()).unwrap();

        assert_eq!(editor.mode(), Mode::FileTree);
        assert!(editor.file_tree().is_visible());
        assert_eq!(
            editor.file_tree().root_path(),
            Some(directory.path().canonicalize().unwrap().as_path())
        );
        assert_eq!(editor.buffer().file_path(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn opening_a_file_from_the_tree_closes_the_sidebar() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let mut editor = Editor::new();
        editor.open_directory(directory.path()).unwrap();
        editor.file_tree_mut().reveal_path(&file);

        editor.open_file_from_tree();

        let canonical_file = file.canonicalize().unwrap();
        assert_eq!(editor.mode(), Mode::Normal);
        assert!(!editor.file_tree().is_visible());
        assert_eq!(editor.buffer().file_path(), canonical_file.to_str());
    }
}
