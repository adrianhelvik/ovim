use crate::ai::tools::builtins::{self, ToolExecutionContext};
use crate::ai::tools::ToolResult;
use crate::ai::truncate_utf8_with_notice;
use crate::unicode::GraphemeCol;
use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use super::ai_chat_tools::{ToolDispatchOutcome, ToolPathResolution};
use super::ai_tool_path::{compact_tool_label, normalize_path, to_relative_path_for_boundary};
use super::Editor;

fn read_capped_output(
    mut reader: impl Read,
    limit: usize,
    progress: Option<(
        tokio::sync::mpsc::UnboundedSender<super::ai_chat_state::ShellProgressEvent>,
        super::ai_chat_state::ShellOutputStream,
    )>,
) -> (Vec<u8>, bool) {
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        if let Some((sender, stream)) = progress.as_ref() {
            let _ = sender.send(super::ai_chat_state::ShellProgressEvent::Output {
                stream: *stream,
                bytes: buffer[..count].to_vec(),
            });
        }
        let available = limit.saturating_sub(retained.len());
        let keep = available.min(count);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    (retained, truncated)
}

/// Join an output-reader thread, bounding the wait once the execution has
/// been cancelled. Runs before the leader is reaped, so the kill handle
/// stays armed (pid/pgid reserved by the un-reaped leader) for the whole
/// drain.
///
/// The readers finish when every process holding a write end of the pipe
/// exits. Cancellation SIGKILLs the whole process group, which closes the
/// group's pipe ends and unblocks the readers promptly. If a descendant
/// escaped the group (e.g. via `setsid`) and still holds a pipe end, the
/// reader could block indefinitely; after cancellation we abandon it (the
/// detached thread exits once the pipe finally closes) instead of blocking
/// the execution forever — a cancelled result is discarded anyway.
fn drain_reader(
    task: std::thread::JoinHandle<(Vec<u8>, bool)>,
    kill: Option<&super::ai_chat_state::ShellKillHandle>,
) -> (Vec<u8>, bool) {
    let Some(handle) = kill else {
        return task.join().unwrap_or_default();
    };
    let mut abandon_at: Option<std::time::Instant> = None;
    loop {
        if task.is_finished() {
            return task.join().unwrap_or_default();
        }
        if handle.is_cancelled() {
            let deadline = *abandon_at.get_or_insert_with(|| {
                std::time::Instant::now() + std::time::Duration::from_secs(2)
            });
            if std::time::Instant::now() >= deadline {
                return (Vec::new(), true);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Reap the shell leader after the output drain has finished. This is
/// reached on every path out of the drain (normal EOF, cancelled group
/// kill, abandoned drain, panicked reader), so the zombie never leaks.
///
/// Reaping releases the leader's pid (and, once the group is empty, the
/// pgid) for reuse, so it must be the very last step. Each non-blocking
/// reap attempt runs under the kill-handle lock and forgets the pid in the
/// same critical section, so a concurrent cancel either `killpg`s the
/// still-reserved group or finds no pid at all. The poll loop (rather than
/// a blocking `wait()` after disarming the handle) keeps a command that
/// closed its output pipes early but kept running cancellable.
fn reap_shell_leader(
    child: &mut std::process::Child,
    kill: Option<&super::ai_chat_state::ShellKillHandle>,
) -> std::io::Result<std::process::ExitStatus> {
    let Some(handle) = kill else {
        return child.wait();
    };
    loop {
        match handle.reap_locked(|| child.try_wait()) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn run_bash_program(
    command: &str,
    workdir: &Path,
    kill: Option<&super::ai_chat_state::ShellKillHandle>,
    progress: Option<tokio::sync::mpsc::UnboundedSender<super::ai_chat_state::ShellProgressEvent>>,
) -> ToolResult {
    if kill.is_some_and(super::ai_chat_state::ShellKillHandle::is_interrupted) {
        return ToolResult::Error("Execution cancelled".into());
    }
    let shell = std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::ffi::OsString::from("/bin/sh"));
    let mut builder = Command::new(&shell);
    builder
        .arg("-lc")
        .arg(command)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        // Make the child a process-group leader so cancellation can kill the
        // whole command tree, not just the shell.
        use std::os::unix::process::CommandExt;
        builder.process_group(0);
    }
    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ToolResult::Error(format!(
                "failed to execute '{}': {}",
                std::path::Path::new(&shell).display(),
                err
            ))
        }
    };
    if let Some(handle) = kill {
        if !handle.publish_child(child.id()) {
            // Cancelled between the pre-spawn check and the spawn: the cancel
            // side never saw a pid, so reap the child here.
            super::ai_chat_state::kill_shell_process_group(child.id());
            let _ = child.wait();
            return ToolResult::Error("Execution cancelled".into());
        }
    }
    if let Some(sender) = progress.as_ref() {
        let _ = sender.send(super::ai_chat_state::ShellProgressEvent::Spawned { pid: child.id() });
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_progress = progress
        .clone()
        .map(|sender| (sender, super::ai_chat_state::ShellOutputStream::Stdout));
    let stderr_progress =
        progress.map(|sender| (sender, super::ai_chat_state::ShellOutputStream::Stderr));
    let stdout_task =
        std::thread::spawn(move || read_capped_output(stdout, 48 * 1024, stdout_progress));
    let stderr_task =
        std::thread::spawn(move || read_capped_output(stderr, 16 * 1024, stderr_progress));
    // Drain the pipes BEFORE reaping the leader. While the leader is
    // un-reaped (running or zombie) the kernel reserves its pid — and
    // therefore its pgid — so a cancel landing anywhere in this window can
    // safely `killpg` the group, even after every living group member has
    // exited (e.g. when a descendant that left the group via setsid still
    // holds the pipe ends). Reaping is the very last step and forgets the
    // pid atomically, so cancel can never signal a reused id.
    let (stdout_bytes, stdout_truncated) = drain_reader(stdout_task, kill);
    let (stderr_bytes, stderr_truncated) = drain_reader(stderr_task, kill);
    let status = match reap_shell_leader(&mut child, kill) {
        Ok(status) => status,
        Err(error) => return ToolResult::Error(format!("failed waiting for shell: {error}")),
    };
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    let mut body = String::new();
    if !stdout.trim_end().is_empty() {
        body.push_str(stdout.trim_end());
    }
    if !stderr.trim_end().is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("stderr:\n");
        body.push_str(stderr.trim_end());
    }
    if stdout_truncated || stderr_truncated {
        body.push_str("\n[output truncated by ovim]");
    }
    let status_line = match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    };
    let cmd_label = compact_tool_label(command);
    let out = if body.is_empty() {
        format!(
            "bash `{}` {} ({status_line}) with no output.",
            cmd_label,
            if status.success() {
                "succeeded"
            } else {
                "failed"
            }
        )
    } else {
        format!(
            "bash `{}` {} ({status_line}).\n{}",
            cmd_label,
            if status.success() {
                "succeeded"
            } else {
                "failed"
            },
            body
        )
    };
    if status.success() {
        ToolResult::Success(truncate_utf8_with_notice(&out, 64 * 1024))
    } else {
        ToolResult::Error(truncate_utf8_with_notice(&out, 64 * 1024))
    }
}

impl Editor {
    /// Execute a single read tool call, checking scope before dispatch.
    pub(crate) fn execute_tool_call(
        &self,
        tool_call: &crate::ai::chat_types::ToolCallInfo,
        ctx: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(tool_def) = self.ai_state.tool_registry.get(&tool_call.name) else {
            return ToolResult::Error(format!("unknown tool: {}", tool_call.name));
        };

        // Check that capabilities satisfy the tool's requirements
        if !ctx.capabilities.contains(&tool_def.required_scope) {
            return ToolResult::Error(format!(
                "tool '{}' requires scope not granted by current context",
                tool_call.name
            ));
        }

        builtins::execute_builtin(&tool_call.name, &tool_call.arguments, ctx)
    }

    pub(super) fn execute_read_file_at_path_tool(
        &mut self,
        tc: &crate::ai::chat_types::ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let Some(raw_path) = tc.arguments.get("path").and_then(|v| v.as_str()) else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' parameter is required and must be non-empty".to_string(),
            ));
        };
        if raw_path.is_empty() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' parameter is required and must be non-empty".to_string(),
            ));
        }

        let resolution = match self.resolve_tool_path_policy(
            raw_path,
            false,
            "read_file_at_path",
            approved_once_root,
        ) {
            Ok(r) => r,
            Err(e) => return ToolDispatchOutcome::Completed(ToolResult::Error(e)),
        };

        let (absolute_path, boundary_root) = match resolution {
            ToolPathResolution::Allowed {
                absolute_path,
                boundary_root,
            } => (absolute_path, boundary_root),
            ToolPathResolution::NeedsApproval(req) => {
                return ToolDispatchOutcome::ApprovalRequired(req)
            }
        };
        if let Some(req) = self.maybe_require_tool_policy_approval(
            tc,
            Some(absolute_path.clone()),
            false,
            approved_once_root,
        ) {
            return ToolDispatchOutcome::ApprovalRequired(req);
        }

        let rel_path = to_relative_path_for_boundary(&absolute_path, &boundary_root);
        let mut patched_call = tc.clone();
        if let Some(obj) = patched_call.arguments.as_object_mut() {
            obj.insert("path".to_string(), json!(rel_path));
        } else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "tool arguments must be an object".to_string(),
            ));
        }

        let mut ctx = self.build_tool_execution_context();
        ctx.scope_context.project_root = Some(boundary_root);
        let result = self.execute_tool_call(&patched_call, &ctx);
        ToolDispatchOutcome::Completed(result)
    }

    pub(super) fn execute_list_files_tool(
        &mut self,
        tc: &crate::ai::chat_types::ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let mut patched_call = tc.clone();
        let (boundary_root, requested_dir_for_policy) =
            if let Some(raw_path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
                if raw_path.is_empty() {
                    match self.ai_effective_project_root() {
                        Some(root) => (root.clone(), root),
                        None => {
                            return ToolDispatchOutcome::Completed(ToolResult::Error(
                                self.no_project_root_error(),
                            ))
                        }
                    }
                } else {
                    let resolution = match self.resolve_tool_path_policy(
                        raw_path,
                        true,
                        "list_files",
                        approved_once_root,
                    ) {
                        Ok(r) => r,
                        Err(e) => return ToolDispatchOutcome::Completed(ToolResult::Error(e)),
                    };
                    let (absolute_path, boundary_root) = match resolution {
                        ToolPathResolution::Allowed {
                            absolute_path,
                            boundary_root,
                        } => (absolute_path, boundary_root),
                        ToolPathResolution::NeedsApproval(req) => {
                            return ToolDispatchOutcome::ApprovalRequired(req)
                        }
                    };
                    let rel_path = to_relative_path_for_boundary(&absolute_path, &boundary_root);
                    if let Some(obj) = patched_call.arguments.as_object_mut() {
                        obj.insert("path".to_string(), json!(rel_path));
                    } else {
                        return ToolDispatchOutcome::Completed(ToolResult::Error(
                            "tool arguments must be an object".to_string(),
                        ));
                    }
                    (boundary_root, absolute_path)
                }
            } else {
                match self.ai_effective_project_root() {
                    Some(root) => (root.clone(), root),
                    None => {
                        return ToolDispatchOutcome::Completed(ToolResult::Error(
                            self.no_project_root_error(),
                        ))
                    }
                }
            };

        if let Some(req) = self.maybe_require_tool_policy_approval(
            tc,
            Some(requested_dir_for_policy),
            true,
            approved_once_root,
        ) {
            return ToolDispatchOutcome::ApprovalRequired(req);
        }

        let mut ctx = self.build_tool_execution_context();
        ctx.scope_context.project_root = Some(boundary_root);
        let result = self.execute_tool_call(&patched_call, &ctx);
        ToolDispatchOutcome::Completed(result)
    }

    pub(super) fn execute_open_file_tool(
        &mut self,
        tc: &crate::ai::chat_types::ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let Some(raw_path) = tc.arguments.get("path").and_then(|v| v.as_str()) else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' is required".to_string(),
            ));
        };
        if raw_path.is_empty() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' is required".to_string(),
            ));
        }
        let create = tc
            .arguments
            .get("create")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let caps = self.build_chat_capabilities();
        let Some(tool_def) = self.ai_state.tool_registry.get("open_file") else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "unknown tool: open_file".into(),
            ));
        };
        if !caps.contains(&tool_def.required_scope) {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "tool 'open_file' requires scope not granted by current context".to_string(),
            ));
        }

        let resolution =
            match self.resolve_tool_path_policy(raw_path, false, "open_file", approved_once_root) {
                Ok(r) => r,
                Err(e) => return ToolDispatchOutcome::Completed(ToolResult::Error(e)),
            };
        let absolute_path = match resolution {
            ToolPathResolution::Allowed { absolute_path, .. } => absolute_path,
            ToolPathResolution::NeedsApproval(req) => {
                return ToolDispatchOutcome::ApprovalRequired(req)
            }
        };
        if let Some(req) = self.maybe_require_tool_policy_approval(
            tc,
            Some(absolute_path.clone()),
            false,
            approved_once_root,
        ) {
            return ToolDispatchOutcome::ApprovalRequired(req);
        }

        ToolDispatchOutcome::Completed(self.handle_open_file_at_absolute_path(
            &absolute_path,
            &tc.arguments,
            create,
        ))
    }

    pub(super) fn execute_path_scoped_mutation_tool(
        &mut self,
        tc: &crate::ai::chat_types::ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let name = tc.name.as_str();
        let requires_path = matches!(
            name,
            "write_file_at_path"
                | "create_file"
                | "apply_patch_at_path"
                | "snapshot_file"
                | "restore_file"
        );

        let raw_path = tc
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let original_active_target = self.active_chat_target_absolute_path();
        let mut mutation_target_for_policy = original_active_target.clone();
        let mut target_to_prepare = None;

        if requires_path && raw_path.is_none() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' is required".to_string(),
            ));
        }
        if name != "snapshot_file" && tc.arguments.get("expected_revision").is_none() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "Edit not applied: 'expected_revision' is required. Re-read the target buffer and retry with its current revision."
                    .to_string(),
            ));
        }

        if let Some(raw_path) = raw_path {
            let caps = self.build_chat_capabilities();
            if caps.file_scope < crate::ai::FileScope::Project {
                return ToolDispatchOutcome::Completed(ToolResult::Error(
                    "path parameter requires project file scope".to_string(),
                ));
            }

            let resolution =
                match self.resolve_tool_path_policy(raw_path, false, name, approved_once_root) {
                    Ok(r) => r,
                    Err(e) => return ToolDispatchOutcome::Completed(ToolResult::Error(e)),
                };
            let absolute_path = match resolution {
                ToolPathResolution::Allowed { absolute_path, .. } => absolute_path,
                ToolPathResolution::NeedsApproval(req) => {
                    return ToolDispatchOutcome::ApprovalRequired(req)
                }
            };
            mutation_target_for_policy = Some(absolute_path.clone());

            if name == "create_file" && absolute_path.exists() {
                return ToolDispatchOutcome::Completed(ToolResult::Error(format!(
                    "'{}' already exists. Use write_file_at_path to overwrite.",
                    absolute_path.display()
                )));
            }

            let allow_create = matches!(
                name,
                "write_file_at_path" | "create_file" | "apply_patch_at_path" | "restore_file"
            );
            target_to_prepare = Some((absolute_path, allow_create));
        }

        if let Some(req) = self.maybe_require_tool_policy_approval_with_original_target(
            tc,
            mutation_target_for_policy,
            false,
            approved_once_root,
            original_active_target.as_deref(),
        ) {
            return ToolDispatchOutcome::ApprovalRequired(req);
        }

        // Preparing a missing target changes editor state, so do it only after
        // all path and tool-policy approvals have succeeded.
        if let Some((absolute_path, allow_create)) = target_to_prepare {
            if let Err(e) =
                self.ensure_mutation_target_buffer_for_path(&absolute_path, allow_create)
            {
                return ToolDispatchOutcome::Completed(ToolResult::Error(e));
            }
        }

        ToolDispatchOutcome::Completed(self.execute_mutation_tool(&tc.name, &tc.arguments))
    }

    fn ensure_mutation_target_buffer_for_path(
        &mut self,
        absolute_path: &Path,
        allow_create: bool,
    ) -> std::result::Result<(), String> {
        let normalized_target = normalize_path(absolute_path);

        if let Some(index) = self.buffers.iter().position(|buffer| {
            buffer
                .file_path()
                .map(|p| normalize_path(Path::new(p)) == normalized_target)
                .unwrap_or(false)
        }) {
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.active_buffer_id = self.buffers[index].id();
            }
            return Ok(());
        }

        if absolute_path.exists() {
            if !absolute_path.is_file() {
                return Err(format!(
                    "'{}' is not a file. Use list_files to inspect the directory.",
                    absolute_path.display()
                ));
            }
            let buffer = crate::buffer::Buffer::load_file(absolute_path)
                .map_err(|e| format!("failed to open '{}': {}", absolute_path.display(), e))?;
            // Push directly (not add_buffer) to avoid changing current_buffer_index
            // or % register — AI chat tracks its active buffer via active_buffer_id.
            let idx = self.push_buffer(buffer);
            self.lsp.state.needs_lsp_init = true;
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.active_buffer_id = self.buffers[idx].id();
            }
            return Ok(());
        }

        if !allow_create {
            return Err(format!(
                "'{}' does not exist. Create it first with create_file or write_file_at_path.",
                absolute_path.display()
            ));
        }

        if absolute_path.parent().is_none() {
            return Err(format!(
                "cannot create '{}': invalid target path",
                absolute_path.display()
            ));
        }

        let mut buffer = crate::buffer::Buffer::new();
        buffer.set_file_path(absolute_path.to_string_lossy().to_string());
        // Push directly (not add_buffer) — see comment above.
        let idx = self.push_buffer(buffer);
        self.lsp.state.needs_lsp_init = true;
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = self.buffers[idx].id();
        }
        Ok(())
    }

    pub(super) fn handle_open_file_at_absolute_path(
        &mut self,
        absolute_path: &Path,
        args: &serde_json::Value,
        create: bool,
    ) -> ToolResult {
        if !absolute_path.exists() {
            if !create {
                return ToolResult::Error(format!(
                    "'{}' is not a file. Use list_files to see available files.",
                    absolute_path.display()
                ));
            }
            let Some(parent) = absolute_path.parent() else {
                return ToolResult::Error(format!(
                    "cannot create '{}': invalid path",
                    absolute_path.display()
                ));
            };
            if !parent.exists() || !parent.is_dir() {
                return ToolResult::Error(format!(
                    "cannot create '{}': parent directory '{}' does not exist",
                    absolute_path.display(),
                    parent.display()
                ));
            }
            let target_path = absolute_path.to_string_lossy().to_string();
            let buffer = crate::buffer::Buffer::new();
            self.add_buffer(buffer);
            self.set_file_path(target_path);
        } else if !absolute_path.is_file() {
            return ToolResult::Error(format!(
                "'{}' is not a file. Use list_files to see available files.",
                absolute_path.display()
            ));
        } else if let Err(e) = self.open_file(absolute_path) {
            return ToolResult::Error(format!(
                "failed to open '{}': {}",
                absolute_path.display(),
                e
            ));
        }

        let line = args
            .get("line")
            .and_then(|v| v.as_u64())
            .map(|n| n.saturating_sub(1) as usize)
            .unwrap_or(0);
        let col = args
            .get("column")
            .and_then(|v| v.as_u64())
            .map(|n| n.saturating_sub(1) as usize)
            .unwrap_or(0);

        let max_line = self.buffer().rope().len_lines().saturating_sub(1);
        let target_line = line.min(max_line);
        self.buffer_mut()
            .cursor_mut()
            .set_position(target_line, GraphemeCol(col));
        self.buffer_mut().validate_cursor_position();
        self.center_cursor_in_viewport();

        let opened_buffer_id = self.buffer().id();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = opened_buffer_id;
        }

        let actual_line = self.buffer().cursor().line() + 1;
        let actual_col = self.buffer().cursor().col().0 + 1;
        let total_lines = self.buffer().rope().len_lines();
        ToolResult::Success(format!(
            "Opened {} at line {}, column {} ({} lines total).",
            absolute_path.display(),
            actual_line,
            actual_col,
            total_lines
        ))
    }

    /// Execute an external tool (`bash`) after the caller has applied tool policy.
    pub(crate) fn execute_external_tool(
        &mut self,
        name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(tool_def) = self.ai_state.tool_registry.get(name).cloned() else {
            return ToolResult::Error(format!("unknown tool: {name}"));
        };

        let caps = self.build_chat_capabilities();
        if !caps.allows_side_effect(tool_def.side_effect) {
            return ToolResult::Error(format!(
                "tool '{}' blocked: shell access not allowed in current context",
                name
            ));
        }
        if !caps.contains(&tool_def.required_scope) {
            return ToolResult::Error(format!(
                "tool '{}' requires scope not granted by current context",
                name
            ));
        }

        match name {
            "bash" => self.handle_bash_tool(args),
            "gdiff_comment" => self.handle_gdiff_comment_tool(args),
            _ => ToolResult::Error(format!("unknown external tool: {name}")),
        }
    }

    fn handle_gdiff_comment_tool(&self, args: &serde_json::Value) -> ToolResult {
        let Some(root) = self.ai_effective_project_root() else {
            return ToolResult::Error(self.no_project_root_error());
        };
        let action = args
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let line = args
            .get("line")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let result = match action {
            "add" => {
                let text = args
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                crate::gdiff::add_comment(&root, path, line, text)
            }
            "remove" => crate::gdiff::remove_comment(&root, path, line),
            _ => return ToolResult::Error("'action' must be either 'add' or 'remove'".to_string()),
        };
        match result {
            Ok(comments) => ToolResult::Success(format!(
                "Gdiff comment {action} succeeded for {path}:{line}. The shared review now has {} comment(s).",
                comments.len()
            )),
            Err(error) => ToolResult::Error(format!("failed to update gdiff review: {error:#}")),
        }
    }

    fn handle_bash_tool(&self, args: &serde_json::Value) -> ToolResult {
        let command = match args.get("command").and_then(|v| v.as_str()).map(str::trim) {
            Some(cmd) if !cmd.is_empty() => cmd,
            _ => {
                return ToolResult::Error("'command' is required and must be non-empty".to_string())
            }
        };
        if let Some(reason) = self.comprehension_gate_for_bash(command) {
            return ToolResult::Error(reason);
        }

        // A shell-capable agent must have an explicit repository boundary. Do
        // not silently fall back to the editor process cwd for effects.
        let Some(workdir) = self.ai_effective_project_root() else {
            return ToolResult::Error(self.no_project_root_error());
        };
        let shell = std::env::var_os("SHELL")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| std::ffi::OsString::from("/bin/sh"));

        let mut child = match Command::new(&shell)
            .arg("-lc")
            .arg(command)
            .current_dir(&workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                return ToolResult::Error(format!(
                    "failed to execute '{}': {}",
                    std::path::Path::new(&shell).display(),
                    err
                ))
            }
        };
        // Drain both pipes concurrently to avoid deadlock, retaining only a
        // bounded prefix. A noisy one-off script cannot grow ovim without
        // bound even though the child is allowed to finish normally.
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_task = std::thread::spawn(move || read_capped_output(stdout, 48 * 1024, None));
        let stderr_task = std::thread::spawn(move || read_capped_output(stderr, 16 * 1024, None));
        let status = match child.wait() {
            Ok(status) => status,
            Err(error) => return ToolResult::Error(format!("failed waiting for shell: {error}")),
        };
        let (stdout_bytes, stdout_truncated) = stdout_task.join().unwrap_or_default();
        let (stderr_bytes, stderr_truncated) = stderr_task.join().unwrap_or_default();

        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let mut body = String::new();
        if !stdout.trim_end().is_empty() {
            body.push_str(stdout.trim_end());
        }
        if !stderr.trim_end().is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str("stderr:\n");
            body.push_str(stderr.trim_end());
        }
        if stdout_truncated || stderr_truncated {
            body.push_str("\n[output truncated by ovim]");
        }

        let status_line = match status.code() {
            Some(code) => format!("exit code {code}"),
            None => "terminated by signal".to_string(),
        };
        let cmd_label = compact_tool_label(command);

        if status.success() {
            let out = if body.is_empty() {
                format!(
                    "bash `{}` succeeded ({status_line}) with no output.",
                    cmd_label
                )
            } else {
                format!("bash `{}` succeeded ({status_line}).\n{}", cmd_label, body)
            };
            ToolResult::Success(truncate_utf8_with_notice(&out, 64 * 1024))
        } else {
            let out = if body.is_empty() {
                format!(
                    "bash `{}` failed ({status_line}) with no output.",
                    cmd_label
                )
            } else {
                format!("bash `{}` failed ({status_line}).\n{}", cmd_label, body)
            };
            ToolResult::Error(truncate_utf8_with_notice(&out, 64 * 1024))
        }
    }

    /// Execute an LSP-backed tool (document_symbols, hover, goto_definition).
    pub(crate) fn execute_lsp_tool(&self, name: &str, args: &serde_json::Value) -> ToolResult {
        // LSP inspection follows the file the user is viewing. Unlike
        // mutations, these tools must not silently inspect a pinned chat target.
        let buf = &self.buffers[self.current_buffer_index];
        let Some(file_path) = buf.file_path() else {
            return ToolResult::Error(self.no_file_open_guidance());
        };
        let language_id = crate::syntax::LanguageRegistry::get_lsp_language_id(file_path)
            .unwrap_or("unknown")
            .to_string();
        let Some(uri) = crate::lsp::uri_from_file_path(file_path) else {
            return ToolResult::Error(format!("Cannot create URI for path: {}", file_path));
        };

        let lsp = match &self.lsp.state.lsp_manager {
            Some(lsp) => Arc::clone(lsp),
            None => {
                // Fall back to cached data for document_symbols
                if name == "document_symbols" {
                    return format_document_symbols_cached(
                        &self.lsp.state.available_document_symbols,
                    );
                }
                return ToolResult::Error(
                    "LSP not available. The language server is not running for this file."
                        .to_string(),
                );
            }
        };

        // Clone cached symbols for fallback
        let cached_symbols = self.lsp.state.available_document_symbols.clone();

        match name {
            "document_symbols" => {
                handle_lsp_document_symbols(lsp, uri, language_id, cached_symbols)
            }
            "hover" => handle_lsp_hover(lsp, uri, language_id, args),
            "goto_definition" => handle_lsp_goto_definition(lsp, uri, language_id, args),
            _ => ToolResult::Error(format!("unknown LSP tool: {name}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions: enclosing symbol, symbol kind labels, LSP tool handlers
// ---------------------------------------------------------------------------

/// Walk a hierarchical `DocumentSymbol` tree to find the deepest symbol
/// whose range contains `cursor_line`.
pub(crate) fn find_enclosing_symbol(
    symbols: &[lsp_types::DocumentSymbol],
    cursor_line: u32,
) -> Option<&lsp_types::DocumentSymbol> {
    let mut best: Option<&lsp_types::DocumentSymbol> = None;

    for sym in symbols {
        let range = &sym.range;
        if cursor_line >= range.start.line && cursor_line <= range.end.line {
            // This symbol contains the cursor. Check if it's more specific than current best.
            let is_tighter = best
                .map(|b| {
                    let b_span = b.range.end.line - b.range.start.line;
                    let s_span = range.end.line - range.start.line;
                    s_span < b_span
                })
                .unwrap_or(true);
            if is_tighter {
                best = Some(sym);
            }
            // Recurse into children for a tighter match
            if let Some(children) = &sym.children {
                if let Some(child) = find_enclosing_symbol(children, cursor_line) {
                    let child_span = child.range.end.line - child.range.start.line;
                    let best_span = best
                        .map(|b| b.range.end.line - b.range.start.line)
                        .unwrap_or(u32::MAX);
                    if child_span < best_span {
                        best = Some(child);
                    }
                }
            }
        }
    }

    best
}

/// Human-readable label for an LSP SymbolKind.
pub(super) fn symbol_kind_label(kind: lsp_types::SymbolKind) -> &'static str {
    match kind {
        lsp_types::SymbolKind::FILE => "File",
        lsp_types::SymbolKind::MODULE => "Module",
        lsp_types::SymbolKind::NAMESPACE => "Namespace",
        lsp_types::SymbolKind::PACKAGE => "Package",
        lsp_types::SymbolKind::CLASS => "Class",
        lsp_types::SymbolKind::METHOD => "Method",
        lsp_types::SymbolKind::PROPERTY => "Property",
        lsp_types::SymbolKind::FIELD => "Field",
        lsp_types::SymbolKind::CONSTRUCTOR => "Constructor",
        lsp_types::SymbolKind::ENUM => "Enum",
        lsp_types::SymbolKind::INTERFACE => "Interface",
        lsp_types::SymbolKind::FUNCTION => "Function",
        lsp_types::SymbolKind::VARIABLE => "Variable",
        lsp_types::SymbolKind::CONSTANT => "Constant",
        lsp_types::SymbolKind::STRUCT => "Struct",
        lsp_types::SymbolKind::ENUM_MEMBER => "EnumMember",
        lsp_types::SymbolKind::TYPE_PARAMETER => "TypeParameter",
        _ => "Symbol",
    }
}

/// Format a hierarchical symbol tree for the `document_symbols` tool output.
fn format_symbol_tree(symbols: &[lsp_types::DocumentSymbol], indent: usize, out: &mut String) {
    for sym in symbols {
        let kind = symbol_kind_label(sym.kind);
        let prefix = "  ".repeat(indent);
        out.push_str(&format!(
            "{}{} {} (lines {}-{})\n",
            prefix,
            kind,
            sym.name,
            sym.range.start.line + 1,
            sym.range.end.line + 1,
        ));
        if let Some(children) = &sym.children {
            format_symbol_tree(children, indent + 1, out);
        }
    }
}

/// Format cached document symbols (used when LSP is unavailable).
fn format_document_symbols_cached(symbols: &[lsp_types::DocumentSymbol]) -> ToolResult {
    if symbols.is_empty() {
        return ToolResult::Success(
            "No document symbols available. The language server may not be running \
             or hasn't finished indexing yet."
                .to_string(),
        );
    }
    let mut out = String::from("Document symbols (cached):\n");
    format_symbol_tree(symbols, 0, &mut out);
    ToolResult::Success(out)
}

/// Extract 1-indexed line/column from tool args, converting to 0-indexed.
fn extract_position(args: &serde_json::Value) -> Result<(u32, u32), String> {
    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "'line' parameter is required (1-indexed)".to_string())?;
    let col = args
        .get("column")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "'column' parameter is required (1-indexed)".to_string())?;
    if line == 0 {
        return Err("'line' must be >= 1".to_string());
    }
    if col == 0 {
        return Err("'column' must be >= 1".to_string());
    }
    Ok(((line - 1) as u32, (col - 1) as u32))
}

fn handle_lsp_document_symbols(
    lsp: Arc<crate::lsp::LspManager>,
    uri: lsp_types::Uri,
    language_id: String,
    cached_symbols: Vec<lsp_types::DocumentSymbol>,
) -> ToolResult {
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { lsp.document_symbols(&uri, &language_id).await })
    });

    match result {
        Ok(symbols) if !symbols.is_empty() => {
            let mut out = String::from("Document symbols:\n");
            format_symbol_tree(&symbols, 0, &mut out);
            ToolResult::Success(out)
        }
        Ok(_) => {
            // Live LSP returned empty — fall back to cached
            format_document_symbols_cached(&cached_symbols)
        }
        Err(_) => {
            // LSP request failed — fall back to cached
            format_document_symbols_cached(&cached_symbols)
        }
    }
}

fn handle_lsp_hover(
    lsp: Arc<crate::lsp::LspManager>,
    uri: lsp_types::Uri,
    language_id: String,
    args: &serde_json::Value,
) -> ToolResult {
    let (line, col) = match extract_position(args) {
        Ok(pos) => pos,
        Err(e) => return ToolResult::Error(e),
    };

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { lsp.hover(&uri, line, col, &language_id).await })
    });

    match result {
        Ok(Some(content)) => ToolResult::Success(content),
        Ok(None) => {
            ToolResult::Success("No hover information available at this position.".to_string())
        }
        Err(e) => ToolResult::Error(format!("LSP hover failed: {e}")),
    }
}

fn handle_lsp_goto_definition(
    lsp: Arc<crate::lsp::LspManager>,
    uri: lsp_types::Uri,
    language_id: String,
    args: &serde_json::Value,
) -> ToolResult {
    let (line, col) = match extract_position(args) {
        Ok(pos) => pos,
        Err(e) => return ToolResult::Error(e),
    };

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { lsp.goto_definition(&uri, line, col, &language_id).await })
    });

    match result {
        Ok(Some(location)) => {
            let path = crate::lsp::uri_to_file_path(&location.uri)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| location.uri.as_str().to_string());
            let def_line = location.range.start.line + 1;
            let def_col = location.range.start.character + 1;
            ToolResult::Success(format!(
                "Definition found: {}:{} (col {})",
                path, def_line, def_col
            ))
        }
        Ok(None) => ToolResult::Success("No definition found at this position.".to_string()),
        Err(e) => ToolResult::Error(format!("LSP goto_definition failed: {e}")),
    }
}
