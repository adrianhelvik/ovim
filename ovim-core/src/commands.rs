//! Command execution for ex commands (:w, :q, etc.)

use crate::command_result::{err, ok, ok_silent, CommandResult};
use crate::editor::Editor;
use crate::editor::QuickfixEntry;
use crate::unicode::GraphemeCol;

/// Expands ~ to home directory in file paths
///
/// Returns an error if the path starts with ~ but the home directory cannot be determined.
fn expand_tilde(path: &str) -> Result<std::path::PathBuf, String> {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let expanded = format!("{}{}", home.display(), &path[1..]);
            return Ok(std::path::PathBuf::from(expanded));
        } else {
            return Err("Could not determine home directory".to_string());
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return Ok(home);
        } else {
            return Err("Could not determine home directory".to_string());
        }
    }
    Ok(std::path::PathBuf::from(path))
}

/// Options for the `save_buffer` helper.
struct SaveOpts<'a> {
    /// Path to save to (None = use buffer's current path).
    path: Option<&'a str>,
    /// Skip the read-only check and clear the flag after save.
    force: bool,
    /// Quit the editor after a successful save.
    quit_after: bool,
}

/// Common save-and-mark logic shared by :w, :w!, :wq, :wq!, and :w <file>.
fn save_buffer(editor: &mut Editor, opts: SaveOpts<'_>) -> CommandResult {
    if !opts.force && editor.buffer().is_read_only() {
        return err("E45: 'readonly' option is set (add ! to override)");
    }

    let resolved = match opts.path {
        Some(raw) => match expand_tilde(raw) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(e) => return err(format!("Failed to expand path '{}': {}", raw, e)),
        },
        None => match editor.buffer().file_path().map(|s| s.to_string()) {
            Some(p) => p,
            None => return err("No file name"),
        },
    };

    let old_path = editor.buffer().file_path().map(|s| s.to_string());

    // Do not silently overwrite changes made by another process. Save-as to a
    // different file remains valid, and the bang variants are the explicit
    // escape hatch when the user intentionally wants the in-memory copy to win.
    let targets_current_file = old_path.as_deref().is_some_and(|current| {
        let current = std::path::Path::new(current);
        let resolved = std::path::Path::new(&resolved);
        current == resolved
            || match (current.canonicalize(), resolved.canonicalize()) {
                (Ok(current), Ok(resolved)) => current == resolved,
                _ => false,
            }
    });
    if !opts.force && targets_current_file && editor.buffer().file_mtime().is_some() {
        match editor.buffer().check_external_modification() {
            Ok(true) => return err("E211: File changed since editing started (add ! to override)"),
            Ok(false) => {}
            Err(error) => return err(format!("Failed to check file before saving: {error}")),
        }
    }

    match editor.buffer_mut().save_as(&resolved) {
        Ok(_) => {
            let new_path = editor.buffer().file_path().map(|s| s.to_string());
            editor.handle_file_path_transition_after_save(old_path, new_path);
            // Git refresh runs on a background thread to avoid blocking the UI.
            editor.spawn_git_refresh(&resolved, editor.options.blame);
            if opts.force {
                editor.buffer_mut().set_read_only(false);
            }
            editor.mark_saved();
            editor.mark_buffer_saved();

            if opts.quit_after {
                editor.quit();
                return ok("Saved and quitting");
            }

            let saved_path = editor
                .buffer()
                .file_path()
                .map(|p| p.to_string())
                .unwrap_or(resolved);
            let line_count = editor.buffer().rope().len_lines();
            let char_count = editor.buffer().rope().len_chars();
            ok(format!(
                "\"{}\" {}L, {}C written",
                saved_path, line_count, char_count
            ))
        }
        Err(e) => err(format!("Failed to save: {}", e)),
    }
}

/// Write all modified buffers (:wa / :wall).
///
/// Vim semantics verified in `nvim --clean` (2026-08-14): every modified
/// named buffer is written; a modified unnamed buffer reports
/// "E141: No file name for buffer N" without stopping the other writes.
fn write_all_buffers(editor: &mut Editor, force: bool) -> CommandResult {
    let (written, errors) = editor.write_all_modified_buffers(force);
    if !errors.is_empty() {
        return err(errors.join("; "));
    }
    if written == 0 {
        return ok_silent();
    }
    ok(format!(
        "{} buffer{} written",
        written,
        if written == 1 { "" } else { "s" }
    ))
}

/// Reload the current buffer from disk (:e / :e!).
fn reload_buffer(editor: &mut Editor, force: bool) -> CommandResult {
    if !force && editor.is_modified() {
        return err("No write since last change (add ! to override)");
    }
    let path = match editor.buffer().file_path().map(|s| s.to_string()) {
        Some(p) => p,
        None => {
            return err(if force {
                "No file to reload"
            } else {
                "No file name"
            })
        }
    };
    match editor.buffer_mut().reload_from_disk() {
        Ok(_) => {
            editor.mark_saved();
            editor.mark_buffer_modified_force_send();
            let line_count = editor.buffer().rope().len_lines();
            ok(format!("\"{}\" {}L reloaded", path, line_count))
        }
        Err(e) => err(format!("Failed to reload: {}", e)),
    }
}

/// Open a file for editing (:e <file> / :e! <file>).
fn edit_file(editor: &mut Editor, raw_filename: &str, force: bool) -> CommandResult {
    if !force && editor.is_modified() {
        return err("No write since last change (add ! to override)");
    }
    let filename = match expand_tilde(raw_filename) {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(e) => return err(format!("Failed to expand path '{}': {}", raw_filename, e)),
    };
    let path = std::path::Path::new(&filename);
    if path.is_dir() {
        return match editor.open_directory(path) {
            Ok(()) => ok(format!("Exploring: {}", path.display())),
            Err(error) => err(format!("Failed to open directory: {error}")),
        };
    }
    match editor.load_file(&filename) {
        Ok(_) => {
            let buf_name = editor
                .buffer()
                .file_path()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "[No Name]".to_string());
            ok(format!("Editing: {}", buf_name))
        }
        Err(e) => err(format!("Failed to load file: {}", e)),
    }
}

/// Helper function to jump to a quickfix entry
pub fn jump_to_quickfix_entry(editor: &mut Editor, entry: &QuickfixEntry) -> CommandResult {
    if let Some(ref path) = entry.filename {
        // Load the file if needed
        let path_str = path.to_string_lossy().to_string();
        if let Err(e) = editor.load_file(&path_str) {
            return err(format!("Failed to load file: {}", e));
        }

        // Jump to line/column (convert from 1-indexed to 0-indexed)
        let line = entry.lnum.saturating_sub(1);
        let col = entry.col.saturating_sub(1);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(col));
        editor.buffer_mut().validate_cursor_position();

        ok(entry.display_text())
    } else {
        ok(entry.text.clone())
    }
}

/// Execute a command (e.g., :w, :q, :tabnew)
pub fn execute_command(editor: &mut Editor, command: &str) -> CommandResult {
    // Intercept write/quit commands when in a chat scratch buffer
    if editor.is_chat_scratch_buffer() {
        match command {
            "w" | "write" | "wq" | "x" => {
                return match editor.finish_chat_scratch(true) {
                    Ok(()) => ok("Scratch content transferred to chat input"),
                    Err(error) => err(format!("Could not finish chat scratch: {error}")),
                };
            }
            "q!" | "quit!" | "bd!" | "bdelete!" | "q" | "quit" => {
                return match editor.finish_chat_scratch(false) {
                    Ok(()) => ok("Scratch buffer discarded"),
                    Err(error) => err(format!("Could not discard chat scratch: {error}")),
                };
            }
            _ => {}
        }
    }

    match command {
        "u" | "undo" => {
            editor.undo();
            ok_silent()
        }
        "red" | "redo" => {
            editor.redo();
            ok_silent()
        }
        "browser" => match editor.request_browser_start() {
            Ok(()) => ok("Opening browser"),
            Err(error) => err(format!("Could not open embedded browser: {error}")),
        },
        // `:openwin [dir]` is the cross-platform entry point for "open a
        // project in a new window". The GUI only binds an accelerator to it on
        // macOS, because elsewhere Ctrl-O is Vim's jump-back motion.
        "openwin" => {
            editor.request_open_window(None);
            ok_silent()
        }
        cmd if cmd.starts_with("openwin ") => {
            let path = cmd["openwin ".len()..].trim();
            editor.request_open_window((!path.is_empty()).then(|| path.to_string()));
            ok_silent()
        }
        "q" | "quit" => {
            if !editor.tab_page_manager().is_single_tab() {
                editor.close_current_tab();
                ok(format!(
                    "Tab closed. Now on tab {}",
                    editor.current_tab_index() + 1
                ))
            } else if editor.any_buffer_modified() {
                // Quitting the last window discards ALL buffers, so hidden
                // modified buffers must block it too (OV-00331). Verified in
                // `nvim --clean` (2026-08-14): :q on the last window with a
                // hidden modified buffer fails with E37.
                err("No write since last change (add ! to override)")
            } else {
                editor.quit();
                ok("Quitting")
            }
        }
        "q!" | "quit!" => {
            if !editor.tab_page_manager().is_single_tab() {
                editor.close_current_tab();
                ok(format!(
                    "Tab closed. Now on tab {}",
                    editor.current_tab_index() + 1
                ))
            } else {
                editor.quit();
                ok("Quitting (forced)")
            }
        }
        "cq" | "cquit" => {
            editor.quit_with_code(1);
            ok("Quitting with error code 1")
        }
        cmd if cmd.starts_with("cq ") || cmd.starts_with("cquit ") => {
            // :cq N - quit with specific exit code
            let code_str = cmd.split_whitespace().nth(1).unwrap_or("1");
            match code_str.parse::<i32>() {
                Ok(code) => {
                    editor.quit_with_code(code);
                    ok(format!("Quitting with error code {}", code))
                }
                Err(_) => err(format!("Invalid exit code: {}", code_str)),
            }
        }
        "qa" | "qall" => {
            // ANY modified buffer blocks :qa — checking only the current one
            // silently discarded hidden buffers, e.g. files a multi-file
            // rename loaded and edited (OV-00331). Verified in `nvim --clean`
            // (2026-08-14): :qa with a modified non-current buffer fails
            // with E37.
            if editor.any_buffer_modified() {
                err("No write since last change (add ! to override)")
            } else {
                editor.quit();
                ok("Quitting all")
            }
        }
        "qa!" | "qall!" => {
            editor.quit();
            ok("Quitting all (forced)")
        }
        "wa" | "wall" | "writeall" => write_all_buffers(editor, false),
        "wa!" | "wall!" | "writeall!" => write_all_buffers(editor, true),
        "w" | "write" => save_buffer(
            editor,
            SaveOpts {
                path: None,
                force: false,
                quit_after: false,
            },
        ),
        "w!" | "write!" => save_buffer(
            editor,
            SaveOpts {
                path: None,
                force: true,
                quit_after: false,
            },
        ),
        "wq" => save_buffer(
            editor,
            SaveOpts {
                path: None,
                force: false,
                quit_after: true,
            },
        ),
        "wq!" => save_buffer(
            editor,
            SaveOpts {
                path: None,
                force: true,
                quit_after: true,
            },
        ),
        "format" | "Format" => {
            editor.request_format_document();
            ok("Formatting document...")
        }
        "GitDiff" | "gitdiff" | "DiffReview" => {
            // Open (or return to) the branch diff review against the default branch
            match editor.open_diff_review(None) {
                Ok(()) => ok_silent(),
                Err(e) => err(format!("GitDiff: {e:#}")),
            }
        }
        cmd if cmd.starts_with("GitDiff ")
            || cmd.starts_with("gitdiff ")
            || cmd.starts_with("DiffReview ") =>
        {
            // :GitDiff <base> — compare the working tree against an explicit ref
            let spec = cmd
                .split_once(' ')
                .map(|(_, rest)| rest.trim())
                .unwrap_or("");
            match editor.open_diff_review(Some(spec)) {
                Ok(()) => ok_silent(),
                Err(e) => err(format!("GitDiff: {e:#}")),
            }
        }
        "GitDiffLayout" | "gitdifflayout" => {
            editor.toggle_diff_review_layout();
            ok_silent()
        }
        cmd if cmd.starts_with("GitDiffLayout ") || cmd.starts_with("gitdifflayout ") => {
            // :GitDiffLayout split|unified
            let name = cmd.split_once(' ').map(|(_, rest)| rest).unwrap_or("");
            match crate::editor::DiffLayout::parse(name) {
                Some(layout) => {
                    editor.set_diff_review_layout(layout);
                    ok_silent()
                }
                None => err("GitDiffLayout: use split or unified"),
            }
        }
        "GitFetch" | "gitfetch" => {
            editor.fetch_review_base();
            ok_silent()
        }
        "LspInfo" => {
            // Show LSP status information in a scratch buffer
            let mut info = String::new();

            if let Some(lsp_manager) = editor.lsp_manager() {
                // Get active servers from lsp_manager (more reliable than editor's map)
                let languages = lsp_manager.active_server_languages();

                if languages.is_empty() {
                    info.push_str("No active LSP servers\n");
                    if !editor.lsp_status().is_empty() {
                        info.push_str(&format!("Status: {}\n", editor.lsp_status()));
                    }
                } else {
                    info.push_str("Active LSP servers:\n\n");
                    for lang_id in &languages {
                        if let Some(cmd) = lsp_manager.server_command(lang_id) {
                            // Extract just the binary name from the full path
                            let binary_name = std::path::Path::new(&cmd)
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or(cmd);
                            info.push_str(&format!("  {} -> {}\n", lang_id, binary_name));
                        } else {
                            info.push_str(&format!("  {}\n", lang_id));
                        }
                    }

                    let (errors, warnings, info_count, hints) = editor.cached_diagnostic_count();
                    info.push_str(&format!(
                        "\nDiagnostics: {} errors, {} warnings, {} info, {} hints\n",
                        errors, warnings, info_count, hints
                    ));

                    if !editor.lsp_status().is_empty() {
                        info.push_str(&format!("\nStatus: {}\n", editor.lsp_status()));
                    }

                    if let Some(file_path) = editor.buffer().file_path() {
                        info.push_str(&format!("\nCurrent file: {}\n", file_path));
                    }
                }
            } else {
                info.push_str("LSP is not enabled\n");
            }

            editor.open_scratch_buffer("LspInfo", &info);
            crate::command_result::ok_silent()
        }
        "LspStatus" => {
            // Show detailed diagnostics list for current file
            use lsp_types::DiagnosticSeverity;

            let mut output = String::new();
            let diagnostics = editor.all_diagnostics();

            if diagnostics.is_empty() {
                output.push_str("No diagnostics for current file\n");
            } else {
                output.push_str(&format!("Diagnostics ({} total):\n\n", diagnostics.len()));

                // Group by severity
                let mut errors: Vec<_> = vec![];
                let mut warnings: Vec<_> = vec![];
                let mut infos: Vec<_> = vec![];
                let mut hints: Vec<_> = vec![];

                for d in diagnostics {
                    match d.severity {
                        Some(DiagnosticSeverity::ERROR) => errors.push(d),
                        Some(DiagnosticSeverity::WARNING) => warnings.push(d),
                        Some(DiagnosticSeverity::INFORMATION) => infos.push(d),
                        Some(DiagnosticSeverity::HINT) => hints.push(d),
                        None => infos.push(d), // Default to info if no severity
                        _ => infos.push(d),
                    }
                }

                // Print errors first
                if !errors.is_empty() {
                    output.push_str("ERRORS:\n");
                    for d in &errors {
                        let line = d.range.start.line + 1;
                        let col = d.range.start.character + 1;
                        // Truncate message to first line for cleaner display
                        let msg = d.message.lines().next().unwrap_or(&d.message);
                        output.push_str(&format!("  {}:{}: {}\n", line, col, msg));
                    }
                    output.push('\n');
                }

                // Print warnings
                if !warnings.is_empty() {
                    output.push_str("WARNINGS:\n");
                    for d in &warnings {
                        let line = d.range.start.line + 1;
                        let col = d.range.start.character + 1;
                        let msg = d.message.lines().next().unwrap_or(&d.message);
                        output.push_str(&format!("  {}:{}: {}\n", line, col, msg));
                    }
                    output.push('\n');
                }

                // Print info
                if !infos.is_empty() {
                    output.push_str("INFO:\n");
                    for d in &infos {
                        let line = d.range.start.line + 1;
                        let col = d.range.start.character + 1;
                        let msg = d.message.lines().next().unwrap_or(&d.message);
                        output.push_str(&format!("  {}:{}: {}\n", line, col, msg));
                    }
                    output.push('\n');
                }

                // Print hints
                if !hints.is_empty() {
                    output.push_str("HINTS:\n");
                    for d in &hints {
                        let line = d.range.start.line + 1;
                        let col = d.range.start.character + 1;
                        let msg = d.message.lines().next().unwrap_or(&d.message);
                        output.push_str(&format!("  {}:{}: {}\n", line, col, msg));
                    }
                }
            }

            // Also show LSP status if set
            if !editor.lsp_status().is_empty() {
                output.push_str(&format!("\nLSP Status: {}\n", editor.lsp_status()));
            }

            editor.open_scratch_buffer("LspStatus", &output);
            crate::command_result::ok_silent()
        }
        "LspLog" => {
            // Open the actual LSP log file in a new tab so % resolves correctly
            let log_path = crate::lsp::get_log_path();
            let log_path_str = log_path.to_string_lossy().to_string();
            editor.new_tab();
            match editor.load_file(&log_path_str) {
                Ok(_) => {
                    // Jump to end of log
                    let line_count = editor.buffer().rope().len_lines().saturating_sub(1);
                    editor.buffer_mut().cursor_mut().set_line(line_count);
                    crate::command_result::ok_silent()
                }
                Err(e) => err(format!("Failed to open LSP log at {}: {}", log_path_str, e)),
            }
        }
        cmd if cmd.starts_with("LspRename ") => {
            // LSP rename symbol: :LspRename new_name
            let new_name = cmd["LspRename ".len()..].trim();
            if new_name.is_empty() {
                err("Usage: LspRename <new_name>")
            } else {
                editor.request_rename(new_name.to_string());
                ok(format!("Renaming to '{}'...", new_name))
            }
        }
        "TestFile" | "TF" => {
            editor.run_test_file();
            ok("Running tests for current file...")
        }
        "TestNearest" | "TN" => {
            editor.run_test_nearest();
            ok("Running nearest test...")
        }
        "TestAll" | "TA" | "TestSuite" | "TS" => {
            editor.run_test_all();
            ok("Running all tests...")
        }
        "TestLast" | "TL" => {
            editor.run_test_last();
            ok("Re-running last test...")
        }
        "TestVisit" | "TV" => {
            editor.test_visit();
            ok("Visiting last-tested position...")
        }
        "TestPanel" | "TestToggle" | "TP" => {
            editor.toggle_test_panel();
            if editor.is_test_panel_open() {
                ok("Test panel opened")
            } else {
                ok("Test panel closed")
            }
        }
        "TestOutput" | "MakeOutput" => {
            // Show raw output from last :make / test run in a scratch buffer
            if editor.last_make_output().is_some() {
                editor.open_test_output_buffer();
                ok("Make/test output")
            } else {
                err("No make/test output available")
            }
        }
        cmd if cmd == "make" || cmd.starts_with("make ") => {
            // :make [args] — run makeprg (default: cargo build) and populate quickfix
            let args = if cmd == "make" {
                ""
            } else {
                cmd.strip_prefix("make ").unwrap_or("")
            };
            execute_make_command(editor, args)
        }
        "copen" => {
            // Open/show quickfix list
            let qf_list = editor.quickfix_list();
            if qf_list.is_empty() {
                ok("Quickfix list is empty")
            } else {
                let title = if qf_list.title().is_empty() {
                    "Quickfix List"
                } else {
                    qf_list.title()
                };
                let entries: Vec<String> = qf_list
                    .entries()
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        let marker = if i == qf_list.selected_index() {
                            ">"
                        } else {
                            " "
                        };
                        format!("{} {}", marker, entry.display_text())
                    })
                    .collect();
                let message = format!(
                    "{} ({} items)\n{}",
                    title,
                    qf_list.len(),
                    entries.join("\n")
                );
                ok(message)
            }
        }
        "cclose" | "ccl" => {
            // Close/clear quickfix list
            editor.quickfix_list_mut().clear();
            ok("Quickfix list cleared")
        }
        "cnext" | "cn" => {
            // Jump to next quickfix entry
            if editor.quickfix_list().is_empty() {
                err("Quickfix list is empty")
            } else {
                editor.quickfix_list_mut().next();
                if let Some(entry) = editor.quickfix_list().current_entry().cloned() {
                    crate::commands::jump_to_quickfix_entry(editor, &entry)
                } else {
                    err("No current entry")
                }
            }
        }
        "cprev" | "cp" | "cprevious" => {
            // Jump to previous quickfix entry
            if editor.quickfix_list().is_empty() {
                err("Quickfix list is empty")
            } else {
                editor.quickfix_list_mut().previous();
                if let Some(entry) = editor.quickfix_list().current_entry().cloned() {
                    crate::commands::jump_to_quickfix_entry(editor, &entry)
                } else {
                    err("No current entry")
                }
            }
        }
        "cfirst" | "cfir" => {
            // Jump to first quickfix entry
            if editor.quickfix_list().is_empty() {
                err("Quickfix list is empty")
            } else {
                editor.quickfix_list_mut().first();
                if let Some(entry) = editor.quickfix_list().current_entry().cloned() {
                    crate::commands::jump_to_quickfix_entry(editor, &entry)
                } else {
                    err("No current entry")
                }
            }
        }
        "clast" | "cla" => {
            // Jump to last quickfix entry
            if editor.quickfix_list().is_empty() {
                err("Quickfix list is empty")
            } else {
                editor.quickfix_list_mut().last();
                if let Some(entry) = editor.quickfix_list().current_entry().cloned() {
                    crate::commands::jump_to_quickfix_entry(editor, &entry)
                } else {
                    err("No current entry")
                }
            }
        }
        "tabnew" | "tabe" | "tabedit" => {
            // Create new tab with default name
            editor.new_tab();
            let tab_index = editor.current_tab_index() + 1; // 1-indexed for display
            ok(format!("Created tab {}", tab_index))
        }
        "tabnext" | "tabn" => {
            // Switch to next tab
            editor.next_tab();
            let tab_index = editor.current_tab_index() + 1; // 1-indexed for display
            ok(format!("Tab {}", tab_index))
        }
        "tabprev" | "tabp" | "tabprevious" => {
            // Switch to previous tab
            editor.previous_tab();
            let tab_index = editor.current_tab_index() + 1; // 1-indexed for display
            ok(format!("Tab {}", tab_index))
        }
        "tabfirst" | "tabfir" => {
            // Switch to first tab
            editor.first_tab();
            ok("Tab 1")
        }
        "tablast" | "tabl" => {
            // Switch to last tab
            editor.last_tab();
            let tab_index = editor.current_tab_index() + 1; // 1-indexed for display
            ok(format!("Tab {}", tab_index))
        }
        "tabclose" | "tabc" => {
            // Close current tab
            if editor.tab_page_manager().is_single_tab() {
                err("Cannot close last tab")
            } else {
                editor.close_current_tab();
                let tab_index = editor.current_tab_index() + 1; // 1-indexed for display
                ok(format!("Tab closed. Now on tab {}", tab_index))
            }
        }
        // Buffer commands (ls, bn, bp, bd) — dispatched to cmd_buffer module
        "ls" | "buffers" | "files" | "bnext" | "bn" | "bprev" | "bp" | "bprevious" | "bd"
        | "bdelete" | "bd!" | "bdelete!" => crate::cmd_buffer::try_handle(editor, command).unwrap(),
        "tabonly" | "tabo" => {
            // Close all tabs except the current one
            if editor.tab_page_manager().is_single_tab() {
                ok("Already only one tab")
            } else {
                let closed_count = editor.tab_count() - 1;
                editor.close_other_tabs();
                ok(format!("Closed {} tabs", closed_count))
            }
        }
        "blame" => {
            let new_val = !editor.options.blame;
            editor.options.blame = new_val;
            if new_val {
                editor.buffer_mut().load_git_blame();
                ok("blame on")
            } else {
                editor.buffer_mut().clear_git_blame();
                ok("blame off")
            }
        }
        "noh" | "nohlsearch" => {
            // Clear search highlighting
            editor.clear_search_highlight();
            ok("Search highlighting cleared")
        }
        "reg" | "registers" => {
            // Display all registers
            let registers = editor.registers().list_registers();
            if registers.is_empty() {
                ok("No registers in use")
            } else {
                let display: Vec<String> = registers
                    .iter()
                    .map(|(name, content)| format!("{}: {}", name, content))
                    .collect();
                ok(display.join("\n"))
            }
        }
        "j" | "join" => {
            // Join current line with the next line
            if let Err(e) = editor.buffer_mut().join_lines(1) {
                return err(format!("Failed to join lines: {}", e));
            }
            crate::command_result::ok_silent()
        }
        "recover" | "rec" => {
            // Recover buffer content from swap file
            if !editor.buffer().has_swap_file() {
                return err("No swap file exists for this buffer");
            }
            match editor.buffer_mut().recover_from_swap_file() {
                Ok(true) => ok("Buffer recovered from swap file"),
                Ok(false) => err("Failed to recover: swap file is empty or missing"),
                Err(e) => err(format!("Failed to recover: {}", e)),
            }
        }
        "checktime" => {
            // Check if file has been modified externally and reload if so
            match editor.buffer().check_external_modification() {
                Ok(true) => match editor.buffer_mut().reload_if_changed_sync() {
                    Ok(true) => {
                        editor.mark_buffer_modified_force_send();
                        ok("File reloaded from disk (external changes detected)".to_string())
                    }
                    Ok(false) => ok("No external changes detected"),
                    Err(e) => err(format!("Failed to reload: {}", e)),
                },
                Ok(false) => ok("No external changes detected"),
                Err(e) => err(format!("Failed to check file: {}", e)),
            }
        }
        "marks" => {
            // Display all marks
            let mut lines = Vec::new();

            // Local marks (a-z)
            let mut local_marks: Vec<_> = editor.marks().iter().collect();
            local_marks.sort_by_key(|(c, _)| *c);
            for (name, mark) in local_marks {
                lines.push(format!(" '{}  {:>5}  {:>3}", name, mark.line + 1, mark.col));
            }

            // Global marks (A-Z)
            let mut global_marks: Vec<_> = editor.marks().iter_global().collect();
            global_marks.sort_by_key(|(c, _)| *c);
            for (name, mark) in global_marks {
                let file = mark.file_path.as_deref().unwrap_or("[No Name]");
                lines.push(format!(
                    " '{}  {:>5}  {:>3}  {}",
                    name,
                    mark.line + 1,
                    mark.col,
                    file
                ));
            }

            if lines.is_empty() {
                ok("No marks set")
            } else {
                lines.insert(0, "mark  line   col  file".to_string());
                ok(lines.join("\n"))
            }
        }
        "tabs" => {
            // List all tabs
            let tabs = editor.tab_page_manager().tabs();
            let current_index = editor.current_tab_index();
            let tab_list: Vec<String> = tabs
                .iter()
                .enumerate()
                .map(|(i, _tab)| {
                    let marker = if i == current_index { ">" } else { " " };
                    format!("{} {} {}", marker, i + 1, editor.get_tab_title(i))
                })
                .collect();
            ok(tab_list.join("\n"))
        }
        "clearaedits" => {
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.agent_edits.clear();
            }
            ok("Agent edit markers cleared.")
        }
        _ => {
            // Handle :tabnew <filename>, :tabe <filename>, :tabedit <filename>
            if let Some(raw_filename) = command
                .strip_prefix("tabnew ")
                .or_else(|| command.strip_prefix("tabe "))
                .or_else(|| command.strip_prefix("tabedit "))
            {
                // Expand ~ to home directory
                let filename = match expand_tilde(raw_filename) {
                    Ok(path) => path.to_string_lossy().to_string(),
                    Err(e) => {
                        return err(format!("Failed to expand path '{}': {}", raw_filename, e));
                    }
                };

                // Create new tab and load file (or create if doesn't exist)
                editor.new_tab();

                // Try to load the file, if it doesn't exist create an empty buffer
                match editor.load_file(&filename) {
                    Ok(_) => {
                        let tab_index = editor.current_tab_index() + 1;
                        ok(format!("Opened {} in tab {}", filename, tab_index))
                    }
                    Err(e) => {
                        // Check if error is because file doesn't exist
                        if e.to_string().contains("Failed to read file")
                            || e.to_string().contains("No such file")
                        {
                            // Create a new empty buffer with the given filename
                            use crate::buffer::Buffer;
                            let new_buffer = Buffer::new();
                            // Normalize the path
                            let absolute_path = std::path::absolute(&filename)
                                .unwrap_or_else(|_| std::path::PathBuf::from(&filename));
                            let path_str = absolute_path.to_string_lossy().to_string();
                            editor.add_buffer(new_buffer);
                            editor.set_file_path(path_str);
                            editor.mark_dirty();

                            // Point the current tab at the new buffer (the
                            // tab title derives from the buffer's file path)
                            editor.sync_current_tab_buffer();

                            let tab_index = editor.current_tab_index() + 1;
                            ok(format!(
                                "Created new file {} in tab {}",
                                filename, tab_index
                            ))
                        } else {
                            err(format!("Failed to load file: {}", e))
                        }
                    }
                }
            // Handle :w <filename>
            } else if let Some(raw_filename) = command
                .strip_prefix("w ")
                .or_else(|| command.strip_prefix("write "))
            {
                save_buffer(
                    editor,
                    SaveOpts {
                        path: Some(raw_filename),
                        force: false,
                        quit_after: false,
                    },
                )
            // Handle :lua <code>
            } else if let Some(_code) = command.strip_prefix("lua ") {
                #[cfg(feature = "lua")]
                {
                    match editor.execute_lua(_code) {
                        Ok(result) => ok(result),
                        Err(e) => err(format!("Lua error: {}", e)),
                    }
                }
                #[cfg(not(feature = "lua"))]
                err("Lua support not compiled in")
            // Handle :luafile <path>
            } else if let Some(raw_path) = command.strip_prefix("luafile ") {
                let _expanded_path = match expand_tilde(raw_path.trim()) {
                    Ok(path) => path.to_string_lossy().to_string(),
                    Err(e) => {
                        return err(format!("Failed to expand path '{}': {}", raw_path, e));
                    }
                };
                #[cfg(feature = "lua")]
                {
                    match editor.execute_lua_file(&_expanded_path) {
                        Ok(_) => ok(format!("Executed {}", _expanded_path)),
                        Err(e) => err(format!("Lua error: {}", e)),
                    }
                }
                #[cfg(not(feature = "lua"))]
                err("Lua support not compiled in")
            // Handle :colorscheme <name> or :colorscheme (to show current)
            // Also support :colo abbreviation
            } else if command == "colorscheme" || command == "colo" {
                let current = editor.current_color_scheme_name();
                let schemes = editor.list_color_schemes().join(", ");
                ok(format!("Current: {}\nAvailable: {}", current, schemes))
            } else if let Some(scheme_name) = command
                .strip_prefix("colorscheme ")
                .or_else(|| command.strip_prefix("colo "))
            {
                match editor.set_color_scheme(scheme_name.trim()) {
                    Ok(_) => ok(format!("Color scheme set to '{}'", scheme_name.trim())),
                    Err(e) => {
                        let available = editor.list_color_schemes().join(", ");
                        err(format!("{}. Available schemes: {}", e, available))
                    }
                }
            // Handle :set commands
            } else if let Some(set_cmd) = command
                .strip_prefix("set ")
                .or_else(|| command.strip_prefix("se "))
            {
                crate::cmd_set::handle_set_command(editor, set_cmd.trim())
            // Handle split commands
            } else if command == "sp" || command == "split" {
                editor.split_window_horizontal();
                ok(format!(
                    "Split horizontally ({} windows)",
                    editor.window_count()
                ))
            } else if command == "vsp" || command == "vsplit" {
                editor.split_window_vertical();
                ok(format!(
                    "Split vertically ({} windows)",
                    editor.window_count()
                ))
            } else if command == "only" || command == "on" {
                // :only - close all other windows
                if editor.window_count() == 1 {
                    ok("Already only one window")
                } else {
                    editor.close_other_windows();
                    ok("All other windows closed")
                }
            // Handle config reload
            } else if command == "ConfigReload" || command == "reload" {
                #[cfg(feature = "lua")]
                {
                    match editor.reload_lua_config() {
                        Ok(msg) => ok(msg),
                        Err(e) => err(format!("Failed to reload config: {}", e)),
                    }
                }
                #[cfg(not(feature = "lua"))]
                err("Lua support not compiled in")
            // Handle :source - load and execute a Lua file
            } else if let Some(file) = command
                .strip_prefix("source ")
                .or_else(|| command.strip_prefix("so "))
            {
                let file = file.trim();
                let _expanded = match expand_tilde(file) {
                    Ok(path) => path,
                    Err(e) => {
                        return err(format!("Failed to expand path '{}': {}", file, e));
                    }
                };
                #[cfg(feature = "lua")]
                {
                    if let Some(context) = editor.lua_context_mut() {
                        let path = _expanded;
                        match context.execute_file(&path) {
                            Ok(_) => {
                                // Process any commands from the sourced file,
                                // reporting the first failure instead of
                                // silently dropping it (OV-00197).
                                let commands = editor.get_lua_commands();
                                let mut failed = Vec::new();
                                for cmd in commands {
                                    if let Err(error) =
                                        crate::editor::InputHandler::execute_command_string(
                                            editor, &cmd,
                                        )
                                    {
                                        crate::log_warn!(
                                            "lua",
                                            "sourced command '{}' failed: {}",
                                            cmd,
                                            error
                                        );
                                        failed.push(cmd);
                                    }
                                }
                                if failed.is_empty() {
                                    ok(format!("Sourced: {}", path.display()))
                                } else {
                                    ok(format!(
                                        "Sourced: {} ({} command(s) failed: {}; see log)",
                                        path.display(),
                                        failed.len(),
                                        failed.join(", ")
                                    ))
                                }
                            }
                            Err(e) => err(format!("Failed to source {}: {}", file, e)),
                        }
                    } else {
                        err("Lua not enabled")
                    }
                }
                #[cfg(not(feature = "lua"))]
                err("Lua support not compiled in")
            // Handle :e and :edit (bare) - reload current file if unmodified
            } else if command == "e" || command == "edit" {
                reload_buffer(editor, false)
            } else if command == "e!" || command == "edit!" {
                reload_buffer(editor, true)
            // :e! <filename> must be checked before :e <filename> since "e " prefix matches "e! "
            } else if let Some(raw_filename) = command
                .strip_prefix("e! ")
                .or_else(|| command.strip_prefix("edit! "))
            {
                edit_file(editor, raw_filename, true)
            } else if let Some(raw_filename) = command
                .strip_prefix("e ")
                .or_else(|| command.strip_prefix("edit "))
            {
                edit_file(editor, raw_filename, false)
            // Handle :registers or :reg (list registers)
            } else if command == "registers"
                || command == "reg"
                || command.starts_with("registers ")
                || command.starts_with("reg ")
            {
                let registers = editor.registers().list_registers();
                if registers.is_empty() {
                    ok("No registers set")
                } else {
                    let lines: Vec<String> = registers
                        .into_iter()
                        .map(|(name, content)| format!("{:<4} {}", name, content))
                        .collect();
                    ok(format!("--- Registers ---\n{}", lines.join("\n")))
                }
            // Handle :marks (list marks)
            } else if command == "marks" || command.starts_with("marks ") {
                let marks = editor.marks().list_marks();
                if marks.is_empty() {
                    ok("No marks set")
                } else {
                    let lines: Vec<String> = marks
                        .into_iter()
                        .map(|(name, line, col, file)| {
                            if let Some(path) = file {
                                format!(" {}  {:>5}  {:>3}  {}", name, line + 1, col, path)
                            } else {
                                format!(" {}  {:>5}  {:>3}", name, line + 1, col)
                            }
                        })
                        .collect();
                    ok(format!(
                        "--- Marks ---\nmark  line  col  file\n{}",
                        lines.join("\n")
                    ))
                }
            // Handle :help keybindings
            } else if command == "help keybindings" || command == "help keys" {
                ok(
                    "Keybinding compatibility guide: architecture/knowledge/keybinding-compat.md"
                        .to_string(),
                )
            // Handle :map, :noremap and variants
            } else if is_map_command(command) {
                handle_map_command(editor, command)
            // Handle :unmap and variants
            } else if is_unmap_command(command) {
                handle_unmap_command(editor, command)
            // Handle :mapclear and variants
            } else if is_mapclear_command(command) {
                handle_mapclear_command(editor, command)
            // Handle :session start/stop/list commands
            } else if command == "ai status" || command == "ai env" {
                handle_ai_status(editor)
            } else if command == "workflow" || command.starts_with("workflow ") {
                handle_workflow_command(editor, command)
            } else if command == "session" || command.starts_with("session ") {
                handle_session_command(editor, command)
            } else if command == "debug" || command.starts_with("debug ") {
                handle_debug_command(editor, command)
            } else if let Some(expr) = command.strip_prefix("eval ") {
                // :eval <expression> — evaluate expression in debug context
                if editor.is_debug_stopped() {
                    let expression = expr.trim().to_string();
                    if expression.is_empty() {
                        err("Usage: :eval <expression>")
                    } else {
                        editor.dap_manager_mut().pending_action =
                            Some(crate::dap::PendingDebugAction::Evaluate { expression });
                        ok("Evaluating...")
                    }
                } else {
                    err("Not stopped at a breakpoint")
                }
            } else if let Some(name) = command.strip_prefix("DebugExpand ") {
                // :DebugExpand <name> — toggle expansion of a variable in the debug panel
                let name = name.trim();
                if name.is_empty() {
                    err("Usage: :DebugExpand <variable_name>")
                } else if !editor.is_debug_stopped() {
                    err("Not stopped at a breakpoint")
                } else {
                    // Find the variable by name in all loaded variable scopes
                    let mut found_ref: Option<u64> = None;
                    let state = editor.debug_state();
                    for vars in state.variables.values() {
                        for var in vars {
                            if var.name == name && var.variables_reference > 0 {
                                found_ref = Some(var.variables_reference);
                                break;
                            }
                        }
                        if found_ref.is_some() {
                            break;
                        }
                    }
                    if let Some(var_ref) = found_ref {
                        if editor
                            .dap_manager_mut()
                            .state
                            .expanded_refs
                            .contains(&var_ref)
                        {
                            editor
                                .dap_manager_mut()
                                .state
                                .expanded_refs
                                .remove(&var_ref);
                        } else {
                            editor.dap_manager_mut().state.expanded_refs.insert(var_ref);
                            editor.dap_manager_mut().pending_action =
                                Some(crate::dap::PendingDebugAction::FetchVariables { var_ref });
                        }
                        editor.mark_dirty();
                        ok(format!("Toggled expansion of '{}'", name))
                    } else {
                        err(format!("Variable '{}' not found or not expandable", name))
                    }
                }
            } else if let Some(condition) = command.strip_prefix("DebugCondition ") {
                // :DebugCondition <expr> — set conditional breakpoint at cursor
                let condition = condition.trim().to_string();
                if condition.is_empty() {
                    // Empty condition = remove condition (convert to unconditional)
                    if let Some(file_path) = editor.buffer().file_path().map(|s| s.to_string()) {
                        let line = editor.buffer().cursor().line() as u64 + 1;
                        let path = std::path::PathBuf::from(&file_path);
                        editor
                            .dap_manager_mut()
                            .state
                            .set_breakpoint_condition(&path, line, None);
                    }
                } else {
                    editor.toggle_conditional_breakpoint(condition);
                }
                // Sync breakpoints if debug is active.
                if editor.is_debug_active() {
                    if let Some(file_path) = editor.buffer().file_path().map(|s| s.to_string()) {
                        let path = std::path::PathBuf::from(&file_path);
                        let lines = editor.dap_manager_mut().state.breakpoint_lines(&path);
                        let _ = lines; // Sync will happen in event loop via pending action
                        editor.dap_manager_mut().pending_action =
                            Some(crate::dap::PendingDebugAction::SyncBreakpoints);
                    }
                }
                ok("Conditional breakpoint set")
            // Handle :! shell command execution
            //
            // This path runs the command inline and returns output — used by the
            // headless API. The TUI input handler intercepts :! before reaching
            // here and queues it for terminal-aware execution instead.
            } else if let Some(shell_cmd) = command.strip_prefix('!') {
                let shell_cmd = shell_cmd.trim();
                if shell_cmd.is_empty() {
                    if let Some(last) = editor.build.last_shell_command.clone() {
                        execute_shell_command_with_expansion(editor, &last)
                    } else {
                        err("No previous shell command")
                    }
                } else {
                    editor.build.last_shell_command = Some(shell_cmd.to_string());
                    execute_shell_command_with_expansion(editor, shell_cmd)
                }
            // Handle :file / :f — show file info (like Ctrl-G in vim)
            } else if command == "f" || command == "file" {
                let name = editor
                    .buffer()
                    .file_path()
                    .map(|s| format!("\"{}\"", s))
                    .unwrap_or_else(|| "\"[No Name]\"".to_string());
                let modified = if editor.is_modified() {
                    " [Modified]"
                } else {
                    ""
                };
                let line = editor.buffer().cursor().line() + 1;
                let total = editor.buffer().line_count();
                let pct = (line * 100).checked_div(total).unwrap_or(0);
                ok(format!(
                    "{}{} line {} of {} --{}%--",
                    name, modified, line, total, pct
                ))
            // Handle :pwd — print working directory
            } else if command == "pwd" {
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "(unknown)".to_string());
                ok(cwd)
            // Handle :cd / :lcd — change working directory
            } else if command == "cd" || command == "lcd" {
                // :cd with no args → go to $HOME
                match dirs::home_dir() {
                    Some(home) => match std::env::set_current_dir(&home) {
                        Ok(_) => ok(home.display().to_string()),
                        Err(e) => err(format!("Failed to cd: {}", e)),
                    },
                    None => err("Could not determine home directory"),
                }
            } else if let Some(path) = command
                .strip_prefix("cd ")
                .or_else(|| command.strip_prefix("lcd "))
            {
                let path = path.trim();
                match expand_tilde(path) {
                    Ok(expanded) => match std::env::set_current_dir(&expanded) {
                        Ok(_) => ok(expanded.display().to_string()),
                        Err(e) => err(format!("E344: Can't find directory \"{}\" ({})", path, e)),
                    },
                    Err(e) => err(e),
                }
            // Handle :LspInstall / :LspManager - open LSP manager panel
            } else if command == "LspInstall" || command == "LspManager" {
                editor.open_lsp_manager();
                crate::command_result::ok_silent()
            // Handle line number command (e.g., :48 to go to line 48)
            } else if let Ok(line_num) = command.parse::<usize>() {
                let target_line = line_num.saturating_sub(1); // 1-indexed to 0-indexed
                let max_line = editor.buffer().line_count().saturating_sub(1);
                let final_line = target_line.min(max_line);
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(final_line, GraphemeCol::ZERO);
                ok(format!("Line {}", line_num))
            } else {
                err(format!("Not an editor command: {}", command))
            }
        }
    }
}

/// Check if this is a map command
fn is_map_command(cmd: &str) -> bool {
    let cmd_word = cmd.split_whitespace().next().unwrap_or("");
    matches!(
        cmd_word,
        "map"
            | "nmap"
            | "imap"
            | "vmap"
            | "xmap"
            | "cmap"
            | "noremap"
            | "nnoremap"
            | "inoremap"
            | "vnoremap"
            | "xnoremap"
            | "cnoremap"
    )
}

/// Check if this is an unmap command
fn is_unmap_command(cmd: &str) -> bool {
    let cmd_word = cmd.split_whitespace().next().unwrap_or("");
    matches!(
        cmd_word,
        "unmap" | "nunmap" | "iunmap" | "vunmap" | "xunmap" | "cunmap"
    )
}

/// Check if this is a mapclear command
fn is_mapclear_command(cmd: &str) -> bool {
    let cmd_word = cmd.split_whitespace().next().unwrap_or("");
    matches!(
        cmd_word,
        "mapclear" | "nmapclear" | "imapclear" | "vmapclear" | "xmapclear" | "cmapclear"
    )
}

/// Parse key notation for map/unmap commands with `<leader>` expansion.
fn parse_map_keys(editor: &Editor, input: &str) -> String {
    use crate::editor::KeyMapManager;

    let leader = editor.leader_key().to_string();
    let expanded = input
        .replace("<leader>", &leader)
        .replace("<Leader>", &leader);

    KeyMapManager::parse_key_notation(&expanded)
}

/// Handle map and noremap commands
fn handle_map_command(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::editor::MapMode;

    let parts: Vec<&str> = command.splitn(3, char::is_whitespace).collect();
    let cmd_word = parts.first().copied().unwrap_or("");

    // Determine mode and whether it's noremap
    let (mode, noremap) = match cmd_word {
        "map" => (MapMode::All, false),
        "noremap" => (MapMode::All, true),
        "nmap" => (MapMode::Normal, false),
        "nnoremap" => (MapMode::Normal, true),
        "imap" => (MapMode::Insert, false),
        "inoremap" => (MapMode::Insert, true),
        "vmap" | "xmap" => (MapMode::Visual, false),
        "vnoremap" | "xnoremap" => (MapMode::Visual, true),
        "cmap" => (MapMode::Command, false),
        "cnoremap" => (MapMode::Command, true),
        _ => (MapMode::Normal, false),
    };

    // If no arguments, list mappings for this mode
    if parts.len() == 1 {
        let mappings = editor.keymaps().list_mappings(Some(mode));
        if mappings.is_empty() {
            return ok("No mappings");
        }
        let lines: Vec<String> = mappings
            .into_iter()
            .map(|(m, mapping)| {
                let noremap_char = if mapping.noremap { '*' } else { ' ' };
                format!(
                    "{}{}  {}  {}",
                    m.display_char(),
                    noremap_char,
                    mapping.lhs,
                    mapping.rhs
                )
            })
            .collect();
        return ok(format!("--- Mappings ---\n{}", lines.join("\n")));
    }

    // If only lhs provided, show mapping for that key
    if parts.len() == 2 {
        let lhs = parse_map_keys(editor, parts[1]);
        if let Some(mapping) = editor.keymaps().get_mapping(mode, &lhs) {
            return ok(format!(
                "{}  {}  {}",
                mode.display_char(),
                mapping.lhs,
                mapping.rhs
            ));
        } else {
            return ok("No mapping found");
        }
    }

    // parts.len() >= 3: lhs and rhs provided
    let lhs = parse_map_keys(editor, parts[1]);
    let rhs = parse_map_keys(editor, parts[2]);

    editor
        .keymaps_mut()
        .add_mapping(mode, lhs.clone(), rhs, noremap);

    crate::command_result::ok_silent()
}

/// Handle unmap commands
fn handle_unmap_command(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::editor::MapMode;

    let parts: Vec<&str> = command.split_whitespace().collect();
    let cmd_word = parts.first().copied().unwrap_or("");

    let mode = match cmd_word {
        "unmap" => MapMode::All,
        "nunmap" => MapMode::Normal,
        "iunmap" => MapMode::Insert,
        "vunmap" | "xunmap" => MapMode::Visual,
        "cunmap" => MapMode::Command,
        _ => MapMode::Normal,
    };

    if parts.len() < 2 {
        return err("E474: Invalid argument");
    }

    let lhs = parse_map_keys(editor, parts[1]);
    if editor.keymaps_mut().remove_mapping(mode, &lhs) {
        crate::command_result::ok_silent()
    } else {
        err("E31: No such mapping")
    }
}

/// Handle mapclear commands
fn handle_mapclear_command(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::editor::MapMode;

    let cmd_word = command.split_whitespace().next().unwrap_or("");

    let mode = match cmd_word {
        "mapclear" => MapMode::All,
        "nmapclear" => MapMode::Normal,
        "imapclear" => MapMode::Insert,
        "vmapclear" | "xmapclear" => MapMode::Visual,
        "cmapclear" => MapMode::Command,
        _ => MapMode::Normal,
    };

    editor.keymaps_mut().clear_mappings(mode);

    crate::command_result::ok_silent()
}

/// Execute :make command — runs makeprg and populates the quickfix list
fn execute_make_command(editor: &mut Editor, args: &str) -> CommandResult {
    use crate::editor::{MakeResult, PendingMake};
    use std::process::Command;

    // Build the command: makeprg + args (default: "cargo build")
    let makeprg = editor.options.makeprg.clone();
    let cmd = if args.is_empty() {
        makeprg
    } else {
        format!("{} {}", makeprg, args)
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let cmd_clone = cmd.clone();

    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        let (shell, shell_arg) = ("cmd", "/C");
        #[cfg(not(target_os = "windows"))]
        let (shell, shell_arg) = ("sh", "-c");

        let result = match Command::new(shell).arg(shell_arg).arg(&cmd_clone).output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                MakeResult {
                    output: format!("{}{}", stdout, stderr),
                    success: output.status.success(),
                }
            }
            Err(e) => MakeResult {
                output: format!("Failed to run '{}': {}", cmd_clone, e),
                success: false,
            },
        };
        let _ = tx.send(result);
    });

    editor.set_pending_make(PendingMake {
        receiver: rx,
        command: cmd.clone(),
    });

    ok(format!("Running: {}", cmd))
}

/// Parse compiler output for file:line:col: error/warning patterns
pub fn parse_compiler_output(output: &str) -> Vec<QuickfixEntry> {
    use crate::editor::{QuickfixEntry, QuickfixEntryType};
    use std::path::PathBuf;

    let mut entries = Vec::new();

    // Regex-free parsing for common patterns:
    // - rustc/cargo:  "  --> file.rs:line:col"
    // - gcc/clang:    "file.rs:line:col: error: message"
    // - typescript:    "file.ts(line,col): error TS1234: message"
    for line in output.lines() {
        let trimmed = line.trim();

        // Rust/cargo style: "  --> file.rs:42:10"
        if let Some(rest) = trimmed.strip_prefix("--> ") {
            // Split from the right: col, then line, rest is file path
            let parts: Vec<&str> = rest.rsplitn(3, ':').collect();
            if parts.len() == 3 {
                if let (Ok(col), Ok(lnum)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>())
                {
                    entries.push(QuickfixEntry::new(
                        Some(PathBuf::from(parts[2])),
                        lnum,
                        col,
                        QuickfixEntryType::Error,
                        String::new(), // Will be filled from context
                    ));
                }
            }
            continue;
        }

        // Rust panic style: "thread 'name' panicked at 'msg', file.rs:42:5"
        // Also: "thread 'name' panicked at file.rs:42:5:" (Rust 2024+ format)
        if trimmed.starts_with("thread '") && trimmed.contains("panicked at") {
            if let Some(entry) = parse_panic_line(trimmed) {
                entries.push(entry);
                continue;
            }
        }

        // gcc/clang/generic style: "file:line:col: error/warning: message"
        // Also matches: "file:line: error: message" (no col)
        if let Some(entry) = parse_gcc_style_line(trimmed) {
            entries.push(entry);
        }
    }

    // Second pass: fill in error messages from cargo/rustc output
    // Cargo errors look like:
    //   error[E0425]: cannot find value `foo` in this scope
    //     --> file.rs:42:10
    // So we look for "error" or "warning" lines preceding "-->" lines
    let lines: Vec<&str> = output.lines().collect();
    let mut entry_idx = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("--> ") && entry_idx < entries.len() {
            // Look backward for the error/warning message
            if i > 0 {
                let prev = lines[i - 1].trim();
                if prev.starts_with("error") || prev.starts_with("warning") {
                    entries[entry_idx].text = prev.to_string();
                    if prev.starts_with("warning") {
                        entries[entry_idx].entry_type = QuickfixEntryType::Warning;
                    }
                }
            }
            entry_idx += 1;
        }
    }

    entries
}

/// Parse a gcc/clang-style error line: "file:line:col: error: message"
fn parse_gcc_style_line(line: &str) -> Option<QuickfixEntry> {
    use crate::editor::{QuickfixEntry, QuickfixEntryType};
    use std::path::PathBuf;

    // Skip lines that don't look like file:line patterns
    // Must contain at least one ':' and not start with whitespace
    if line.is_empty() || line.starts_with(' ') || !line.contains(':') {
        return None;
    }

    // Try to match file:line:col: type: message
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 3 {
        return None;
    }

    let file = parts[0];
    let lnum: usize = parts[1].trim().parse().ok()?;

    // Check if parts[2] is a column number or the error type
    let (col, rest) = if let Ok(c) = parts[2].trim().parse::<usize>() {
        let rest = if parts.len() > 3 { parts[3] } else { "" };
        (c, rest)
    } else {
        // No column — parts[2] is the type/message
        let rest = if parts.len() > 3 {
            &line[parts[0].len() + 1 + parts[1].len() + 1..]
        } else {
            parts[2]
        };
        (0, rest)
    };

    let rest = rest.trim();
    let entry_type = if rest.starts_with("error") {
        QuickfixEntryType::Error
    } else if rest.starts_with("warning") {
        QuickfixEntryType::Warning
    } else if rest.starts_with("note") {
        QuickfixEntryType::Note
    } else if rest.starts_with("info") {
        QuickfixEntryType::Info
    } else {
        // Not a recognizable error pattern — could be just a file path with colons
        // Only include if it looks like an error (has some message text)
        if rest.is_empty() {
            return None;
        }
        QuickfixEntryType::Error
    };

    // Don't include entries for paths that don't look like files
    if !file.contains('.') && !file.contains('/') {
        return None;
    }

    Some(QuickfixEntry::new(
        Some(PathBuf::from(file)),
        lnum,
        col,
        entry_type,
        rest.to_string(),
    ))
}

/// Parse Rust panic lines from test output.
///
/// Formats:
/// - `thread 'test_name' panicked at 'assertion message', src/file.rs:42:5`
/// - `thread 'test_name' panicked at src/file.rs:42:5:` (Rust 2024+)
fn parse_panic_line(line: &str) -> Option<QuickfixEntry> {
    // Extract the panic message and location
    let after_panicked = line.split("panicked at").nth(1)?.trim();

    // Try old format: 'message', file:line:col
    if after_panicked.starts_with('\'') {
        // Find closing quote + comma
        if let Some(comma_pos) = after_panicked.rfind("', ") {
            let message = &after_panicked[1..comma_pos];
            let location = &after_panicked[comma_pos + 3..];
            return parse_file_line_col(location, message);
        }
    }

    // Try new format: file:line:col:
    // or: file:line:col:\nmessage
    let location = after_panicked.trim_end_matches(':');
    parse_file_line_col(location, "panicked")
}

/// Parse a `file:line:col` string into a QuickfixEntry.
fn parse_file_line_col(location: &str, message: &str) -> Option<QuickfixEntry> {
    use crate::editor::{QuickfixEntry, QuickfixEntryType};

    let parts: Vec<&str> = location.rsplitn(3, ':').collect();
    if parts.len() >= 2 {
        let col: usize = parts[0].trim().parse().ok().unwrap_or(0);
        let lnum: usize = parts[1].trim().parse().ok()?;
        let file = if parts.len() == 3 {
            parts[2]
        } else {
            return None;
        };

        if !file.contains('.') && !file.contains('/') {
            return None;
        }

        return Some(QuickfixEntry::new(
            Some(std::path::PathBuf::from(file)),
            lnum,
            col,
            QuickfixEntryType::Error,
            format!("PANIC: {}", message),
        ));
    }
    None
}

/// Execute a shell command with % and # expansion, and return the output
fn execute_shell_command_with_expansion(editor: &Editor, cmd: &str) -> CommandResult {
    use crate::editor::shell_expansion::expand_shell_command;

    // Get current and alternate file for expansion
    let current_file = editor.buffer().file_path().unwrap_or("").to_string();
    let alternate_file = editor.registers().get(Some('#'));

    // Expand % and # in the command
    let expanded_cmd = expand_shell_command(cmd, &current_file, &alternate_file);

    execute_shell_command(&expanded_cmd)
}

/// Execute a shell command and return the output
fn execute_shell_command(cmd: &str) -> CommandResult {
    use std::process::Command;

    // Determine the shell to use based on platform
    #[cfg(target_os = "windows")]
    let (shell, shell_arg) = ("cmd", "/C");
    #[cfg(not(target_os = "windows"))]
    let (shell, shell_arg) = ("sh", "-c");

    match Command::new(shell).arg(shell_arg).arg(cmd).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let mut result = String::new();

            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&stderr);
            }

            // Trim trailing newlines for cleaner display
            let result = result.trim_end().to_string();

            if output.status.success() {
                if result.is_empty() {
                    ok("Command executed successfully")
                } else {
                    ok(result)
                }
            } else {
                let exit_code = output
                    .status
                    .code()
                    .map(|c| format!(" (exit code {})", c))
                    .unwrap_or_default();
                if result.is_empty() {
                    err(format!("Command failed{}", exit_code))
                } else {
                    err(format!("{}\n\nCommand failed{}", result, exit_code))
                }
            }
        }
        Err(e) => err(format!("Failed to execute command: {}", e)),
    }
}

/// Handle :session start/stop/list commands
///
/// `:session start NAME` is retained as a clear migration error. Automation
/// sessions must be explicit at process startup so ordinary TUI instances do
/// not expose a latent network mutation surface.
/// `:session stop` — removes a frontend-managed session registration
/// `:session` or `:session list` — shows active sessions
/// `:ai status` / `:ai env` — show active AI profile and env var diagnostics.
fn handle_ai_status(editor: &mut Editor) -> CommandResult {
    let config = &editor.ai_state.config;
    let active = &editor.ai_state.active_profile;

    let mut lines = Vec::new();
    lines.push("**AI Configuration**".to_string());
    lines.push(format!("Active profile: {}", active));
    lines.push(format!("Default profile: {}", config.default_profile));
    lines.push(format!(
        "Profiles: {}",
        config
            .profiles
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let approval_mode = match config.tool_approval_mode {
        crate::ai::ToolApprovalMode::Auto => "auto",
        crate::ai::ToolApprovalMode::SensitivePrompt => "sensitive_prompt",
        crate::ai::ToolApprovalMode::AlwaysPrompt => "always_prompt",
    };
    lines.push(format!("Tool approval mode: {}", approval_mode));

    // Show context mappings
    if !config.contexts.is_empty() {
        let ctx_str: Vec<String> = config
            .contexts
            .iter()
            .map(|(k, v)| format!("{}→{}", k, v))
            .collect();
        lines.push(format!("Contexts: {}", ctx_str.join(", ")));
    }

    lines.push(String::new());

    // Show details for active profile
    if let Some(profile) = config.resolve_profile(active) {
        lines.push(format!("**Profile '{}' details:**", active));
        lines.push(format!("  Provider: {}", profile.provider));
        lines.push(format!("  Model: {}", profile.model));
        if let Some(ref url) = profile.base_url {
            lines.push(format!("  Base URL: {}", url));
        }
        lines.push(format!("  Edit format: {}", profile.edit_format));

        // Environment variable check
        let env_name = profile.api_key_env.as_deref().unwrap_or("(none)");
        lines.push(format!("  API key env var: {}", env_name));
        if let Some(ref name) = profile.api_key_env {
            match std::env::var(name) {
                Ok(val) => {
                    let masked = if val.chars().count() > 8 {
                        let head: String = val.chars().take(4).collect();
                        let tail: String = val
                            .chars()
                            .rev()
                            .take(4)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        format!("{}...{}", head, tail)
                    } else {
                        "****".to_string()
                    };
                    lines.push(format!("  Env var status: SET ({})", masked));
                }
                Err(_) => {
                    lines.push("  Env var status: NOT SET".to_string());
                }
            }
        }
    } else {
        lines.push(format!("Active profile '{}' not found!", active));
    }

    // Show all AI-related env vars visible to this process
    lines.push(String::new());
    lines.push("**AI-related env vars visible to process:**".to_string());
    let mut found_any = false;
    for (key, val) in std::env::vars() {
        if key.contains("OPENAI")
            || key.contains("ANTHROPIC")
            || key.contains("OVIM")
            || key.contains("API_KEY")
        {
            let masked = if val.chars().count() > 8 {
                let head: String = val.chars().take(4).collect();
                let tail: String = val
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("{}...{}", head, tail)
            } else {
                "****".to_string()
            };
            lines.push(format!("  {} = {}", key, masked));
            found_any = true;
        }
    }
    if !found_any {
        lines.push("  (none found matching OPENAI/ANTHROPIC/OVIM/API_KEY)".to_string());
    }

    let message = lines.join("\n");
    editor.set_hover_info(message);
    crate::command_result::ok_silent()
}

fn handle_workflow_command(editor: &mut Editor, command: &str) -> CommandResult {
    let subcmd = command.strip_prefix("workflow").unwrap_or("").trim();

    match subcmd {
        "" | "list" => {
            if let Err(e) = editor.ensure_workflows_loaded() {
                return err(format!("Failed to load workflows: {}", e));
            }
            let names = editor.workflow_names_sorted();
            let message = if names.is_empty() {
                "No workflows found.".to_string()
            } else {
                format!("{} workflow(s):\n{}", names.len(), names.join("\n"))
            };
            ok(message)
        }
        "reload" => match editor.reload_workflows() {
            Ok(count) => ok(format!("Loaded {} workflow(s)", count)),
            Err(e) => err(format!("Failed to reload workflows: {}", e)),
        },
        "status" => ok(editor.workflow_status_report()),
        s if s.starts_with("run ") => {
            let mut parts = s["run ".len()..].split_whitespace();
            let Some(name) = parts.next() else {
                return err("Usage: :workflow run <name> [k=v ...]");
            };

            let mut inputs = std::collections::BTreeMap::new();
            for pair in parts {
                let Some((key, raw_value)) = pair.split_once('=') else {
                    return err(format!("Invalid input '{}': expected k=v", pair));
                };
                let value = serde_json::from_str::<serde_json::Value>(raw_value)
                    .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
                inputs.insert(key.to_string(), value);
            }

            match editor.run_workflow(name, inputs) {
                Ok(run_id) => ok(format!("Workflow '{}' started (run #{})", name, run_id)),
                Err(e) => err(format!("Failed to run workflow '{}': {}", name, e)),
            }
        }
        _ => err(format!(
                "Unknown workflow subcommand '{}'. Usage: :workflow [list|reload|run <name> [k=v ...]|status]",
                subcmd
            )),
    }
}

fn handle_session_command(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::session::SessionInfo;

    let subcmd = command.strip_prefix("session").unwrap_or("").trim();

    match subcmd {
        "" | "list" => {
            // Show active sessions
            match SessionInfo::list_all() {
                Ok(sessions) if sessions.is_empty() => {
                    let msg = if let Some(name) = editor.active_session() {
                        format!("Active session: {}", name)
                    } else {
                        "No registered sessions. Start an explicit automation session with: ovim <file> --headless --session NAME".to_string()
                    };
                    ok(msg)
                }
                Ok(sessions) => {
                    let mut msg = format!("{} active session(s):", sessions.len());
                    for s in &sessions {
                        let marker = if editor.active_session() == Some(&s.session_name) {
                            " (this)"
                        } else {
                            ""
                        };
                        msg.push_str(&format!(
                            "\n  {} (PID {}, port {}){}",
                            s.session_name, s.pid, s.port, marker
                        ));
                    }
                    ok(msg)
                }
                Err(e) => err(format!("Failed to list sessions: {}", e)),
            }
        }
        s if s.starts_with("start ") => {
            let name = s["start ".len()..].trim();
            if name.is_empty() {
                return err("Usage: :session start NAME");
            }

            // Validate with the same rule reads enforce (charset + 64-char cap),
            // so a registered session can always be read back and targeted.
            if let Err(e) = SessionInfo::validate_session_name(name) {
                return err(e.to_string());
            }

            // Check if already registered
            if let Some(existing) = editor.active_session() {
                return err(format!(
                    "Already registered as session '{}'. Use :session stop first.",
                    existing
                ));
            }

            // Need API port to register
            let port = match editor.api_port() {
                Some(p) => p,
                None => {
                    return err(
                        "Interactive sessions do not expose the automation API. Start one explicitly with: ovim <file> --headless --session NAME",
                    );
                }
            };

            let file = editor.buffer().file_path().map(|s| s.to_string());

            let session_info = SessionInfo::new(port, file, name.to_string());
            match session_info.write() {
                Ok(()) => {
                    editor.set_active_session(name.to_string());
                    ok(format!("Session '{}' registered", name))
                }
                Err(e) => err(format!("Failed to register session: {}", e)),
            }
        }
        "stop" => {
            match editor.take_active_session() {
                Some(name) => {
                    // Delete the session file
                    let port = editor.api_port().unwrap_or(0);
                    let session_info = SessionInfo::new(port, None, name.clone());
                    let _ = session_info.delete();
                    ok(format!("Session '{}' unregistered", name))
                }
                None => err("No active session to stop"),
            }
        }
        _ => err(format!(
            "Unknown session subcommand: '{}'. Usage: :session [start NAME|stop|list]",
            subcmd
        )),
    }
}

fn handle_debug_command(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::dap::PendingDebugAction;

    let subcmd = command.strip_prefix("debug").unwrap_or("").trim();

    match subcmd {
        "breakpoint" | "bp" => {
            editor.toggle_breakpoint();
            ok("Breakpoint toggled")
        }
        "panels" => {
            editor.toggle_debug_panels();
            crate::command_result::ok_silent()
        }
        "continue" | "c" => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Continue);
            ok("Continue")
        }
        "next" | "n" | "step" => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::StepOver);
            ok("Step over")
        }
        "stepin" | "si" => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::StepIn);
            ok("Step in")
        }
        "stepout" | "so" => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::StepOut);
            ok("Step out")
        }
        "stop" => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Stop);
            ok("Stopping debug session")
        }
        "start" => {
            // Auto-detect DAP adapter from the current file's language config
            let dap_config = editor
                .buffer()
                .file_path()
                .and_then(|fp| {
                    crate::language_config::LanguageRegistry::try_get()
                        .and_then(|reg| reg.detect(fp))
                })
                .and_then(|lang| lang.dap.as_ref());

            let Some(config) = dap_config else {
                return err("No DAP adapter configured for this language. Use :debug start <command> [args...]");
            };
            let Some(cmd) = crate::language_config::find_dap_command(config) else {
                let hint = config.install_hint.as_deref().unwrap_or("Install the debug adapter and ensure it's in PATH");
                return err(format!("DAP adapter '{}' not found. {}", config.command, hint));
            };
            let args = config.args.clone();
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Start {
                command: cmd.clone(),
                args: args.clone(),
                run_config: None,
            });
            ok(format!("Starting debug adapter: {} {}", cmd, args.join(" ")))
        }
        s if s.starts_with("start ") => {
            let rest = s["start ".len()..].trim();
            let mut parts = rest.split_whitespace();
            let Some(cmd) = parts.next() else {
                return err("Usage: :debug start [command] [args...]");
            };
            let args: Vec<String> = parts.map(String::from).collect();
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Start {
                command: cmd.to_string(),
                args,
                run_config: None,
            });
            ok(format!("Starting debug adapter: {}", cmd))
        }
        "" => ok(
                "Usage: :debug [start <cmd>|stop|continue|next|stepin|stepout|breakpoint|panels]"
                    .to_string(),
            ),
        _ => err(format!(
                "Unknown debug subcommand: '{}'. Usage: :debug [start|stop|continue|next|stepin|stepout|breakpoint|panels]",
                subcmd
            )),
    }
}

/// Handle :set commands for options.
///
/// **Deprecated**: Use [`crate::cmd_set::handle_set_command`] directly.
/// This wrapper exists for backwards compatibility with any external callers.
pub fn handle_set_command(editor: &mut Editor, args: &str) -> CommandResult {
    crate::cmd_set::handle_set_command(editor, args)
}

#[cfg(test)]
mod tests {
    use super::execute_command;
    use crate::command_result::CommandResult;
    use crate::editor::{Editor, EditorServices};
    use crate::unicode::CharCol;

    #[test]
    fn edit_directory_opens_the_explorer_instead_of_a_text_buffer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main.rs"), "fn main() {}").unwrap();
        let mut editor = Editor::new();

        let result = execute_command(&mut editor, &format!("edit {}", directory.path().display()));

        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(editor.mode(), crate::mode::Mode::FileTree);
        assert_eq!(
            editor.file_tree().root_path(),
            Some(directory.path().canonicalize().unwrap().as_path())
        );
        assert_eq!(editor.buffer().file_path(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_refuses_to_overwrite_external_changes_without_bang() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("conflict.txt");
        std::fs::write(&path, "original\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&path).await.unwrap();
        editor
            .buffer_mut()
            .insert_text_at(0, CharCol::ZERO, "local ");

        // Ensure even coarse filesystems observe a distinct modification time.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "external\n").unwrap();

        let result = execute_command(&mut editor, "w");
        assert!(
            matches!(result, CommandResult::Error(ref error) if error.error.contains("E211")),
            "unexpected result: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external\n");

        let result = execute_command(&mut editor, "w!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "local original\n");
    }

    #[test]
    fn format_command_requests_lsp_document_formatting() {
        let mut editor = Editor::new();

        let result = execute_command(&mut editor, "format");

        assert!(matches!(result, CommandResult::Success(_)));
        assert!(editor.pending_intents().format_document);
    }

    #[tokio::test]
    async fn browser_command_opens_a_new_frontend_session() {
        let (browser, mut host) = crate::browser::browser_channel();
        let mut editor =
            Editor::new().with_services(EditorServices::default().with_browser(browser));

        let result = execute_command(&mut editor, "browser");
        assert!(matches!(result, CommandResult::Success(_)));

        let host_task = tokio::spawn(async move {
            let request = host.recv().await.expect("browser start request");
            assert_eq!(
                request.command(),
                &crate::browser::BrowserCommand::Start { url: None }
            );
            request.respond(Ok(crate::browser::BrowserResponse::Session(
                crate::browser::BrowserSession {
                    session_id: "browser-1".into(),
                    url: String::new(),
                    title: String::new(),
                    visible: false,
                    loading: false,
                    document_id: 0,
                },
            )));
        });
        editor.dispatch_pending_intents().await;
        host_task.await.unwrap();
    }

    #[tokio::test]
    async fn browser_command_surfaces_a_host_rejection() {
        let (browser, mut host) = crate::browser::browser_channel();
        let mut editor =
            Editor::new().with_services(EditorServices::default().with_browser(browser));
        assert!(matches!(
            execute_command(&mut editor, "browser"),
            CommandResult::Success(_)
        ));

        let host_task = tokio::spawn(async move {
            let request = host.recv().await.expect("browser start request");
            request.respond(Err(crate::browser::BrowserError::new(
                crate::browser::BrowserErrorKind::InvalidRequest,
                "Browser tab limit reached",
            )));
        });
        editor.dispatch_pending_intents().await;
        host_task.await.unwrap();

        assert_eq!(
            editor.status_message(),
            "Could not open embedded browser: Browser tab limit reached"
        );
    }

    #[test]
    fn browser_command_reports_an_unavailable_frontend() {
        let mut editor = Editor::new();

        let result = execute_command(&mut editor, "browser");

        assert!(matches!(
            result,
            CommandResult::Error(ref error)
                if error.error == "Could not open embedded browser: The embedded browser is unavailable in this frontend"
        ));
    }

    #[test]
    fn openwin_queues_a_directory_picker_for_the_frontend() {
        let mut editor = Editor::new();

        assert!(matches!(
            execute_command(&mut editor, "openwin"),
            CommandResult::Success(_)
        ));

        let pending = editor.take_pending_window_open().expect("window request");
        assert_eq!(pending.path, None);
        assert!(editor.take_pending_window_open().is_none());
    }

    #[test]
    fn openwin_forwards_an_explicit_project_directory() {
        let mut editor = Editor::new();

        assert!(matches!(
            execute_command(&mut editor, "openwin  ~/projects/ovim  "),
            CommandResult::Success(_)
        ));

        // The path stays verbatim: expansion and validation belong to the
        // frontend that actually opens the window.
        let pending = editor.take_pending_window_open().expect("window request");
        assert_eq!(pending.path.as_deref(), Some("~/projects/ovim"));
    }

    #[test]
    fn openwin_with_a_blank_argument_falls_back_to_the_picker() {
        let mut editor = Editor::new();

        assert!(matches!(
            execute_command(&mut editor, "openwin   "),
            CommandResult::Success(_)
        ));

        let pending = editor.take_pending_window_open().expect("window request");
        assert_eq!(pending.path, None);
    }

    /// OV-00331: `:qa` used to check only the CURRENT buffer, silently
    /// discarding hidden modified buffers (e.g. files edited by a multi-file
    /// rename). Vim semantics verified in `nvim --clean` (2026-08-14): with
    /// buffer 1 modified and an unmodified buffer 2 current, `:qa` fails
    /// with "E37: No write since last change"; `:qa!` quits.
    #[tokio::test(flavor = "multi_thread")]
    async fn qa_refuses_when_a_non_current_buffer_is_modified() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, "aaa\n").unwrap();
        std::fs::write(&second, "bbb\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&first).await.unwrap();
        editor
            .buffer_mut()
            .insert_text_at(0, CharCol::ZERO, "changed ");
        editor.load_file_async(&second).await.unwrap();
        assert!(
            !editor.is_modified(),
            "current buffer must be clean so only the hidden buffer blocks"
        );

        let result = execute_command(&mut editor, "qa");
        assert!(
            matches!(result, CommandResult::Error(ref e) if e.error.contains("No write since last change")),
            "unexpected result: {result:?}"
        );
        assert!(!editor.should_quit());

        let result = execute_command(&mut editor, "qa!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(editor.should_quit());
    }

    /// OV-00331: `:q` on the last window exits the editor, so hidden
    /// modified buffers must block it exactly like `:qa`. Verified in
    /// `nvim --clean` (2026-08-14): fails with "E37: No write since last
    /// change".
    #[tokio::test(flavor = "multi_thread")]
    async fn q_on_last_window_refuses_when_a_hidden_buffer_is_modified() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, "aaa\n").unwrap();
        std::fs::write(&second, "bbb\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&first).await.unwrap();
        editor
            .buffer_mut()
            .insert_text_at(0, CharCol::ZERO, "changed ");
        editor.load_file_async(&second).await.unwrap();

        let result = execute_command(&mut editor, "q");
        assert!(
            matches!(result, CommandResult::Error(ref e) if e.error.contains("No write since last change")),
            "unexpected result: {result:?}"
        );
        assert!(!editor.should_quit());
    }

    /// OV-00331: `:wa` had no handler at all (it only appeared as a
    /// completion candidate). Vim semantics verified in `nvim --clean`
    /// (2026-08-14): `:wa` writes every modified named buffer.
    #[tokio::test(flavor = "multi_thread")]
    async fn wa_writes_all_named_modified_buffers() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, "aaa\n").unwrap();
        std::fs::write(&second, "bbb\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&first).await.unwrap();
        editor.buffer_mut().insert_text_at(0, CharCol::ZERO, "one ");
        editor.load_file_async(&second).await.unwrap();
        editor.buffer_mut().insert_text_at(0, CharCol::ZERO, "two ");

        let result = execute_command(&mut editor, "wa");
        assert!(
            matches!(result, CommandResult::Success(_)),
            "unexpected result: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one aaa\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two bbb\n");
        assert!(!editor.any_buffer_modified());

        // With everything written, :qa proceeds.
        let result = execute_command(&mut editor, "qa");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(editor.should_quit());
    }

    /// External review on OV-00331: a REAL on-disk file whose basename looks
    /// like a scratch buffer (`[draft]`) must still be written by `:wa` and
    /// must still block `:qa` — the bracket-name heuristic alone excluded it
    /// from both, silently losing the changes on quit. Scratch buffers never
    /// exist on disk, which is the disambiguator.
    #[tokio::test(flavor = "multi_thread")]
    async fn real_file_with_bracket_name_is_written_and_blocks_quit() {
        let temp = tempfile::tempdir().unwrap();
        let bracketed = temp.path().join("[draft]");
        std::fs::write(&bracketed, "aaa\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&bracketed).await.unwrap();
        editor.buffer_mut().insert_text_at(0, CharCol::ZERO, "one ");

        assert!(
            editor.any_buffer_modified(),
            "a modified real [draft] file must count as modified"
        );
        let result = execute_command(&mut editor, "qa");
        assert!(
            matches!(result, CommandResult::Error(_)),
            ":qa must refuse while the real [draft] file is modified: {result:?}"
        );

        let result = execute_command(&mut editor, "wa");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(std::fs::read_to_string(&bracketed).unwrap(), "one aaa\n");
        assert!(!editor.any_buffer_modified());
    }

    /// External review round 2: a real bracket-named file must KEEP its quit
    /// protection when an external process deletes it after load — the
    /// buffer was loaded from disk (file_mtime recorded), so a vanished path
    /// must not reclassify it as a scratch buffer.
    #[tokio::test(flavor = "multi_thread")]
    async fn externally_deleted_bracket_file_still_blocks_quit() {
        let temp = tempfile::tempdir().unwrap();
        let bracketed = temp.path().join("[draft]");
        std::fs::write(&bracketed, "aaa\n").unwrap();

        let mut editor = Editor::new();
        editor.load_file_async(&bracketed).await.unwrap();
        editor.buffer_mut().insert_text_at(0, CharCol::ZERO, "one ");

        std::fs::remove_file(&bracketed).unwrap();

        assert!(
            editor.any_buffer_modified(),
            "externally deleted [draft] must still count as modified"
        );
        let result = execute_command(&mut editor, "qa");
        assert!(
            matches!(result, CommandResult::Error(_)),
            ":qa must refuse — the only copy of the edits is this buffer: {result:?}"
        );
    }

    /// Vim semantics verified in `nvim --clean` (2026-08-14): `:wa` with a
    /// modified unnamed buffer reports "E141: No file name for buffer N"
    /// and STILL writes the named buffers.
    #[tokio::test(flavor = "multi_thread")]
    async fn wa_reports_e141_for_unnamed_buffer_but_still_writes_named() {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named.txt");
        std::fs::write(&named, "aaa\n").unwrap();

        let mut editor = Editor::new();
        // Buffer 1 is the initial unnamed buffer — modify it.
        editor
            .buffer_mut()
            .insert_text_at(0, CharCol::ZERO, "draft");
        editor.load_file_async(&named).await.unwrap();
        editor
            .buffer_mut()
            .insert_text_at(0, CharCol::ZERO, "changed ");

        let result = execute_command(&mut editor, "wa");
        assert!(
            matches!(result, CommandResult::Error(ref e) if e.error.contains("E141: No file name for buffer 1")),
            "unexpected result: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&named).unwrap(),
            "changed aaa\n",
            "named buffer must be written despite the unnamed-buffer error"
        );
    }

    #[test]
    fn session_start_rejects_overlong_name() {
        let mut editor = Editor::new();
        let name = "x".repeat(65);

        let result = execute_command(&mut editor, &format!("session start {name}"));
        assert!(
            matches!(result, CommandResult::Error(ref e) if e.error.contains("Invalid session name")),
            "65-char name must be rejected, got: {result:?}"
        );
    }

    #[test]
    fn session_start_rejects_punctuation_only_name() {
        let mut editor = Editor::new();

        let result = execute_command(&mut editor, "session start !!!...");
        assert!(
            matches!(result, CommandResult::Error(ref e) if e.error.contains("Invalid session name")),
            "punctuation-only name must be rejected, got: {result:?}"
        );
    }

    #[test]
    fn session_start_accepts_valid_names_past_validation() {
        // No API port is set, so a valid name proceeds past validation and
        // fails on the API-server check instead — proving the name itself
        // was accepted without touching the real session directory.
        let max_len = "y".repeat(64);
        for name in ["dev", "my_session-2", max_len.as_str()] {
            let mut editor = Editor::new();
            let result = execute_command(&mut editor, &format!("session start {name}"));
            assert!(
                matches!(result, CommandResult::Error(ref e) if e.error.contains("do not expose the automation API")),
                "valid name {name:?} must pass validation, got: {result:?}"
            );
        }
    }
}
