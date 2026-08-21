//! Subcommand implementations for controlling ovim sessions
#![allow(clippy::print_stdout)]

use crate::cli::FileArg;
use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::{Command, LspCommand, SessionCommand};
use crate::client::OvimClient;
use crate::session::SessionInfo;

/// Resolve a session by name (no auto-discovery)
fn resolve_session(session_name: &str) -> Result<SessionInfo> {
    SessionInfo::read(session_name).context(format!("Failed to find session '{}'", session_name))
}

fn with_resolved_session<F>(session_name: Option<&str>, f: F) -> Result<()>
where
    F: FnOnce(&SessionInfo) -> Result<()>,
{
    let session_name = resolve_default_session_name(session_name);
    let session = resolve_session(&session_name)?;
    f(&session)
}

fn resolve_default_session_name(session_name: Option<&str>) -> String {
    session_name
        .map(|s| s.to_string())
        .or_else(|| std::env::var("OVIM_SESSION").ok())
        .unwrap_or_else(|| "default".to_string())
}

fn with_temp_headless_session<F>(
    file_arg: &str,
    wait_lsp: bool,
    timeout_ms: u64,
    f: F,
) -> Result<()>
where
    F: FnOnce(&SessionInfo) -> Result<()>,
{
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    let temp_dir = tempfile::tempdir().context("Failed to create temp session dir")?;

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    let session_name = format!("oneshot-{}-{}", std::process::id(), unique);

    let exe = std::env::current_exe().context("Failed to locate ovim executable")?;
    let mut child = Command::new(exe)
        .arg(file_arg)
        .arg("--headless")
        .arg("--session")
        .arg(&session_name)
        .env("OVIM_SESSION_DIR", temp_dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to start headless ovim session")?;

    // Wait for the session file to appear.
    let start = Instant::now();
    let session = loop {
        if let Ok(Some(status)) = child.try_wait() {
            anyhow::bail!("Headless session exited early: {}", status);
        }

        match SessionInfo::read_from_dir(&session_name, temp_dir.path()) {
            Ok(info) => break info,
            Err(_) => {
                if start.elapsed() > Duration::from_secs(5) {
                    anyhow::bail!("Timed out waiting for headless session to start");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };

    let client = OvimClient::new(&session);
    if wait_lsp {
        wait_for_lsp_ready(&client, timeout_ms)?;
    }

    let result = f(&session);

    // Best-effort cleanup.
    #[cfg(unix)]
    {
        let _ = client.kill_session(&session);
    }
    let _ = child.kill();
    let _ = child.wait();

    result
}

fn wait_for_lsp_ready(client: &OvimClient, timeout_ms: u64) -> Result<()> {
    use std::thread;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    // Files whose language has no LSP (markdown, HCL) spawn zero servers.
    // Requiring a non-empty list forever would stall the full timeout and
    // then blame the servers (OV-00282) — so give startup a short grace
    // period to register servers, then treat a still-empty list as ready.
    let empty_grace = Duration::from_millis(1500.min(timeout_ms));

    loop {
        if let Ok(lsp_status) = client.get_lsp_status() {
            if lsp_status.servers.is_empty() {
                if start.elapsed() >= empty_grace {
                    return Ok(());
                }
            } else if lsp_status.servers.iter().all(|s| s.has_capabilities) {
                return Ok(());
            }
        }

        if start.elapsed() >= timeout {
            anyhow::bail!("Timeout waiting for LSP to be ready");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

/// Expand \n escape sequences in a string (consistent with send_keys)
fn expand_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') => {
                    chars.next();
                    result.push('\n');
                }
                Some('\\') => {
                    chars.next();
                    result.push('\\');
                }
                _ => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Execute a subcommand
pub fn execute_subcommand(command: Command) -> Result<()> {
    match command {
        Command::Gui { file, resume } => cmd_gui(file, resume),

        // File operations (direct file I/O, no session needed)
        Command::Edit {
            file,
            line,
            old,
            new,
            session,
        } => match session {
            Some(session) => cmd_edit_session(&session, &file, line, &old, &new),
            None => cmd_edit_file(&file, line, &old, &new),
        },
        Command::Insert {
            file,
            after,
            before,
            text,
            session,
        } => match session {
            Some(session) => cmd_insert_session(&session, &file, after, before, &text),
            None => cmd_insert_file(&file, after, before, &text),
        },
        Command::DeleteLines {
            file,
            from,
            to,
            session,
        } => match session {
            Some(session) => cmd_delete_lines_session(&session, &file, from, to),
            None => cmd_delete_lines_file(&file, from, to),
        },
        Command::ReadLines {
            file,
            from,
            to,
            json,
            session,
        } => match session {
            Some(session) => cmd_read_lines_session(&session, &file, from, to, json),
            None => cmd_read_lines_file(&file, from, to, json),
        },

        // Session control
        Command::Send { session, keys } => cmd_send(&session, &keys),
        Command::Paste { session, text } => cmd_paste(&session, &expand_escapes(&text)),
        Command::Resize {
            session,
            dimension: (width, height),
        } => cmd_resize(&session, width, height),
        Command::Exec { session, command } => cmd_exec(&session, &command),
        Command::Snapshot { session, format } => cmd_snapshot(&session, &format),
        Command::Buffer { session } => cmd_buffer(&session),
        Command::Context { session, file } => cmd_context(session.as_deref(), file.as_deref()),
        Command::Search { pattern, session } => cmd_search(&pattern, &session),
        Command::NextMatch { session } => cmd_next_match(&session),

        // LSP commands (nested)
        Command::Lsp { command } => execute_lsp_command(command),

        // Session management (nested)
        Command::Session { command } => execute_session_command(command),

        // Integration
        Command::Install {
            editor,
            show_config,
            workspace,
        } => cmd_install(&editor, show_config, workspace),
        Command::McpServer {
            workspace,
            port,
            session,
        } => cmd_mcp_server(workspace, port, session),
    }
}

/// Run the native shell in-process so `ovim gui` works in source builds,
/// standalone CLI distributions, and desktop packages without path probing.
fn cmd_gui(file: Option<String>, resume: bool) -> Result<()> {
    #[cfg(feature = "gui")]
    {
        crate::gui::app::run(file.as_deref().map(FileArg::parse), resume)
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = (file, resume);
        anyhow::bail!("This Ovim build does not include the native GUI feature")
    }
}

/// Execute LSP subcommands
fn execute_lsp_command(command: LspCommand) -> Result<()> {
    match command {
        LspCommand::Status { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_lsp_status)
            } else {
                with_resolved_session(session_name, cmd_lsp_status)
            }
        }
        LspCommand::Hover { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_hover)
            } else {
                with_resolved_session(session_name, cmd_hover)
            }
        }
        LspCommand::Definition { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_goto_definition)
            } else {
                with_resolved_session(session_name, cmd_goto_definition)
            }
        }
        LspCommand::References { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_find_references)
            } else {
                with_resolved_session(session_name, cmd_find_references)
            }
        }
        LspCommand::Diagnostics { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_diagnostics)
            } else {
                with_resolved_session(session_name, cmd_diagnostics)
            }
        }
        LspCommand::Symbols { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_symbols)
            } else {
                with_resolved_session(session_name, cmd_symbols)
            }
        }
        LspCommand::Outline { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_outline)
            } else {
                with_resolved_session(session_name, cmd_outline)
            }
        }
        LspCommand::Symbol {
            query,
            session,
            file,
        } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, |s| cmd_symbol(&query, s))
            } else {
                with_resolved_session(session_name, |s| cmd_symbol(&query, s))
            }
        }
        LspCommand::Trace { session, file } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, true, 30_000, cmd_trace)
            } else {
                with_resolved_session(session_name, cmd_trace)
            }
        }
        LspCommand::Wait {
            session,
            timeout,
            file,
        } => {
            let session_name = session.as_deref();
            if let Some(file) = file.as_deref() {
                with_temp_headless_session(file, false, timeout, |s| cmd_wait_lsp(s, timeout))
            } else {
                with_resolved_session(session_name, |s| cmd_wait_lsp(s, timeout))
            }
        }
        LspCommand::Check { file, verbose } => cmd_check_lsp(&file, verbose),
        LspCommand::Languages { verbose } => cmd_list_languages(verbose),
    }
}

/// Execute session management subcommands
fn execute_session_command(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::List => cmd_sessions(),
        SessionCommand::Kill { session } => cmd_kill(&session),
        SessionCommand::Health { session } => cmd_health(&session),
        SessionCommand::Cleanup { max_age, dry_run } => cmd_cleanup(max_age, dry_run),
    }
}

// ─── File Operations (direct file I/O) ──────────────────────────────────────

fn session_client_for_file(session_name: &str, file_path: &str) -> Result<OvimClient> {
    let session = resolve_session(session_name)?;
    let client = OvimClient::new(&session);
    let active = client
        .get_buffer()
        .context("Failed to inspect the session buffer")?
        .file_path
        .context("The session buffer has no file path")?;

    let requested = std::path::Path::new(file_path);
    let active_path = std::path::Path::new(&active);
    let matches = requested == active_path
        || match (requested.canonicalize(), active_path.canonicalize()) {
            (Ok(requested), Ok(active)) => requested == active,
            _ => false,
        };
    if !matches {
        anyhow::bail!(
            "Session '{}' is editing '{}', not '{}'",
            session_name,
            active,
            file_path
        );
    }
    Ok(client)
}

fn cmd_edit_session(
    session_name: &str,
    file_path: &str,
    line: Option<usize>,
    old: &str,
    new: &str,
) -> Result<()> {
    let client = session_client_for_file(session_name, file_path)?;
    client.edit(line, &expand_escapes(old), &expand_escapes(new))?;
    println!("Edited live session '{}' (not yet written)", session_name);
    Ok(())
}

fn cmd_insert_session(
    session_name: &str,
    file_path: &str,
    after: Option<usize>,
    before: Option<usize>,
    text: &str,
) -> Result<()> {
    let client = session_client_for_file(session_name, file_path)?;
    client.insert(after, before, &expand_escapes(text))?;
    println!("Edited live session '{}' (not yet written)", session_name);
    Ok(())
}

fn cmd_delete_lines_session(
    session_name: &str,
    file_path: &str,
    from: usize,
    to: usize,
) -> Result<()> {
    let client = session_client_for_file(session_name, file_path)?;
    client.delete_lines(from, to)?;
    println!("Edited live session '{}' (not yet written)", session_name);
    Ok(())
}

fn cmd_read_lines_session(
    session_name: &str,
    file_path: &str,
    from: usize,
    to: usize,
    json_output: bool,
) -> Result<()> {
    let client = session_client_for_file(session_name, file_path)?;
    let response = client.read_lines(from, to)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "lines": response.lines }))?
        );
    } else {
        for line in response.lines {
            println!("{:>4} | {}", line.number, line.text);
        }
    }
    Ok(())
}

/// Replace text in a file (direct file I/O, no session needed).
/// Planning is shared with session mode via `edit_engine` (OV-00298).
fn cmd_edit_file(file_path: &str, line: Option<usize>, old: &str, new: &str) -> Result<()> {
    let old_expanded = expand_escapes(old);
    let new_expanded = expand_escapes(new);

    let content = std::fs::read_to_string(file_path)
        .context(format!("Failed to read file: {}", file_path))?;

    let (splice, match_line) =
        crate::edit_engine::plan_edit(&content, line, &old_expanded, &new_expanded)?;
    overwrite_file(
        file_path,
        &crate::edit_engine::apply_splice(&content, &splice),
    )?;

    if line.is_some() {
        println!("Replaced on line {}", match_line);
    } else {
        println!("Replaced 1 occurrence");
    }
    Ok(())
}

/// Insert text into a file (direct file I/O).
/// Planning is shared with session mode via `edit_engine` (OV-00298).
fn cmd_insert_file(
    file_path: &str,
    after: Option<usize>,
    before: Option<usize>,
    text: &str,
) -> Result<()> {
    use crate::edit_engine::InsertAt;

    let text_expanded = expand_escapes(text);

    let content = std::fs::read_to_string(file_path)
        .context(format!("Failed to read file: {}", file_path))?;

    let at = match (after, before) {
        (Some(after_line), _) => InsertAt::After(after_line),
        (None, Some(before_line)) => InsertAt::Before(before_line),
        (None, None) => anyhow::bail!("Either --after or --before must be specified"),
    };

    let (splice, insert_count) = crate::edit_engine::plan_insert(&content, at, &text_expanded)?;
    overwrite_file(
        file_path,
        &crate::edit_engine::apply_splice(&content, &splice),
    )?;

    match at {
        InsertAt::After(n) => println!("Inserted {} line(s) after line {}", insert_count, n),
        InsertAt::Before(n) => println!("Inserted {} line(s) before line {}", insert_count, n),
    }
    Ok(())
}

/// Delete lines from a file (direct file I/O).
/// Planning is shared with session mode via `edit_engine` (OV-00298).
fn cmd_delete_lines_file(file_path: &str, from: usize, to: usize) -> Result<()> {
    let content = std::fs::read_to_string(file_path)
        .context(format!("Failed to read file: {}", file_path))?;

    let (splice, deleted_count) = crate::edit_engine::plan_delete_lines(&content, from, to)?;
    overwrite_file(
        file_path,
        &crate::edit_engine::apply_splice(&content, &splice),
    )?;

    println!("Deleted {} line(s) ({}-{})", deleted_count, from, to);
    Ok(())
}

/// Read lines from a file (direct file I/O)
fn cmd_read_lines_file(file_path: &str, from: usize, to: usize, json_output: bool) -> Result<()> {
    let content = std::fs::read_to_string(file_path)
        .context(format!("Failed to read file: {}", file_path))?;

    let lines: Vec<&str> = content.lines().collect();

    if from == 0 || to == 0 {
        anyhow::bail!("Line numbers are 1-indexed (got from={}, to={})", from, to);
    }
    if from > lines.len() {
        anyhow::bail!(
            "Line {} out of range (file has {} lines)",
            from,
            lines.len()
        );
    }

    let effective_to = to.min(lines.len());

    if json_output {
        let json_lines: Vec<serde_json::Value> = (from..=effective_to)
            .map(|i| {
                serde_json::json!({
                    "number": i,
                    "text": lines[i - 1]
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "lines": json_lines }))?
        );
    } else {
        for i in from..=effective_to {
            println!("{:>4} | {}", i, lines[i - 1]);
        }
    }

    Ok(())
}

/// Overwrite the file in place (truncate + write on the existing inode).
///
/// This matches `Buffer::save_as_async`, which deliberately writes in place
/// to preserve permissions, ownership, ACLs, hard links, and symlink
/// targets. The previous tmp-then-rename scheme used
/// `path.with_extension("tmp")`, which REPLACED the extension — silently
/// destroying any sibling `foo.tmp` the user owned, resetting the target's
/// permissions, and replacing symlinks with regular files (OV-00278).
fn overwrite_file(file_path: &str, content: &str) -> Result<()> {
    use std::io::Write;

    let mut f = std::fs::File::create(file_path)
        .context(format!("Failed to open file for writing: {}", file_path))?;
    f.write_all(content.as_bytes())?;
    f.flush()?;
    f.sync_all()?;

    Ok(())
}

// ─── Session Control ─────────────────────────────────────────────────────────

/// List all running sessions
fn cmd_sessions() -> Result<()> {
    let sessions = SessionInfo::list_all().context("Failed to list sessions")?;

    if sessions.is_empty() {
        println!("No running ovim sessions");
        return Ok(());
    }

    println!("Running ovim sessions:\n");
    println!(
        "{:<15} {:<8} {:<10} {:<10} FILE",
        "SESSION", "PID", "PORT", "LSP"
    );
    println!("{}", "─".repeat(80));

    for session in sessions {
        let lsp_status = if session.lsp_ready {
            "\x1b[32mready\x1b[0m"
        } else {
            "\x1b[33mpending\x1b[0m"
        };

        let file = session
            .file
            .as_ref()
            .map(|f| {
                // Truncate by chars, not bytes: byte slicing panics when the
                // cut lands inside a multibyte char (OV-00281).
                let char_count = f.chars().count();
                if char_count > 40 {
                    let tail: String = f.chars().skip(char_count - 37).collect();
                    format!("...{}", tail)
                } else {
                    f.clone()
                }
            })
            .unwrap_or_else(|| "-".to_string());

        println!(
            "{:<15} {:<8} {:<10} {:<17} {}",
            session.session_name, session.pid, session.port, lsp_status, file
        );
    }

    Ok(())
}

/// Send keys to a session
fn cmd_send(session_name: &str, keys: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    client
        .send_keys(keys)
        .context("Failed to send keys to session")?;

    let (width, height) = session.dimensions().unwrap_or((120, 35));
    match client.get_render_plain(width, height) {
        Ok(render) => print!("{}", render),
        Err(_) => println!("Keys sent to session '{}'", session.session_name),
    }
    Ok(())
}

fn cmd_paste(session_name: &str, text: &str) -> Result<()> {
    let session = resolve_session(session_name)?;
    OvimClient::new(&session).paste(text)?;
    Ok(())
}

fn cmd_resize(session_name: &str, width: u16, height: u16) -> Result<()> {
    let session = resolve_session(session_name)?;
    OvimClient::new(&session).resize(width, height)?;
    Ok(())
}

/// Execute ex command in a session
fn cmd_exec(session_name: &str, command: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    let result = client
        .execute_command(command)
        .context("Failed to execute command")?;

    println!("{}", result);
    Ok(())
}

/// Get snapshot from a session
fn cmd_snapshot(session_name: &str, format: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    let snapshot = client.get_snapshot().context("Failed to get snapshot")?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        "pretty" => {
            println!("Session: {}", session.session_name);
            println!("Mode: {}", snapshot.mode);
            println!(
                "Cursor: line {}, col {}",
                snapshot.cursor.line, snapshot.cursor.column
            );
            println!("Buffer: {} lines", snapshot.buffer.line_count);
            if let Some(path) = &snapshot.buffer.file_path {
                println!("File: {}", path);
            }
            if let Some(visual) = &snapshot.visual_selection {
                println!(
                    "Visual: ({}, {}) -> ({}, {})",
                    visual.start.line, visual.start.column, visual.end.line, visual.end.column
                );
            }
            println!("Registers: {}", snapshot.registers.len());
            println!("Marks: {}", snapshot.marks.len());
        }
        _ => anyhow::bail!("Unknown format: {}", format),
    }

    Ok(())
}

/// Get buffer content from a session
fn cmd_buffer(session_name: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    let buffer = client.get_buffer().context("Failed to get buffer")?;

    print!("{}", buffer.content);
    Ok(())
}

fn cmd_context(session_name: Option<&str>, file: Option<&str>) -> Result<()> {
    if let Some(target) = file {
        return cmd_context_for_file(target);
    }

    let session_name = session_name
        .map(|s| s.to_string())
        .or_else(|| std::env::var("OVIM_SESSION").ok())
        .unwrap_or_else(|| "default".to_string());

    cmd_context_for_session(&session_name)
}

fn cmd_context_for_file(target: &str) -> Result<()> {
    let file_arg = FileArg::parse(target);
    let contents =
        std::fs::read_to_string(&file_arg.path).context("Failed to read file for context")?;

    let total_lines = contents.lines().count().max(1);
    let line_0 = file_arg
        .line
        .unwrap_or(1)
        .saturating_sub(1)
        .min(total_lines - 1);
    let col_0 = file_arg.col.unwrap_or(1).saturating_sub(1);

    let ctx =
        crate::api::format_context_window(&contents, line_0, col_0, Some(&file_arg.path), "NORMAL");
    print!("{}", ctx);
    Ok(())
}

/// Get context window from a session
fn cmd_context_for_session(session_name: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);

    let response = client
        .send_mcp_request(
            "tools/call",
            serde_json::json!({
                "name": "get_context_window",
                "arguments": {}
            }),
            1,
        )
        .context("Failed to get context window")?;

    if let Some(text_field) = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
    {
        if let Some(text_str) = text_field.as_str() {
            if let Ok(context_info) = serde_json::from_str::<serde_json::Value>(text_str) {
                if let Some(context_text) = context_info.get("context").and_then(|c| c.as_str()) {
                    println!("{}", context_text);
                    return Ok(());
                }
            }
        }
    }

    anyhow::bail!("Failed to extract context from MCP response")
}

/// Search for pattern and jump to first match
fn cmd_search(pattern: &str, session_name: &str) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    let session = resolve_session(session_name)?;
    let client = OvimClient::new(&session);

    let before = client
        .get_snapshot()
        .context("Failed to get snapshot before search")?;

    let search_cmd = format!("/{}<CR>", pattern);
    client
        .send_keys(&search_cmd)
        .context("Failed to send search keys")?;
    thread::sleep(Duration::from_millis(200));
    let after = client
        .get_snapshot()
        .context("Failed to get snapshot after search")?;

    let found =
        (before.cursor.line, before.cursor.column) != (after.cursor.line, after.cursor.column);

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": found,
            "file": after.buffer.file_path,
            "line": after.cursor.line + 1,
            "column": after.cursor.column + 1
        }))?
    );

    Ok(())
}

/// Jump to next match and return position
fn cmd_next_match(session_name: &str) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    let session = resolve_session(session_name)?;
    let client = OvimClient::new(&session);

    let before = client
        .get_snapshot()
        .context("Failed to get snapshot before next match")?;
    client
        .send_keys("n")
        .context("Failed to send next match keys")?;
    thread::sleep(Duration::from_millis(100));
    let after = client
        .get_snapshot()
        .context("Failed to get snapshot after next match")?;

    let moved =
        (before.cursor.line, before.cursor.column) != (after.cursor.line, after.cursor.column);

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": moved,
            "file": after.buffer.file_path,
            "line": after.cursor.line + 1,
            "column": after.cursor.column + 1
        }))?
    );

    Ok(())
}

// ─── LSP Commands ────────────────────────────────────────────────────────────

/// Get LSP status from a session
fn cmd_lsp_status(session: &SessionInfo) -> Result<()> {
    let client = OvimClient::new(session);
    let lsp_status = client
        .get_lsp_status()
        .context("Failed to get LSP status")?;

    if lsp_status.servers.is_empty() {
        println!("No LSP servers running");
        return Ok(());
    }

    println!("LSP Servers for session '{}':\n", session.session_name);

    for server in &lsp_status.servers {
        let state_colored = if server.has_capabilities {
            format!("\x1b[32m{}\x1b[0m", server.state)
        } else if server.state.contains("Initializing") {
            format!("\x1b[33m{}\x1b[0m", server.state)
        } else {
            server.state.clone()
        };

        println!("Language: {}", server.language);
        println!("  Command: {}", server.command);
        println!("  State: {}", state_colored);
        println!("  Pending requests: {}", server.pending_requests);
        println!("  Has capabilities: {}", server.has_capabilities);
        println!();
    }

    if let Some(progress) = &lsp_status.progress {
        println!("Progress: {}", progress);
    }

    Ok(())
}

/// Trigger goto-definition and return new location as JSON
fn cmd_goto_definition(session: &SessionInfo) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    let client = OvimClient::new(session);

    let before = client
        .get_snapshot()
        .context("Failed to get snapshot before goto-definition")?;
    client
        .send_keys("gd")
        .context("Failed to send goto-definition keys")?;
    thread::sleep(Duration::from_millis(300));
    let after = client
        .get_snapshot()
        .context("Failed to get snapshot after goto-definition")?;

    let moved = (before.cursor.line, before.cursor.column)
        != (after.cursor.line, after.cursor.column)
        || before.buffer.file_path != after.buffer.file_path;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": moved,
            "file": after.buffer.file_path,
            "line": after.cursor.line + 1,
            "column": after.cursor.column + 1
        }))?
    );

    Ok(())
}

/// Trigger find-references and return list from picker.
///
/// Sends `grr` (the actual `request_find_references` binding — `gr` alone
/// only sets the LSP `gr*` prefix) and polls the snapshot until the LSP
/// picker opens. Earlier this sent `gr` and slept a single 500 ms, which
/// produced an empty list every call — see OV-00007.
fn cmd_find_references(session: &SessionInfo) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    let client = OvimClient::new(session);

    client
        .send_keys("grr")
        .context("Failed to send find-references keys")?;

    // Poll for the LSP-locations picker to appear. Mirrors the cmd_hover
    // poll loop — total ceiling 6 s, matching other LSP requests that wait
    // on a tokio task to fill a slot. We close the picker on the way out so
    // a follow-up CLI call lands in NORMAL mode.
    let mut snapshot = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(200));
        let snap = client
            .get_snapshot()
            .context("Failed to get snapshot after find-references")?;
        if snap.picker.is_some() {
            snapshot = Some(snap);
            break;
        }
    }
    let snapshot = snapshot
        .map(Ok)
        .unwrap_or_else(|| client.get_snapshot())
        .context("Failed to get snapshot after find-references")?;

    let references = if let Some(picker) = &snapshot.picker {
        picker
            .results
            .iter()
            .map(|r| {
                json!({
                    "display": r.display,
                    "file": r.location,
                    "line": r.line,
                    "column": r.col
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Dismiss the picker so the session is left in NORMAL mode.
    let _ = client.send_keys("<Esc>");

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": !references.is_empty(),
            "references": references
        }))?
    );

    Ok(())
}

/// Trigger hover and return hover_info
fn cmd_hover(session: &SessionInfo) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    let client = OvimClient::new(session);

    client
        .set_mode("NORMAL")
        .context("Failed to set NORMAL mode")?;
    thread::sleep(Duration::from_millis(50));

    client.send_keys("K").context("Failed to send hover keys")?;

    let mut hover_info = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(200));
        let snapshot = client.get_snapshot().context("Failed to get snapshot")?;
        if snapshot.mode.contains("HOVER") {
            hover_info = snapshot.hover_info;
            break;
        }
    }

    let _ = client.set_mode("NORMAL");

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": hover_info.is_some(),
            "hover": hover_info
        }))?
    );

    Ok(())
}

/// Return LSP diagnostic info
fn cmd_diagnostics(session: &SessionInfo) -> Result<()> {
    use serde_json::json;

    let client = OvimClient::new(session);

    let response = client
        .send_mcp_request(
            "tools/call",
            json!({
                "name": "get_diagnostics",
                "arguments": {}
            }),
            1,
        )
        .context("Failed to get diagnostics via MCP")?;

    if let Some(text_field) = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
    {
        if let Some(text_str) = text_field.as_str() {
            if let Ok(diagnostics_info) = serde_json::from_str::<Value>(text_str) {
                println!("{}", serde_json::to_string_pretty(&diagnostics_info)?);
                return Ok(());
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": false,
            "diagnostics": []
        }))?
    );

    Ok(())
}

/// List document symbols
fn cmd_symbols(session: &SessionInfo) -> Result<()> {
    use serde_json::json;

    let client = OvimClient::new(session);

    let response = client
        .send_mcp_request(
            "tools/call",
            json!({
                "name": "get_outline",
                "arguments": {}
            }),
            1,
        )
        .context("Failed to get symbols via MCP")?;

    if let Some(text_field) = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
    {
        if let Some(text_str) = text_field.as_str() {
            if let Ok(symbols_info) = serde_json::from_str::<Value>(text_str) {
                println!("{}", serde_json::to_string_pretty(&symbols_info)?);
                return Ok(());
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "success": false,
            "symbols": []
        }))?
    );

    Ok(())
}

/// Get structural outline of the current document
fn cmd_outline(session: &SessionInfo) -> Result<()> {
    let client = OvimClient::new(session);

    let outline = client.get_outline().context("Failed to get outline")?;

    println!("{}", serde_json::to_string_pretty(&outline)?);
    Ok(())
}

/// Search workspace symbols by name
fn cmd_symbol(query: &str, session: &SessionInfo) -> Result<()> {
    let client = OvimClient::new(session);

    let results = client
        .search_symbols(query)
        .context("Failed to search symbols")?;

    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

/// Get call hierarchy trace for symbol at cursor
fn cmd_trace(session: &SessionInfo) -> Result<()> {
    let client = OvimClient::new(session);

    let trace = client.get_trace().context("Failed to get trace")?;

    println!("{}", serde_json::to_string_pretty(&trace)?);
    Ok(())
}

/// Wait for LSP to be ready (blocks until ready or timeout)
fn cmd_wait_lsp(session: &SessionInfo, timeout_ms: u64) -> Result<()> {
    use serde_json::json;
    use std::thread;
    use std::time::{Duration, Instant};

    let client = OvimClient::new(session);

    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    loop {
        if let Ok(lsp_status) = client.get_lsp_status() {
            let all_ready = !lsp_status.servers.is_empty()
                && lsp_status.servers.iter().all(|s| s.has_capabilities);

            if all_ready {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "success": true,
                        "elapsed_ms": start.elapsed().as_millis()
                    }))?
                );
                return Ok(());
            }
        }

        if start.elapsed() >= timeout {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "success": false,
                    "error": "Timeout waiting for LSP to be ready",
                    "elapsed_ms": start.elapsed().as_millis()
                }))?
            );
            anyhow::bail!("Timeout waiting for LSP");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

/// List all configured languages and their LSP status.
///
/// Uses the catalog with user-registered languages (init.lua / plugins) so
/// the listing agrees with what the editor itself detects, not just the
/// built-in set.
fn cmd_list_languages(verbose: bool) -> Result<()> {
    let catalog = ovim_core::language_catalog::LanguageCatalog::with_user_languages();
    let languages = catalog.all();

    if languages.is_empty() {
        println!("No languages configured.");
        return Ok(());
    }

    if verbose {
        println!("Configured Languages:\n");
        for definition in &languages {
            let lang = &definition.config;
            println!("Language: {} ({})", lang.name, lang.id);
            println!("  Extensions: {}", lang.extensions.join(", "));

            if !lang.filenames.is_empty() {
                println!("  Filenames: {}", lang.filenames.join(", "));
            }

            if let Some(ref syntax) = lang.syntax {
                println!("  Syntax: {}", syntax.grammar);
            } else if definition.syntax.is_some() {
                println!("  Syntax: user-registered parser");
            } else {
                println!("  Syntax: None");
            }

            if !matches!(
                definition.owner,
                ovim_core::language_catalog::RegistrationOwner::BuiltIn
            ) {
                println!("  Registered by: {}", definition.owner);
            }

            if let Some(ref lsp) = lang.lsp {
                println!("  LSP Command: {}", lsp.command);
                if !lsp.args.is_empty() {
                    println!("  LSP Args: {}", lsp.args.join(" "));
                }
                if !lsp.fallback_commands.is_empty() {
                    println!("  Fallbacks: {}", lsp.fallback_commands.join(", "));
                }
                if !lsp.root_markers.is_empty() {
                    println!("  Root Markers: {}", lsp.root_markers.join(", "));
                }
                if let Some(ref hint) = lsp.install_hint {
                    println!("  Install Hint: {}", hint);
                }
                if lsp.auto_install.is_some() {
                    println!("  Auto-Install: Enabled");
                }
            } else {
                println!("  LSP: None");
            }

            println!();
        }
    } else {
        println!("{:<15} {:<20} {:<10}", "ID", "Name", "LSP");
        println!("{}", "-".repeat(50));

        for definition in &languages {
            let lang = &definition.config;
            let lsp_status = if lang.lsp.is_some() {
                "Configured"
            } else {
                "-"
            };

            println!("{:<15} {:<20} {:<10}", lang.id, lang.name, lsp_status);
        }

        println!("\nUse --verbose for detailed configuration");
    }

    Ok(())
}

/// Check language configuration and LSP status for a file.
///
/// Uses the catalog with user-registered languages (init.lua / plugins) so
/// detection agrees with what the editor itself does, not just the
/// built-in set.
fn cmd_check_lsp(file_path: &str, verbose: bool) -> Result<()> {
    use crate::language_config::{find_lsp_command, find_project_root};
    use std::path::Path;

    let path = Path::new(file_path);
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    println!("File: {}", abs_path.display());
    println!();

    let catalog = ovim_core::language_catalog::LanguageCatalog::with_user_languages();
    let definition = match catalog.detect(&abs_path) {
        Some(definition) => definition,
        None => {
            println!("No language configuration found for this file");
            println!("\nSupported extensions: ");
            for definition in catalog.all() {
                let lang = &definition.config;
                if !lang.extensions.is_empty() {
                    println!("  {} -> {}", lang.extensions.join(", "), lang.name);
                }
            }
            return Ok(());
        }
    };
    let lang_config = &definition.config;

    println!(
        "Language Detected: {} ({})",
        lang_config.name, lang_config.id
    );
    println!("  Extensions: {}", lang_config.extensions.join(", "));
    if !matches!(
        definition.owner,
        ovim_core::language_catalog::RegistrationOwner::BuiltIn
    ) {
        println!("  Registered by: {}", definition.owner);
    }

    if let Some(ref syntax) = lang_config.syntax {
        println!("\nSyntax Highlighting: {} grammar", syntax.grammar);
    } else if definition.syntax.is_some() {
        println!("\nSyntax Highlighting: user-registered parser");
    } else {
        println!("\nSyntax Highlighting: Not configured");
    }

    if let Some(ref lsp) = lang_config.lsp {
        println!("\nLSP Configuration:");
        println!("  Primary Command: {}", lsp.command);

        if !lsp.args.is_empty() {
            println!("  Args: {}", lsp.args.join(" "));
        }

        if !lsp.fallback_commands.is_empty() {
            println!("  Fallback Commands: {}", lsp.fallback_commands.join(", "));
        }

        match find_lsp_command(lsp) {
            Some(ref found_command) => {
                println!("\nLSP Server Found: {}", found_command);

                let root_markers = &lsp.root_markers;
                if !root_markers.is_empty() {
                    let project_root = find_project_root(&abs_path, root_markers);
                    println!("Project Root: {}", project_root.display());
                    println!("  (detected using markers: {})", root_markers.join(", "));
                }
            }
            None => {
                println!("\nLSP Server Not Found");
                println!("  Searched for: {}", lsp.command);
                if !lsp.fallback_commands.is_empty() {
                    println!("  Also searched: {}", lsp.fallback_commands.join(", "));
                }

                if let Some(ref hint) = lsp.install_hint {
                    println!("\n  Installation:");
                    println!("  {}", hint);
                }

                if lsp.auto_install.is_some() {
                    println!("\n  Auto-install is configured for this language.");
                    println!(
                        "  LSP will be installed automatically when you open a {} file in ovim.",
                        lang_config.id
                    );
                }
            }
        }

        if verbose {
            println!("\n--- Full LSP Configuration ---");
            println!("{:#?}", lsp);
        }
    } else {
        println!("\nLSP: Not configured for {}", lang_config.name);
        println!("\nYou can add LSP support by creating ~/.config/ovim/languages.toml");
        println!("See: user-docs/LANGUAGE_SUPPORT.md");
    }

    Ok(())
}

// ─── Session Management ──────────────────────────────────────────────────────

/// Kill a session
fn cmd_kill(session_name: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    client
        .kill_session(&session)
        .context("Failed to kill session")?;

    println!(
        "\x1b[32mSession '{}' (PID: {}) killed\x1b[0m",
        session.session_name, session.pid
    );
    Ok(())
}

/// Check health of a session
fn cmd_health(session_name: &str) -> Result<()> {
    let session = resolve_session(session_name)?;

    let client = OvimClient::new(&session);
    let health = client.get_health().context("Failed to get health")?;

    println!("Session: {}", session.session_name);
    println!("Status: {}", health.status);
    println!("Uptime: {} seconds", health.uptime_seconds);
    if let Some(file) = &health.file {
        println!("File: {}", file);
    }
    println!("Ready: {}", health.ready);

    if !health.lsp_servers.is_empty() {
        println!("\nLSP Servers:");
        for (lang, status) in &health.lsp_servers {
            let status_colored = match status.as_str() {
                "ready" => format!("\x1b[32m{}\x1b[0m", status),
                "initializing" => format!("\x1b[33m{}\x1b[0m", status),
                _ => status.clone(),
            };
            println!("  {}: {}", lang, status_colored);
        }
    }

    Ok(())
}

/// Clean up stale, expired, and corrupted session files
fn cmd_cleanup(max_age_days: Option<u64>, dry_run: bool) -> Result<()> {
    use crate::session::cleanup_stale_sessions;
    use std::time::Duration;

    let max_age = max_age_days.map(|days| Duration::from_secs(days * 24 * 60 * 60));

    println!("Cleaning up session files...\n");

    if dry_run {
        println!("[DRY RUN MODE - no files will be removed]\n");
    }

    if let Some(days) = max_age_days {
        println!("Maximum session age: {} days\n", days);
    }

    let result = cleanup_stale_sessions(max_age, dry_run).context("Failed to clean up sessions")?;

    if result.total_removed() == 0 {
        println!("No stale sessions found. Everything is clean!");
        return Ok(());
    }

    println!("Cleanup Summary:");
    println!("─────────────────────────────────────────────");

    if result.stale_removed > 0 {
        println!(
            "  Stale sessions (dead processes):  {}",
            result.stale_removed
        );
    }

    if result.expired_removed > 0 {
        println!(
            "  Expired sessions (too old):       {}",
            result.expired_removed
        );
    }

    if result.corrupted_removed > 0 {
        println!(
            "  Corrupted session files:          {}",
            result.corrupted_removed
        );
    }

    if result.temp_files_removed > 0 {
        println!(
            "  Orphaned temp files:              {}",
            result.temp_files_removed
        );
    }

    println!("─────────────────────────────────────────────");
    println!(
        "  Total removed:                    {}",
        result.total_removed()
    );

    if !result.removed_sessions.is_empty() {
        println!("\nRemoved sessions:");
        for session in &result.removed_sessions {
            println!("  - {}", session);
        }
    }

    if dry_run {
        println!("\n[DRY RUN] Run without --dry-run to actually remove these files.");
    } else {
        println!("\nCleanup complete!");
    }

    Ok(())
}

// ─── Integration ─────────────────────────────────────────────────────────────

/// Install ovim as MCP server for supported editors
fn cmd_install(editor: &str, show_config: bool, workspace: Option<String>) -> Result<()> {
    use std::fs;
    use std::path::PathBuf;

    let ovim_path = std::env::current_exe().context("Failed to get current executable path")?;
    let ovim_bin = ovim_path
        .canonicalize()
        .unwrap_or(ovim_path)
        .to_string_lossy()
        .to_string();

    let workspace_dir = if let Some(w) = workspace {
        PathBuf::from(w)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ovim-workspace")
    };

    if !show_config {
        fs::create_dir_all(&workspace_dir).context("Failed to create workspace directory")?;
    }

    let mcp_config = serde_json::json!({
        "type": "stdio",
        "command": ovim_bin,
        "args": ["mcp-server", "--workspace", workspace_dir.to_string_lossy().to_string()]
    });

    match editor.to_lowercase().as_str() {
        "claude-code" | "code" => install_claude_code(&mcp_config, show_config)?,
        "claude-desktop" | "desktop" => install_claude_desktop(&mcp_config, show_config)?,
        "claude" => {
            install_claude_code(&mcp_config, show_config)?;
            install_claude_desktop(&mcp_config, show_config)?;
        }
        "cursor" => install_cursor(&mcp_config, show_config)?,
        "all" => {
            install_claude_code(&mcp_config, show_config)?;
            install_claude_desktop(&mcp_config, show_config)?;
            install_cursor(&mcp_config, show_config)?;
        }
        _ => anyhow::bail!(
            "Unknown editor: {}. Supported: claude (both), claude-code, claude-desktop, cursor, all",
            editor
        ),
    }

    if !show_config {
        println!("\n\x1b[32mInstallation complete!\x1b[0m");
        println!("\nNext steps:");
        println!("1. Restart the editor to load the new MCP server");
        println!("2. The ovim MCP server will auto-spawn sessions as needed");
        println!("3. Any queries involving your code will automatically use ovim's LSP features");
    }

    Ok(())
}

/// Install for Claude Code
fn install_claude_code(mcp_config: &serde_json::Value, show_config: bool) -> Result<()> {
    use std::fs;
    use std::path::PathBuf;

    let config_path = PathBuf::from(".mcp.json");
    let claude_dir = PathBuf::from(".claude");
    let claude_settings_path = claude_dir.join("settings.json");
    let hook_script_path = claude_dir.join("hooks/inject_context.sh");

    let abs_path = std::fs::canonicalize(".")
        .map(|p| p.join(".mcp.json"))
        .unwrap_or_else(|_| config_path.clone());

    let mut config: serde_json::Value = if config_path.exists() {
        let content =
            fs::read_to_string(&config_path).context("Failed to read existing .mcp.json")?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if config.get("mcpServers").is_none() {
        config["mcpServers"] = serde_json::json!({});
    }

    config["mcpServers"]["ovim"] = mcp_config.clone();

    if show_config {
        println!("\nClaude Code config to be added/merged to .mcp.json:");
        println!(
            "{}",
            serde_json::to_string_pretty(&config["mcpServers"]["ovim"])?
        );
        println!("\nWill be saved to: {}", abs_path.display());
        println!("\nClaude Code hook config (UserPromptSubmit event):");
        let parent = abs_path.parent().unwrap_or(&abs_path);
        println!(
            "Hooks directory: {}",
            parent.join(".claude/hooks").display()
        );
        println!(
            "Settings file: {}",
            parent.join(".claude/settings.json").display()
        );
        println!(
            "Hook script: {}",
            parent.join(".claude/hooks/inject_context.sh").display()
        );
    } else {
        fs::create_dir_all(claude_dir.join("hooks"))
            .context("Failed to create .claude/hooks directory")?;

        let hook_script = "#!/bin/bash\n\
# Auto-inject ovim context into Claude Code messages\n\
# Runs on UserPromptSubmit event - outputs context to be injected\n\
\n\
# Try to find ovim binary (prefer installed, fallback to local build)\n\
if command -v ovim &>/dev/null; then\n\
  context_output=$(ovim context 2>/dev/null)\n\
elif [ -x ./target/release/ovim ]; then\n\
  context_output=$(./target/release/ovim context 2>/dev/null)\n\
elif [ -x ./target/debug/ovim ]; then\n\
  context_output=$(./target/debug/ovim context 2>/dev/null)\n\
fi\n\
\n\
# Exit code 0 and stdout will be injected as context\n\
if [ -n \"$context_output\" ]; then\n\
  echo \"$context_output\"\n\
fi\n\
";
        fs::write(&hook_script_path, hook_script).context("Failed to write hook script")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook_script_path, fs::Permissions::from_mode(0o755))
                .context("Failed to make hook executable")?;
        }

        let mut claude_settings: serde_json::Value = if claude_settings_path.exists() {
            let content = fs::read_to_string(&claude_settings_path)
                .context("Failed to read existing .claude/settings.json")?;
            serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        if claude_settings.get("hooks").is_none() {
            claude_settings["hooks"] = serde_json::json!({});
        }

        let mut hooks = claude_settings["hooks"]["UserPromptSubmit"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let new_hook = serde_json::json!({
            "type": "command",
            "command": ".claude/hooks/inject_context.sh"
        });

        let hook_exists = hooks.iter().any(|h| {
            h.get("command").and_then(|c| c.as_str()) == Some(".claude/hooks/inject_context.sh")
        });

        if !hook_exists {
            hooks.push(new_hook);
            claude_settings["hooks"]["UserPromptSubmit"] = serde_json::Value::Array(hooks);
            println!("Added ovim context hook to .claude/settings.json");
        } else {
            println!("Hook already exists in .claude/settings.json (skipped)");
        }

        fs::write(
            &claude_settings_path,
            serde_json::to_string_pretty(&claude_settings)?,
        )
        .context("Failed to write .claude/settings.json")?;

        fs::write(&config_path, serde_json::to_string_pretty(&config)?)
            .context("Failed to write .mcp.json")?;

        println!("Updated .mcp.json for Claude Code");
        println!("  Location: {}", abs_path.display());
        println!("  Hook script: {}", hook_script_path.display());
        println!("  Context will auto-inject on every message you send!");
        println!("\nImportant: Files are created in the current directory.");
        println!("  Make sure you run this command from your project root.");
    }

    Ok(())
}

/// Install for Claude Desktop
fn install_claude_desktop(mcp_config: &serde_json::Value, show_config: bool) -> Result<()> {
    use std::fs;
    use std::path::PathBuf;

    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let config_path = PathBuf::from(&home).join(".config/Claude/claude_desktop_config.json");

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).context("Failed to create .config/Claude directory")?;
    }

    let mut config: serde_json::Value = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .context("Failed to read existing Claude Desktop config")?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if config.get("mcpServers").is_none() {
        config["mcpServers"] = serde_json::json!({});
    }

    config["mcpServers"]["ovim"] = mcp_config.clone();

    if show_config {
        println!("\nClaude Desktop config to be added/merged:");
        println!(
            "{}",
            serde_json::to_string_pretty(&config["mcpServers"]["ovim"])?
        );
        println!("\nWill be saved to: {}", config_path.to_string_lossy());
    } else {
        fs::write(&config_path, serde_json::to_string_pretty(&config)?)
            .context("Failed to write Claude Desktop config")?;
        println!("Updated Claude Desktop config");
    }

    Ok(())
}

/// Install for Cursor IDE
fn install_cursor(mcp_config: &serde_json::Value, show_config: bool) -> Result<()> {
    use std::fs;
    use std::path::PathBuf;

    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let config_path = PathBuf::from(&home).join(".cursor/rules/mcp_config.json");

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).context("Failed to create .cursor/rules directory")?;
    }

    let mut config: serde_json::Value = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .context("Failed to read existing Cursor MCP config")?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if config.get("mcpServers").is_none() {
        config["mcpServers"] = serde_json::json!({});
    }

    config["mcpServers"]["ovim"] = mcp_config.clone();

    if show_config {
        println!("\nCursor IDE config to be added/merged:");
        println!(
            "{}",
            serde_json::to_string_pretty(&config["mcpServers"]["ovim"])?
        );
        println!("\nWill be saved to: {}", config_path.to_string_lossy());
    } else {
        fs::write(&config_path, serde_json::to_string_pretty(&config)?)
            .context("Failed to write Cursor MCP config")?;
        println!("Updated Cursor IDE config");
    }

    Ok(())
}

/// Start ovim as a long-running MCP server
fn cmd_mcp_server(
    workspace: Option<String>,
    _port: Option<u16>,
    _session: Option<String>,
) -> Result<()> {
    use std::path::PathBuf;

    let workspace_dir = if let Some(w) = workspace {
        PathBuf::from(w)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ovim-workspace")
    };

    crate::mcp_stdio_server::run_mcp_server(workspace_dir)
}
