use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::edit::Edit;
use crate::editor::{CursorPos, Editor};
use crate::unicode::{CharCol, GraphemeCol};

use super::super::shell_expansion::expand_shell_command;
use super::range::parse_range_with_status;
/// Handles shell command execution (:! or :.! or :%!)
/// - `:!cmd` - runs command and displays output
/// - `:.!cmd` - replaces current line with command output
/// - `:%!cmd` - pipes entire buffer through command
/// - `:range!cmd` - pipes specified range through command
pub(super) fn handle_shell_command(
    editor: &mut Editor,
    range_str: &str,
    shell_cmd: &str,
) -> Result<()> {
    // Expand % and # in the shell command
    let current_file = editor.buffer().file_path().unwrap_or("").to_string();
    let alternate_file = editor.registers().get(Some('#'));
    let shell_cmd = expand_shell_command(shell_cmd, &current_file, &alternate_file);

    // Determine the shell to use
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_arg = if cfg!(windows) { "/C" } else { "-c" };

    // Check if we're piping buffer content through the command
    let is_filter = !range_str.is_empty();

    if is_filter {
        // Parse the range
        let (start_line, end_line) =
            match parse_range_with_status(editor, range_str, Some("Invalid range")) {
                Some(range) => range,
                None => return Ok(()),
            };

        // Get the text from the range. Re-add the line terminators
        // `line_text` strips so the spawned filter sees the same input it
        // would have seen via `cat`.
        let mut input_text = String::new();
        for line_idx in start_line..=end_line {
            if let Some(line) = editor.buffer().line_text(line_idx) {
                input_text.push_str(&line);
                input_text.push('\n');
            }
        }

        // Run command with input piped
        let output = Command::new(shell)
            .arg(shell_arg)
            .arg(shell_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(input_text.as_bytes())?;
                }
                child.wait_with_output()
            });

        match output {
            Ok(output) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let output_text = stdout;

                    // Insert the command output (with trailing newline if needed)
                    let insert_text = if output_text.is_empty() {
                        String::new()
                    } else if output_text.ends_with('\n') {
                        output_text.to_string()
                    } else {
                        format!("{}\n", output_text)
                    };

                    let cursor_before = editor.cursor_position();
                    let ((), edits) = editor.buffer_mut().record(|buf| {
                        buf.delete_range(start_line, CharCol::ZERO, end_line + 1, CharCol::ZERO);
                        if !insert_text.is_empty() {
                            buf.insert_text_at(start_line, CharCol::ZERO, &insert_text);
                        }
                        buf.cursor_mut().set_position(start_line, GraphemeCol::ZERO);
                    });
                    if !edits.is_empty() {
                        let cursor_after = CursorPos::new(start_line, GraphemeCol::ZERO);
                        editor.push_recorded_undo(edits, cursor_before, cursor_after);
                    }

                    let line_count = insert_text.lines().count();
                    editor.set_status_message(format!("{} lines filtered", line_count));
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    editor.set_status_message(format!("Command failed: {}", stderr.trim()));
                }
            }
            Err(e) => {
                editor.set_status_message(format!("Failed to run command: {}", e));
            }
        }
    } else {
        // Queue for the event loop to execute with full terminal access.
        // The TUI will leave alternate screen, run the command with inherited I/O,
        // show a "Press ENTER" prompt, then restore the editor.
        editor.build.last_shell_command = Some(shell_cmd.to_string());
        editor.build.pending_shell_command = Some(crate::editor::PendingShellCommand {
            command: shell_cmd.to_string(),
        });
    }

    Ok(())
}

/// Handles :r !cmd - read output from shell command and insert below cursor
/// - `:r !cmd` - insert output below current line
/// - `:0r !cmd` - insert at start of buffer
/// - `:'<,'>r !cmd` - insert after selection
pub(super) fn handle_read_shell_command(
    editor: &mut Editor,
    range_str: &str,
    shell_cmd: &str,
) -> Result<()> {
    // Expand % and # in the shell command
    let current_file = editor.buffer().file_path().unwrap_or("").to_string();
    let alternate_file = editor.registers().get(Some('#'));
    let shell_cmd = expand_shell_command(shell_cmd, &current_file, &alternate_file);

    // Determine the shell to use
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_arg = if cfg!(windows) { "/C" } else { "-c" };

    // Run the command
    let output = Command::new(shell)
        .arg(shell_arg)
        .arg(&shell_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let output_text = stdout.to_string();

                if output_text.is_empty() {
                    editor.set_status_message("Command produced no output".to_string());
                    return Ok(());
                }

                // Determine insertion point
                let insert_line = if range_str.is_empty() {
                    // Insert after current line
                    editor.buffer().cursor().line() + 1
                } else if let Some((_, end_line)) =
                    parse_range_with_status(editor, range_str, Some("Invalid range"))
                {
                    // Insert after the range end line
                    end_line + 1
                } else {
                    return Ok(());
                };

                // Record change for undo
                let cursor_before = CursorPos::new(
                    editor.buffer().cursor().line(),
                    editor.buffer().cursor().col(),
                );

                // Calculate insertion point
                let insert_char = if insert_line < editor.buffer().line_count() {
                    editor.buffer().rope().line_to_char(insert_line)
                } else {
                    editor.buffer().rope().len_chars()
                };

                // Ensure text ends with newline
                let text = if output_text.ends_with('\n') {
                    output_text
                } else {
                    format!("{}\n", output_text)
                };

                // Separate appended output from an unterminated final line, but
                // do not create a blank line when the buffer already ends in a
                // newline.
                let needs_line_break = insert_line >= editor.buffer().line_count()
                    && insert_char > 0
                    && editor.buffer().rope().char(insert_char - 1) != '\n';
                let text = if needs_line_break {
                    format!("\n{}", text)
                } else {
                    text
                };

                // Insert the text
                editor.buffer_mut().rope_mut().insert(insert_char, &text);

                // Position cursor at start of inserted text
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

                let line_count = text.lines().count();
                editor.set_status_message(format!(
                    "{} line{} inserted",
                    line_count,
                    if line_count == 1 { "" } else { "s" }
                ));
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                editor.set_status_message(format!("Command failed: {}", stderr.trim()));
            }
        }
        Err(e) => {
            editor.set_status_message(format!("Failed to run command: {}", e));
        }
    }

    Ok(())
}

/// Handles :w !cmd - write buffer/range to command stdin
/// - `:w !cmd` - send entire buffer to command stdin
/// - `:'<,'>w !cmd` - send selection to command stdin
pub(super) fn handle_write_to_command(
    editor: &mut Editor,
    range_str: &str,
    shell_cmd: &str,
) -> Result<()> {
    use std::io::Write;

    // Expand % and # in the shell command
    let current_file = editor.buffer().file_path().unwrap_or("").to_string();
    let alternate_file = editor.registers().get(Some('#'));
    let shell_cmd = expand_shell_command(shell_cmd, &current_file, &alternate_file);

    // Determine the shell to use
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_arg = if cfg!(windows) { "/C" } else { "-c" };

    // Get the content to write
    let content = if range_str.is_empty() {
        // Write entire buffer
        editor.buffer().rope().to_string()
    } else if let Some((start_line, end_line)) =
        parse_range_with_status(editor, range_str, Some("Invalid range"))
    {
        // Write specified range
        let mut text = String::new();
        for line_idx in start_line..=end_line {
            if let Some(line) = editor.buffer().line_text(line_idx) {
                text.push_str(&line);
            }
        }
        text
    } else {
        return Ok(());
    };

    // Run the command with content piped to stdin
    let mut child = match Command::new(shell)
        .arg(shell_arg)
        .arg(&shell_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            editor.set_status_message(format!("Failed to run command: {}", e));
            return Ok(());
        }
    };

    // Write content to stdin
    if let Some(ref mut stdin) = child.stdin {
        if let Err(e) = stdin.write_all(content.as_bytes()) {
            editor.set_status_message(format!("Failed to write to command: {}", e));
            return Ok(());
        }
    }

    // Wait for command to complete
    match child.wait_with_output() {
        Ok(output) => {
            if output.status.success() {
                let line_count = content.lines().count();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let msg = if stdout.trim().is_empty() {
                    format!(
                        "{} line{} written",
                        line_count,
                        if line_count == 1 { "" } else { "s" }
                    )
                } else {
                    // Show command output if any
                    let trimmed = stdout.trim();
                    if trimmed.len() > 100 {
                        format!(
                            "{} lines written: {}...",
                            line_count,
                            crate::unicode::truncate_bytes(trimmed, 100)
                        )
                    } else {
                        format!("{} lines written: {}", line_count, trimmed)
                    }
                };
                editor.set_status_message(msg);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                editor.set_status_message(format!("Command failed: {}", stderr.trim()));
            }
        }
        Err(e) => {
            editor.set_status_message(format!("Failed to wait for command: {}", e));
        }
    }

    Ok(())
}
