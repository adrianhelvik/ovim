use crate::command_result::CommandResult;
use crate::edit::Edit;
use crate::editor::path_completion::extract_path_from_command;
use crate::editor::{CursorPos, Editor, Mode};
use crate::unicode::{CharCol, GraphemeCol};
use crate::{KeyCode, KeyEvent};
use anyhow::Result;

mod pattern;
mod range;
mod shell;

use pattern::{handle_global_command, handle_substitute_command};
pub use range::parse_range;
use range::{parse_range_endpoint_with_status, parse_range_with_status};
use shell::{handle_read_shell_command, handle_shell_command, handle_write_to_command};

/// Handles input in Command mode
pub fn handle_command_mode(editor: &mut Editor, key_event: KeyEvent) -> Result<()> {
    match key_event.code {
        KeyCode::Char(ch) => {
            editor.append_to_command_line(ch);
            update_path_completion(editor);
        }
        KeyCode::Backspace => {
            if editor.command_line().is_empty() {
                editor.path_completion_mut().hide();
                editor.set_mode(Mode::Normal);
            } else {
                editor.backspace_command_line();
                update_path_completion(editor);
            }
        }
        KeyCode::Delete => {
            editor.delete_command_line_char();
            update_path_completion(editor);
        }
        KeyCode::Tab => {
            handle_tab_completion(editor, false);
        }
        KeyCode::BackTab => {
            handle_tab_completion(editor, true);
        }
        KeyCode::Up => {
            if editor.path_completion().is_visible() {
                editor.path_completion_mut().select_previous();
                accept_selected_into_command_line(editor);
            } else {
                editor.history_prev();
            }
        }
        KeyCode::Down => {
            if editor.path_completion().is_visible() {
                editor.path_completion_mut().select_next();
                accept_selected_into_command_line(editor);
            } else {
                editor.history_next();
            }
        }
        KeyCode::Left => {
            editor.move_command_cursor_left();
        }
        KeyCode::Right => {
            editor.move_command_cursor_right();
        }
        KeyCode::Home => {
            editor.move_command_cursor_home();
        }
        KeyCode::End => {
            editor.move_command_cursor_end();
        }
        KeyCode::Enter => {
            if editor.path_completion().is_visible() {
                if editor.path_completion().selected_is_dir() {
                    // Directory: accept into command line, refresh completions, don't execute.
                    if let Some(new_path) = editor.path_completion().accept() {
                        let cmd = editor.command_line().to_string();
                        if let Some(path_portion) = extract_path_from_command(&cmd) {
                            let prefix_len = cmd.len() - path_portion.len();
                            let new_cmd = format!("{}{}", &cmd[..prefix_len], new_path);
                            editor.set_command_line(&new_cmd);
                            let cwd = std::env::current_dir().unwrap_or_default();
                            editor.path_completion_mut().update(&new_path, &cwd);
                        }
                    }
                } else {
                    // File: accept into command line, then execute.
                    if let Some(new_path) = editor.path_completion().accept() {
                        let cmd = editor.command_line().to_string();
                        if let Some(path_portion) = extract_path_from_command(&cmd) {
                            let prefix_len = cmd.len() - path_portion.len();
                            let new_cmd = format!("{}{}", &cmd[..prefix_len], new_path);
                            editor.set_command_line(&new_cmd);
                        }
                    }
                    editor.path_completion_mut().hide();
                    editor.add_command_to_history();
                    execute_command(editor)?;
                    editor.clear_command_line();
                    if editor.mode() == Mode::Command {
                        editor.set_mode(Mode::Normal);
                    }
                }
            } else {
                editor.add_command_to_history();
                execute_command(editor)?;
                editor.clear_command_line();
                if editor.mode() == Mode::Command {
                    editor.set_mode(Mode::Normal);
                }
            }
        }
        KeyCode::Esc => {
            editor.path_completion_mut().hide();
            editor.clear_command_line();
            editor.set_mode(Mode::Normal);
        }
        _ => {}
    }
    Ok(())
}

fn expand_tilde_in_path(path: &str) -> std::path::PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(path.trim_start_matches("~/"));
        }
    }
    std::path::PathBuf::from(path)
}

/// Updates the path completion popup based on current command line content.
fn update_path_completion(editor: &mut Editor) {
    let cmd = editor.command_line().to_string();
    if let Some(path_portion) = extract_path_from_command(&cmd) {
        let path_portion = path_portion.to_string();
        let cwd = std::env::current_dir().unwrap_or_default();
        editor.path_completion_mut().update(&path_portion, &cwd);
    } else {
        editor.path_completion_mut().hide();
    }
}

/// Known command names for Tab completion.
const COMMAND_NAMES: &[&str] = &[
    "bd",
    "bdelete",
    "buffer",
    "buffers",
    "cd",
    "cclose",
    "cfirst",
    "clast",
    "close",
    "cn",
    "cnext",
    "copen",
    "colorscheme",
    "cp",
    "cprev",
    "cq",
    "cquit",
    "delmarks",
    "e",
    "edit",
    "f",
    "file",
    "help",
    "hi",
    "highlight",
    "history",
    "lcd",
    "ls",
    "LspInstall",
    "LspManager",
    "map",
    "marks",
    "messages",
    "nmap",
    "nohlsearch",
    "noh",
    "norm",
    "normal",
    "noremap",
    "nmapclear",
    "only",
    "pwd",
    "q",
    "qa",
    "quit",
    "quitall",
    "reg",
    "registers",
    "reload",
    "saveas",
    "se",
    "set",
    "session",
    "sort",
    "source",
    "sp",
    "split",
    "tabe",
    "tabedit",
    "tabclose",
    "tabmove",
    "tabnext",
    "tabprev",
    "unmap",
    "unlet",
    "vmap",
    "vsp",
    "vsplit",
    "w",
    "wa",
    "wall",
    "wq",
    "wqa",
    "write",
    "writeall",
    "x",
    "xa",
];

/// State for command name Tab completion cycling.
struct CmdCompletion {
    matches: Vec<&'static str>,
    index: usize,
    original_prefix: String,
}

thread_local! {
    static CMD_COMPLETION: std::cell::RefCell<Option<CmdCompletion>> = const { std::cell::RefCell::new(None) };
}

/// Handles Tab (forward=false) or BackTab (backward=true) for path completion.
fn handle_tab_completion(editor: &mut Editor, backward: bool) {
    let cmd = editor.command_line().to_string();
    let trimmed = cmd.trim_start();

    // If the command line has no space, we're completing a command name.
    if !trimmed.contains(' ') {
        handle_command_name_completion(editor, trimmed, backward);
        return;
    }

    // Otherwise, fall through to path completion.
    if !editor.path_completion().is_visible() {
        // Try to trigger completion from current command line.
        update_path_completion(editor);
        if !editor.path_completion().is_visible() {
            return;
        }
        // First Tab after triggering: accept entry[0] without cycling.
    } else if editor.path_completion().tab_accepted() {
        // Already visible and Tab was used before — cycle selection.
        if backward {
            editor.path_completion_mut().select_previous();
        } else {
            editor.path_completion_mut().select_next();
        }
    }
    // else: popup was visible from typing but Tab hasn't accepted yet —
    // accept the current selection (entry[0]) without cycling.

    editor.path_completion_mut().set_tab_accepted();

    // Accept the selected entry: replace path portion in command line.
    let accepted = editor.path_completion().accept();
    if let Some(new_path) = accepted {
        let cmd = editor.command_line().to_string();
        if let Some(path_portion) = extract_path_from_command(&cmd) {
            let prefix_len = cmd.len() - path_portion.len();
            let new_cmd = format!("{}{}", &cmd[..prefix_len], new_path);
            editor.set_command_line(&new_cmd);

            // If we just completed a directory, refresh entries for its contents.
            if new_path.ends_with('/') {
                let cwd = std::env::current_dir().unwrap_or_default();
                editor.path_completion_mut().update(&new_path, &cwd);
            }
        }
    }
}

/// Handle Tab completion for command names (first word of command line).
fn handle_command_name_completion(editor: &mut Editor, prefix: &str, backward: bool) {
    CMD_COMPLETION.with(|cell| {
        let mut state = cell.borrow_mut();

        // Check if we're continuing a previous completion cycle
        let continuing = state.as_ref().is_some_and(|s| {
            s.original_prefix == prefix || {
                // Also continue if the command line matches a previous completion result
                s.matches.contains(&prefix)
            }
        });

        if continuing {
            let s = state.as_mut().unwrap();
            if backward {
                s.index = if s.index == 0 {
                    s.matches.len() // wraps to original prefix
                } else {
                    s.index - 1
                };
            } else {
                s.index += 1;
                if s.index > s.matches.len() {
                    s.index = 0;
                }
            }
            // index == matches.len() means show original prefix
            if s.index == s.matches.len() {
                editor.set_command_line(&s.original_prefix);
            } else {
                editor.set_command_line(s.matches[s.index]);
            }
        } else {
            // Build new completion list
            let matches: Vec<&'static str> = COMMAND_NAMES
                .iter()
                .filter(|cmd| cmd.starts_with(prefix))
                .copied()
                .collect();

            if matches.is_empty() {
                *state = None;
                return;
            }

            if matches.len() == 1 {
                // Unique match — complete it directly
                editor.set_command_line(matches[0]);
                *state = None;
                return;
            }

            // Multiple matches — start cycling from first
            let idx = if backward { matches.len() - 1 } else { 0 };
            editor.set_command_line(matches[idx]);
            *state = Some(CmdCompletion {
                matches,
                index: idx,
                original_prefix: prefix.to_string(),
            });
        }
    });
}

/// Accepts the currently selected path completion entry and updates the command line text.
fn accept_selected_into_command_line(editor: &mut Editor) {
    if let Some(new_path) = editor.path_completion().accept() {
        let cmd = editor.command_line().to_string();
        if let Some(path_portion) = extract_path_from_command(&cmd) {
            let prefix_len = cmd.len() - path_portion.len();
            let new_cmd = format!("{}{}", &cmd[..prefix_len], new_path);
            editor.set_command_line(&new_cmd);
        }
    }
}

/// Finds the byte index where the command name begins — the end of the leading
/// `[range]` prefix. The prefix may include mark addresses (`'a`, `'<`, `'>`),
/// whose following letter must NOT be mistaken for the command name. Skipping
/// the char after each `'` keeps `:'a,'bd` parsing as range `'a,'b` + command
/// `d` rather than range `'` + command `a,'bd`. Returns None when the whole
/// string is a bare range with no command.
fn command_name_start(command: &str) -> Option<usize> {
    let mut chars = command.char_indices();
    while let Some((idx, ch)) = chars.next() {
        if ch == '\'' {
            // Mark address: consume the mark-name char that follows.
            chars.next();
            continue;
        }
        if ch.is_alphabetic() {
            return Some(idx);
        }
    }
    None
}

/// Extracts the destination address of a `:copy`/`:t` or `:move`/`:m` command
/// from the command portion, accepting both the spaced (`t 3`) and the unspaced
/// (`t3`, `t0`, `t.`) Vim forms. Returns `None` when `cmd_part` isn't this
/// command — a trailing letter (`tabnew`, `marks`) is rejected so it doesn't get
/// mistaken for a copy/move to a mark.
fn parse_copy_move_dest<'a>(cmd_part: &'a str, long: &str, short: &str) -> Option<&'a str> {
    for word in [long, short] {
        if let Some(rest) = cmd_part.strip_prefix(word) {
            if rest.is_empty() {
                return Some("");
            }
            let first = rest.chars().next().unwrap();
            if first.is_whitespace()
                || first.is_ascii_digit()
                || matches!(first, '.' | '$' | '\'' | '+' | '-' | '/' | '?')
            {
                return Some(rest.trim());
            }
        }
    }
    None
}

/// Executes a command string directly (used for API/Lua commands)
pub fn execute_command_string(editor: &mut Editor, command: &str) -> Result<()> {
    execute_command_impl(editor, command)
}

/// Executes a command string on behalf of the headless API / CLI, returning a
/// structured [`CommandResult`].
///
/// The headless `exec` path historically called [`crate::commands::execute_command`]
/// directly, which only knows the "standard" ex-commands (`:w`, `:q`, `:set`, …).
/// Substitute (`:s`), global (`:g`/`:v`), ranges, and `:d`/`:y` live in the
/// interactive command handler and were therefore unreachable headlessly — an
/// `exec ':%s/a/b/'` returned "Not an editor command" while the same keys typed
/// interactively worked. Routing through the same dispatcher the interactive
/// command line uses keeps the two paths in parity.
pub fn execute_command_string_api(editor: &mut Editor, command: &str) -> CommandResult {
    use crate::command_result::{err, ok, ok_silent};

    let command = command.trim();

    // Standard commands return a structured result we forward verbatim — this
    // preserves messages like line counts and errors like "No write since last
    // change". Only when the standard dispatcher reports the command as unknown
    // do we fall through to the richer interactive handler.
    let result = crate::commands::execute_command(editor, command);
    let is_unknown = matches!(
        &result,
        CommandResult::Error(e) if e.error.contains("Not an editor command")
    );
    if !is_unknown {
        return result;
    }

    // The standard dispatcher performs no mutation when it doesn't recognize a
    // command, so it is safe to re-run the full interactive handler (which
    // re-checks the standard dispatcher and then handles substitute / global /
    // range / etc.). That handler reports outcomes on the status line rather
    // than returning them, so clear the line first and read it back afterwards.
    editor.set_status_message(String::new());
    match execute_command_string(editor, command) {
        Ok(()) => {
            let status = editor.status_message().trim().to_string();
            if status.is_empty() {
                ok_silent()
            } else if is_vim_error_status(&status) {
                err(status)
            } else {
                ok(status)
            }
        }
        Err(e) => err(e.to_string()),
    }
}

/// Vim surfaces command errors on the status line using the `E<number>:`
/// convention (`E146`, `E20`, `E486`, …). Map those back to an API error even
/// though the editor itself treats them as ordinary status messages.
fn is_vim_error_status(status: &str) -> bool {
    matches!(
        status.strip_prefix('E'),
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit())
    )
}

/// Executes a command from the command line
fn execute_command(editor: &mut Editor) -> Result<()> {
    let command = editor.command_line().trim().to_string();
    execute_command_impl(editor, &command)
}

/// Internal command execution implementation
fn execute_command_impl(editor: &mut Editor, command: &str) -> Result<()> {
    let command = command.trim();

    // Handle command chaining with |
    // BUG FIX: Don't split on | for substitute, global, or vglobal commands
    // because | can appear in patterns like :s/foo|bar/baz/
    if command.contains('|') && !is_command_with_pattern(command) {
        // Simple split for non-substitute commands
        for part in command.split('|') {
            let part = part.trim();
            if !part.is_empty() {
                execute_command_single(editor, part)?;
            }
        }
        return Ok(());
    }

    execute_command_single(editor, command)
}

/// Helper: Check if command contains patterns that may include | characters.
/// Returns true for substitute (:s), global (:g), and vglobal (:v) commands.
///
/// We strip leading range characters (digits, commas, %, ., $, ', <, >) then
/// check if the command part starts with s/, g/, g!/, or v/.  This avoids
/// false positives on commands like `:e files/foo | set number`.
fn is_command_with_pattern(command: &str) -> bool {
    let trimmed = command.trim();
    // Skip past range prefix: digits, commas, %, ., $, ', <, >, +, -, spaces
    let cmd = trimmed.trim_start_matches(|c: char| c.is_ascii_digit() || ",%.$ '<>+-".contains(c));
    cmd.starts_with("s/")
        || cmd.starts_with("g/")
        || cmd.starts_with("g!/")
        || cmd.starts_with("v/")
}

/// Execute a single command (no chaining)
fn execute_command_single(editor: &mut Editor, command: &str) -> Result<()> {
    // Update the : register with the command
    editor.registers_mut().set_last_command(command.to_string());

    // Intercept plain :!cmd in TUI mode — queue for the event loop so it
    // runs with full terminal access (outside alternate screen).
    // Filter commands (:range!cmd) and :r/:w !cmd are NOT intercepted here;
    // they're handled below by the standard command flow.
    if let Some(shell_cmd) = command.strip_prefix('!') {
        use super::shell_expansion::expand_shell_command;
        let shell_cmd = shell_cmd.trim();
        let cmd = if shell_cmd.is_empty() {
            // Bare :! — repeat last
            match editor.build.last_shell_command.clone() {
                Some(last) => last,
                None => {
                    editor.set_status_message("No previous shell command".to_string());
                    return Ok(());
                }
            }
        } else {
            let current_file = editor.buffer().file_path().unwrap_or("").to_string();
            let alternate_file = editor.registers().get(Some('#'));
            expand_shell_command(shell_cmd, &current_file, &alternate_file)
        };
        editor.build.last_shell_command = Some(cmd.clone());
        editor.build.pending_shell_command =
            Some(crate::editor::PendingShellCommand { command: cmd });
        return Ok(());
    }

    // First, try to delegate to the top-level commands module which has all the standard commands
    let response = crate::commands::execute_command(editor, command);
    match response {
        CommandResult::Success(success_resp) => {
            // Command executed successfully
            if let Some(msg) = success_resp.message {
                // Multi-line messages go to hover popup, single-line to status bar
                let msg = msg.into_owned();
                if msg.contains('\n') {
                    editor.set_hover_info(msg);
                } else {
                    editor.set_status_message(msg);
                }
            }
            return Ok(());
        }
        CommandResult::Error(err_resp) => {
            // Check if it's an "unknown command" error
            if err_resp.error.contains("Not an editor command") {
                // Fall through to custom input-specific command handling below
            } else {
                // It's a real error from a known command
                editor.set_status_message(err_resp.error);
                return Ok(());
            }
        }
    }

    // If we reach here, it's an unknown command - try custom input-specific handling

    // Handle :w !cmd (write to command stdin) - must be checked before range parsing
    // Note: :w! is force write (handled above), :w !cmd (with space) writes to command stdin
    if let Some(write_cmd) = command
        .strip_prefix("w !")
        .or_else(|| command.strip_prefix("write !"))
    {
        return handle_write_to_command(editor, "", write_cmd.trim());
    }

    // Handle range + w !cmd (e.g., :'<,'>w !pbcopy)
    // This requires checking if the command ends with "w !..." pattern after a range
    if command.contains("w !") || command.contains("write !") {
        // Find where 'w !' or 'write !' starts
        if let Some(pos) = command.find("w !").or_else(|| command.find("write !")) {
            let range_str = &command[..pos];
            let shell_cmd = if command[pos..].starts_with("write !") {
                &command[pos + 7..]
            } else {
                &command[pos + 3..]
            };
            return handle_write_to_command(editor, range_str.trim(), shell_cmd.trim());
        }
    }

    // Handle :r !cmd (read from command) - must be checked before range parsing
    // Note: This is different from file reading :r filename
    if let Some(read_cmd) = command
        .strip_prefix("r !")
        .or_else(|| command.strip_prefix("read !"))
    {
        return handle_read_shell_command(editor, "", read_cmd.trim());
    }

    // Handle range + r !cmd (e.g., :0r !cmd)
    if command.contains("r !") || command.contains("read !") {
        if let Some(pos) = command.find("r !").or_else(|| command.find("read !")) {
            let range_str = &command[..pos];
            let shell_cmd = if command[pos..].starts_with("read !") {
                &command[pos + 6..]
            } else {
                &command[pos + 3..]
            };
            return handle_read_shell_command(editor, range_str.trim(), shell_cmd.trim());
        }
    }

    // First, try to parse range from command
    // Format: :[range]command
    //
    // BUG FIX: '!' is ambiguous:
    // - Shell: :[range]!cmd  (the '!' starts the command)
    // - Global invert: :g!/pat/cmd (the '!' is part of the command name)
    //
    // Treat '!' as the command separator only when it appears *before* the
    // first alphabetic command character (i.e. it's the command itself).
    let cmd_start = command_name_start(command);
    let bang_split = command.find('!').and_then(|exclaim_idx| {
        let is_shell_separator = match cmd_start {
            None => true,
            Some(alpha_idx) => exclaim_idx < alpha_idx,
        };
        if is_shell_separator {
            Some(exclaim_idx)
        } else {
            None
        }
    });

    let (range_str, cmd_part) = if let Some(exclaim_idx) = bang_split {
        (&command[..exclaim_idx], &command[exclaim_idx..])
    } else if let Some(cmd_start) = cmd_start {
        (&command[..cmd_start], &command[cmd_start..])
    } else {
        (command, "")
    };

    // Handle goto line (just a number or range without command)
    if cmd_part.is_empty() && !range_str.is_empty() {
        if let Some((start_line, _end_line)) = parse_range_with_status(editor, range_str, None) {
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(start_line, GraphemeCol::ZERO);
            return Ok(());
        }
    }

    // Handle ranged delete command (:d or :delete)
    if cmd_part == "d" || cmd_part == "delete" {
        if let Some((start_line, end_line)) = parse_range_with_status(editor, range_str, None) {
            let cursor_before = editor.cursor_position();
            let (deleted_text, edits) = editor.buffer_mut().record(|buf| {
                buf.delete_range(start_line, CharCol::ZERO, end_line + 1, CharCol::ZERO)
            });

            // Store in register (use delete, which updates " and numbered regs but not 0)
            editor.delete_to_register(deleted_text.clone());

            // Position cursor at start of deleted range
            let new_cursor_line = start_line.min(editor.buffer().line_count().saturating_sub(1));
            editor
                .buffer_mut()
                .cursor_mut()
                .set_position(new_cursor_line, GraphemeCol::ZERO);

            if !edits.is_empty() {
                let cursor_after = CursorPos::new(new_cursor_line, GraphemeCol::ZERO);
                editor.push_recorded_undo(edits, cursor_before, cursor_after);
            }

            return Ok(());
        }
    }

    // Handle ranged yank command (:y or :yank)
    if cmd_part == "y" || cmd_part == "yank" {
        if let Some((start_line, end_line)) = parse_range_with_status(editor, range_str, None) {
            // Re-add the terminator `line_text` strips so register content
            // is linewise, matching `Y`/`yy`.
            let mut yanked_text = String::new();
            for line_idx in start_line..=end_line {
                if let Some(line) = editor.buffer().line_text(line_idx) {
                    yanked_text.push_str(&line);
                    yanked_text.push('\n');
                }
            }

            // Store in register (use yank, which updates " and 0)
            editor.yank_to_register(yanked_text);

            return Ok(());
        }
    }

    // Handle :sort command (sorts lines in range)
    if cmd_part == "sort" || cmd_part.starts_with("sort ") {
        if let Some((start_line, end_line)) = parse_range_with_status(editor, range_str, None) {
            let reverse = cmd_part.contains('!') || cmd_part.contains(" r");
            let numeric = cmd_part.contains(" n");
            let unique = cmd_part.contains(" u");
            let ignore_case = cmd_part.contains(" i");

            // Collect lines (terminator-stripped — we'll re-add `\n` when
            // we serialize back to a single block below).
            let mut lines: Vec<String> = (start_line..=end_line)
                .filter_map(|idx| editor.buffer().line_text(idx).map(|l| l.into_owned()))
                .collect();

            // Bug 3 fix: Use stable sort (Vim's :sort is stable)
            // sort_by is stable, but sort() uses unstable sort internally
            if numeric {
                // Sort by leading number
                lines.sort_by(|a, b| {
                    let num_a: i64 = a
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let num_b: i64 = b
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    num_a.cmp(&num_b)
                });
            } else if ignore_case {
                lines.sort_by_key(|a| a.to_lowercase());
            } else {
                // Use stable sort for consistency with Vim
                lines.sort();
            }

            if reverse {
                lines.reverse();
            }

            if unique {
                lines.dedup();
            }

            // Replace the range with sorted lines
            let cursor_before = CursorPos::new(
                editor.buffer().cursor().line(),
                editor.buffer().cursor().col(),
            );

            // Get the char positions for the range
            let start_char = editor.buffer().rope().line_to_char(start_line);
            let end_char = if end_line + 1 < editor.buffer().line_count() {
                editor.buffer().rope().line_to_char(end_line + 1)
            } else {
                editor.buffer().rope().len_chars()
            };

            // Store original text for undo
            let original_text = editor
                .buffer()
                .rope()
                .slice(start_char..end_char)
                .to_string();

            // Remove old lines
            editor.buffer_mut().rope_mut().remove(start_char..end_char);

            // Insert sorted lines, re-adding the terminators we stripped.
            let mut new_text = String::new();
            for line in &lines {
                new_text.push_str(line);
                new_text.push('\n');
            }
            editor.buffer_mut().rope_mut().insert(start_char, &new_text);

            let mut edits = Vec::new();
            if !original_text.is_empty() {
                edits.push(Edit::Delete {
                    offset: start_char,
                    text: original_text,
                });
            }
            if !new_text.is_empty() {
                edits.push(Edit::Insert {
                    offset: start_char,
                    text: new_text.clone(),
                });
            }
            if !edits.is_empty() {
                editor.push_recorded_undo(edits, cursor_before, cursor_before);
            }

            let sorted_count = lines.len();
            editor.set_status_message(format!("{} lines sorted", sorted_count));
            return Ok(());
        }
    }

    // Handle :copy or :t command (copy lines to destination)
    // Format: :[range]copy {address} or :[range]t {address}
    // (helper `parse_copy_move_dest` accepts both the spaced and unspaced forms)
    if let Some(dest_str) = parse_copy_move_dest(cmd_part, "copy", "t") {
        if dest_str.is_empty() {
            editor.set_status_message("E488: Trailing characters".to_string());
            return Ok(());
        }

        if let Some((start_line, end_line)) = parse_range_with_status(editor, range_str, None) {
            // Parse destination address
            if let Some(dest_line) =
                parse_range_endpoint_with_status(editor, dest_str, Some("E14: Invalid address"))
            {
                // Collect lines to copy. `line_text` strips terminators by
                // design — re-add `\n` so the inserted block keeps its
                // line breaks.
                let mut text_to_insert = String::new();
                let mut lines_copied = 0usize;
                for idx in start_line..=end_line {
                    if let Some(line) = editor.buffer().line_text(idx) {
                        text_to_insert.push_str(&line);
                        text_to_insert.push('\n');
                        lines_copied += 1;
                    }
                }

                // Insert after destination line. Address 0 is Vim's "before the
                // first line" — insert at buffer index 0 rather than after line 1
                // (both "0" and "1" parse to 0-based line 0, so disambiguate here).
                let insert_line = if dest_str == "0" { 0 } else { dest_line + 1 };
                let cursor_before = CursorPos::new(
                    editor.buffer().cursor().line(),
                    editor.buffer().cursor().col(),
                );

                let insert_char = if insert_line < editor.buffer().line_count() {
                    editor.buffer().rope().line_to_char(insert_line)
                } else {
                    editor.buffer().rope().len_chars()
                };

                // Add newline if we're at end of file
                let text =
                    if insert_line >= editor.buffer().line_count() && !text_to_insert.is_empty() {
                        format!("\n{}", text_to_insert)
                    } else {
                        text_to_insert.clone()
                    };

                editor.buffer_mut().rope_mut().insert(insert_char, &text);

                // Move cursor to first copied line
                let cursor_after = CursorPos::new(insert_line, GraphemeCol::ZERO);
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(insert_line, GraphemeCol::ZERO);
                editor.push_recorded_undo(
                    vec![Edit::Insert {
                        offset: insert_char,
                        text: text.clone(),
                    }],
                    cursor_before,
                    cursor_after,
                );

                let count = lines_copied;
                editor.set_status_message(format!(
                    "{} line{} copied",
                    count,
                    if count == 1 { "" } else { "s" }
                ));
                return Ok(());
            }
        }
    }

    // Handle :move or :m command (move lines to destination)
    // Format: :[range]move {address} or :[range]m {address}
    if let Some(dest_str) = parse_copy_move_dest(cmd_part, "move", "m") {
        if dest_str.is_empty() {
            editor.set_status_message("E488: Trailing characters".to_string());
            return Ok(());
        }

        if let Some((start_line, end_line)) = parse_range_with_status(editor, range_str, None) {
            // Parse destination address
            if let Some(mut dest_line) =
                parse_range_endpoint_with_status(editor, dest_str, Some("E14: Invalid address"))
            {
                // Bug 2 fix: Check for invalid moves (moving to within the range)
                // Should be <= end_line to prevent moving into self
                if dest_line >= start_line && dest_line <= end_line {
                    editor.set_status_message("E134: Move lines into themselves".to_string());
                    return Ok(());
                }

                let cursor_before = CursorPos::new(
                    editor.buffer().cursor().line(),
                    editor.buffer().cursor().col(),
                );

                // Collect lines to move. `line_text` strips terminators by
                // design — re-add `\n` so the moved block keeps its line
                // breaks.
                let mut text_to_move = String::new();
                let mut line_count = 0usize;
                for idx in start_line..=end_line {
                    if let Some(line) = editor.buffer().line_text(idx) {
                        text_to_move.push_str(&line);
                        text_to_move.push('\n');
                        line_count += 1;
                    }
                }

                // Delete the source lines
                let start_char = editor.buffer().rope().line_to_char(start_line);
                let end_char = if end_line + 1 < editor.buffer().line_count() {
                    editor.buffer().rope().line_to_char(end_line + 1)
                } else {
                    editor.buffer().rope().len_chars()
                };
                editor.buffer_mut().rope_mut().remove(start_char..end_char);

                // Adjust destination if it was after the deleted lines
                if dest_line > end_line {
                    dest_line = dest_line.saturating_sub(line_count);
                }

                // Insert after destination line; address 0 means the top of the
                // file (Vim), so insert at index 0 instead of after line 1.
                let insert_line = if dest_str == "0" { 0 } else { dest_line + 1 };
                let insert_char = if insert_line < editor.buffer().line_count() {
                    editor.buffer().rope().line_to_char(insert_line)
                } else {
                    editor.buffer().rope().len_chars()
                };

                // Add newline if we're at end of file
                let text =
                    if insert_line >= editor.buffer().line_count() && !text_to_move.is_empty() {
                        format!("\n{}", text_to_move)
                    } else {
                        text_to_move.clone()
                    };

                editor.buffer_mut().rope_mut().insert(insert_char, &text);

                // Move cursor to first moved line
                let cursor_after = CursorPos::new(insert_line, GraphemeCol::ZERO);
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(insert_line, GraphemeCol::ZERO);
                let mut edits = Vec::new();
                if !text_to_move.is_empty() {
                    edits.push(Edit::Delete {
                        offset: start_char,
                        text: text_to_move,
                    });
                }
                if !text.is_empty() {
                    edits.push(Edit::Insert {
                        offset: insert_char,
                        text: text.clone(),
                    });
                }
                if !edits.is_empty() {
                    editor.push_recorded_undo(edits, cursor_before, cursor_after);
                }

                editor.set_status_message(format!(
                    "{} line{} moved",
                    line_count,
                    if line_count == 1 { "" } else { "s" }
                ));
                return Ok(());
            }
        }
    }

    // Handle commands with arguments
    if command.starts_with("w ") || command.starts_with("write ") {
        // :w <filename> or :write <filename> - save as
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.len() >= 2 {
            let old_path = editor.buffer().file_path().map(|s| s.to_string());
            let filename = parts[1..].join(" ");
            editor.buffer_mut().save_as(&filename)?;
            let new_path = editor.buffer().file_path().map(|s| s.to_string());
            editor.handle_file_path_transition_after_save(old_path, new_path);
            if editor.options.blame {
                editor.buffer_mut().load_git_blame();
            }
            editor.mark_saved();
            editor.mark_buffer_saved();
        }
        return Ok(());
    }

    // Handle :global and :vglobal commands (:g/pattern/command, :v/pattern/command),
    // including when prefixed by a range (:rangeg/...).
    if cmd_part.starts_with("g/") || cmd_part.starts_with("g!/") || cmd_part.starts_with("v/") {
        let range = if range_str.trim().is_empty() {
            // Vim's :global defaults to the entire buffer, not the current line.
            None
        } else {
            match parse_range_with_status(editor, range_str, Some("E14: Invalid address")) {
                Some(r) => Some(r),
                None => return Ok(()),
            }
        };

        handle_global_command(editor, cmd_part, range)?;
        return Ok(());
    }

    // Handle substitute command (:s, :%s, :'<,'>s)
    // Only treat as substitute when the command part starts with `s/`.
    if cmd_part.starts_with("s/") {
        handle_substitute_command(editor, range_str, cmd_part)?;
        return Ok(());
    }

    // Handle range-prefixed shell commands (:.!cmd, :%!cmd, :1,5!cmd)
    // Plain :!cmd is intercepted at the top of execute_command_single.
    if let Some(shell_cmd) = cmd_part.strip_prefix('!') {
        let shell_cmd = shell_cmd.trim();
        if !shell_cmd.is_empty() {
            handle_shell_command(editor, range_str, shell_cmd)?;
            return Ok(());
        }
    }

    // Only handle custom input-specific commands here.
    // Standard commands are now delegated to the top-level commands module.
    {
        // Check if it's a :r or :read command
        if let Some(target) = command
            .strip_prefix("r ")
            .or_else(|| command.strip_prefix("read "))
        {
            let target = target.trim();
            if !target.is_empty() {
                if let Some(shell_cmd) = target.strip_prefix('!') {
                    // :r !cmd - read output from shell command
                    handle_read_shell_command(editor, range_str, shell_cmd.trim())?;
                } else {
                    // :r filename - read file contents
                    let expanded_target = expand_tilde_in_path(target);
                    let display_target = expanded_target.to_string_lossy().to_string();
                    match std::fs::read_to_string(&expanded_target) {
                        Ok(contents) => {
                            // Insert contents at current cursor position
                            let cursor = editor.buffer().cursor();
                            let line = cursor.line() + 1; // Insert after current line
                            editor
                                .buffer_mut()
                                .insert_text_at(line, CharCol::ZERO, &contents);
                            editor.set_status_message(format!(
                                "Read {} lines from {}",
                                contents.lines().count(),
                                display_target
                            ));
                        }
                        Err(e) => {
                            editor.set_status_message(format!("Error reading file: {}", e));
                        }
                    }
                }
            }
            return Ok(());
        }

        // Check if it's a :b <n> or :buffer <n> command
        if let Some(buffer_num_str) = command
            .strip_prefix("b ")
            .or_else(|| command.strip_prefix("buffer "))
        {
            if let Ok(buffer_num) = buffer_num_str.trim().parse::<usize>() {
                if buffer_num > 0 {
                    // Convert from 1-indexed to 0-indexed
                    editor.switch_to_buffer(buffer_num - 1);
                }
            }
            return Ok(());
        }

        // Check if it's a :colorscheme <name> or :colo <name> command
        if let Some(scheme_name) = command
            .strip_prefix("colorscheme ")
            .or_else(|| command.strip_prefix("colo "))
        {
            match editor.set_color_scheme(scheme_name.trim()) {
                Ok(_) => {
                    let message = format!("Color scheme set to '{}'", scheme_name.trim());
                    editor.set_status_message(message);
                }
                Err(e) => {
                    let available = editor.list_color_schemes().join(", ");
                    let message = format!("{}. Available schemes: {}", e, available);
                    editor.set_status_message(message);
                }
            }
        } else if editor.status_message().is_empty() {
            // Truly unrecognized ex-command (no handler set a status). Report it
            // the way Vim does (E492) so the user gets feedback on typos instead
            // of silence, and so the headless API can surface it as an error.
            // Guarded on an empty status so a recognized command that failed and
            // already set its own error (e.g. E20 from `:copy 'z`) isn't clobbered.
            editor.set_status_message(format!("E492: Not an editor command: {}", command));
        }
    }

    Ok(())
}
