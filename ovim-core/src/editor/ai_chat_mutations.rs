use crate::ai::formats::apply_patch::parse_apply_patch;
use crate::ai::formats::matching::{find_match, MatchResult};
use crate::ai::formats::Hunk;
use crate::ai::tools::ToolResult;
use crate::unicode::{CharCol, GraphemeCol};
use serde::Deserialize;

use super::ai_integration::remap_abs_char_through_edits;
use super::Editor;

// -----------------------------------------------------------------
// Typed arg structs for AI tool handlers
// -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EditRangeArgs {
    start_line: usize,
    end_line: usize,
    new_text: String,
}

fn validate_expected_revision(
    args: &serde_json::Value,
    current_revision: usize,
    file_label: &str,
) -> Result<(), ToolResult> {
    let Some(value) = args.get("expected_revision") else {
        return Err(ToolResult::Error(format!(
            "Edit not applied: 'expected_revision' is required for {file_label}. Re-read the target buffer and retry with its current revision."
        )));
    };
    let Some(expected_revision) = value.as_u64() else {
        return Err(ToolResult::Error(
            "'expected_revision' must be a non-negative integer".to_string(),
        ));
    };
    if expected_revision > usize::MAX as u64 || expected_revision as usize != current_revision {
        return Err(ToolResult::Error(format!(
            "Edit not applied: {file_label} advanced from revision {expected_revision} to {current_revision}. Re-read the affected range and retry."
        )));
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct InsertLinesArgs {
    after_line: usize,
    text: String,
}

#[derive(Debug, Deserialize)]
struct DeleteLinesArgs {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    path: String,
    diff: String,
}

#[derive(Debug, Deserialize)]
struct RestoreFileArgs {
    path: String,
    snapshot_id: String,
}

#[derive(Debug, Deserialize)]
struct OpenFileArgs {
    path: String,
    #[serde(default)]
    line: usize,
    #[serde(default)]
    column: usize,
}

#[derive(Debug, Deserialize)]
struct SelectTextArgs {
    start_line: usize,
    end_line: usize,
    #[serde(default)]
    start_column: usize,
    #[serde(default)]
    end_column: Option<usize>,
}

fn parse_args<T: serde::de::DeserializeOwned>(args: &serde_json::Value) -> Result<T, ToolResult> {
    serde_json::from_value::<T>(args.clone())
        .map_err(|e| ToolResult::Error(format!("Invalid arguments: {}", e)))
}

/// Extract a context snippet around a line range from a rope, with line numbers.
/// `start_line` and `end_line` are 1-indexed, inclusive.
fn snippet_around(
    rope: &ropey::Rope,
    start_line: usize,
    end_line: usize,
    context: usize,
) -> String {
    let total = rope.len_lines();
    if total == 0 {
        return String::new();
    }
    // Convert to 0-indexed
    let first = start_line.saturating_sub(1).saturating_sub(context);
    let last = end_line.min(total).saturating_add(context).min(total);

    let mut out = String::new();
    for i in first..last {
        // Visible line content, terminator stripped (`display::line_content`
        // mirrors `Buffer::line_text` for `&Rope`-only callers).
        let s = crate::display::line_content(rope, i);
        out.push_str(&format!("{:>4} | {}\n", i + 1, s));
    }
    out
}

fn normalize_path_label(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

#[derive(Debug, PartialEq, Eq)]
struct MutationDiffstat {
    added: usize,
    removed: usize,
    current_range: Option<(usize, usize)>,
    deletion_point: Option<usize>,
}

fn mutation_diffstat(before: &str, after: &str) -> MutationDiffstat {
    // Keep terminators so adding or removing the final newline is represented
    // as a changed line rather than an empty diff.
    let before_lines: Vec<_> = before.split_inclusive('\n').collect();
    let after_lines: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = before_lines
        .iter()
        .zip(&after_lines)
        .take_while(|(before, after)| before == after)
        .count();

    let max_suffix = before_lines
        .len()
        .saturating_sub(prefix)
        .min(after_lines.len().saturating_sub(prefix));
    let suffix = (0..max_suffix)
        .take_while(|offset| {
            before_lines[before_lines.len() - 1 - offset]
                == after_lines[after_lines.len() - 1 - offset]
        })
        .count();
    let removed = before_lines.len().saturating_sub(prefix + suffix);
    let added = after_lines.len().saturating_sub(prefix + suffix);

    MutationDiffstat {
        added,
        removed,
        current_range: (added > 0).then_some((prefix + 1, prefix + added)),
        deletion_point: (added == 0 && removed > 0).then_some(prefix + 1),
    }
}

fn append_mutation_envelope(
    message: &mut String,
    tool_name: &str,
    file_label: &str,
    revision_before: usize,
    revision_after: usize,
    diffstat: &MutationDiffstat,
    save_outcome: Option<&str>,
) {
    let changed_range = match (diffstat.current_range, diffstat.deletion_point) {
        (Some((start, end)), _) => format!("lines {start}-{end}"),
        (None, Some(line)) => format!("deletion point at line {line}"),
        (None, None) => "none".to_string(),
    };
    let diagnostics = if revision_before == revision_after {
        "unchanged"
    } else {
        "refresh requested"
    };

    message.push_str(&format!(
        "\nMutation: {tool_name}\nFile: {file_label}\nChanged range: {changed_range}\nDiffstat: +{} -{}\nBuffer revision: {revision_before} -> {revision_after}\nDiagnostics: {diagnostics}\nSave: {}\nProvenance: agent\nRollback: editor undo",
        diffstat.added,
        diffstat.removed,
        save_outcome.unwrap_or("not requested"),
    ));
}

fn normalize_trailing_ws_and_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        + if text.ends_with('\n') || text.ends_with("\r\n") {
            "\n"
        } else {
            ""
        }
}

fn line_start_at_or_before(text: &str, byte_offset: usize) -> usize {
    if byte_offset == 0 {
        return 0;
    }
    text[..byte_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0)
}

fn line_end_after_n_lines(text: &str, start: usize, lines: usize) -> usize {
    if lines == 0 || start >= text.len() {
        return start.min(text.len());
    }

    let mut end = start;
    let mut remaining = lines;
    while remaining > 0 && end < text.len() {
        match text[end..].find('\n') {
            Some(rel) => end += rel + 1,
            None => {
                end = text.len();
                break;
            }
        }
        remaining -= 1;
    }
    end
}

fn find_normalized_match_range(
    haystack: &str,
    byte_offset: usize,
    needle: &str,
) -> Option<(usize, usize)> {
    let needle_lines = needle.lines().count().max(1);
    let start = line_start_at_or_before(haystack, byte_offset.min(haystack.len()));
    let end = line_end_after_n_lines(haystack, start, needle_lines);
    if start > end || end > haystack.len() {
        return None;
    }

    let candidate = &haystack[start..end];
    if normalize_trailing_ws_and_lf(candidate) == normalize_trailing_ws_and_lf(needle) {
        return Some((start, end));
    }
    None
}

fn apply_patch_hunks_to_text(mut content: String, hunks: &[Hunk]) -> Result<String, ToolResult> {
    for (idx, hunk) in hunks.iter().enumerate() {
        if hunk.search.is_empty() {
            content.push_str(&hunk.replace);
            continue;
        }

        let range = match find_match(&content, &hunk.search) {
            MatchResult::Exact { byte_offset } => {
                let end = byte_offset.saturating_add(hunk.search.len());
                if end > content.len() {
                    return Err(ToolResult::Error(format!(
                        "patch hunk {} matched an invalid byte range",
                        idx + 1
                    )));
                }
                (byte_offset, end)
            }
            MatchResult::WhitespaceNormalized { byte_offset } => {
                find_normalized_match_range(&content, byte_offset, &hunk.search).ok_or_else(
                    || {
                        ToolResult::Error(format!(
                            "patch hunk {} matched only after whitespace normalization but could not determine a stable replacement range",
                            idx + 1
                        ))
                    },
                )?
            }
            MatchResult::NotFound(err) => {
                let mut msg = format!("patch hunk {} not found: {}", idx + 1, err.message);
                if let Some(line) = err.closest_line {
                    msg.push_str(&format!(" (closest line: {})", line + 1));
                }
                if let Some(snippet) = err.closest_snippet {
                    msg.push_str(&format!(" near '{}'", snippet));
                }
                return Err(ToolResult::Error(msg));
            }
        };

        content.replace_range(range.0..range.1, &hunk.replace);
    }

    Ok(content)
}

impl Editor {
    // -----------------------------------------------------------------
    // Navigation tool dispatch
    // -----------------------------------------------------------------

    /// Execute a navigation tool with `&mut self` access.
    /// Navigation tools are always allowed, even when edits are disabled.
    pub(crate) fn execute_navigation_tool(
        &mut self,
        name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let tool_def = match self.ai_state.tool_registry.get(name) {
            Some(t) => t.clone(),
            None => return ToolResult::Error(format!("unknown tool: {name}")),
        };

        let caps = self.build_chat_capabilities();
        if !caps.contains(&tool_def.required_scope) {
            return ToolResult::Error(format!(
                "tool '{}' requires scope not granted by current context",
                name
            ));
        }

        match name {
            "open_file" => match parse_args(args) {
                Ok(a) => self.handle_open_file(a),
                Err(e) => e,
            },
            "select_text" => match parse_args(args) {
                Ok(a) => self.handle_select_text(a),
                Err(e) => e,
            },
            _ => ToolResult::Error(format!("unknown navigation tool: {name}")),
        }
    }

    // -----------------------------------------------------------------
    // Navigation handlers
    // -----------------------------------------------------------------

    fn handle_open_file(&mut self, args: OpenFileArgs) -> ToolResult {
        let rel_path = args.path;

        if rel_path.contains("..") {
            return ToolResult::Error("path traversal (..) not allowed".to_string());
        }

        let project_root =
            match std::env::current_dir() {
                Ok(root) => root,
                Err(_) => return ToolResult::Error(
                    "No project root detected. Project-level tools require a working directory."
                        .to_string(),
                ),
            };

        let candidate = project_root.join(&rel_path);
        // Validate it stays within project root
        let normalized = candidate.canonicalize().unwrap_or(candidate.clone());
        let root_normalized = project_root.canonicalize().unwrap_or(project_root.clone());
        if !normalized.starts_with(&root_normalized) {
            return ToolResult::Error("path is outside project root".to_string());
        }

        if !candidate.is_file() {
            return ToolResult::Error(format!(
                "'{}' is not a file. Use list_files to see available files.",
                rel_path
            ));
        }

        // Open the file (switches to existing buffer or creates new one)
        if let Err(e) = self.open_file(&candidate) {
            return ToolResult::Error(format!("failed to open '{}': {}", rel_path, e));
        }

        // Navigate to requested position (1-indexed -> 0-indexed)
        let line = args.line.saturating_sub(1);
        let col = args.column.saturating_sub(1);

        // Clamp to buffer bounds
        let max_line = self.buffer().rope().len_lines().saturating_sub(1);
        let target_line = line.min(max_line);
        self.buffer_mut()
            .cursor_mut()
            .set_position(target_line, GraphemeCol(col));
        self.buffer_mut().validate_cursor_position();
        self.center_cursor_in_viewport();

        // Also update the chat's active_buffer_id to point at the newly opened buffer.
        // Keep chat view mode unchanged; users can explicitly enter review focus.
        let opened_buffer_id = self.buffer().id();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = opened_buffer_id;
        }

        // Return a snippet around the target position
        let actual_line = self.buffer().cursor().line() + 1; // back to 1-indexed
        let actual_col = self.buffer().cursor().col().0 + 1;
        let total_lines = self.buffer().rope().len_lines();
        let snip = snippet_around(self.buffer().rope(), actual_line, actual_line, 5);
        ToolResult::Success(format!(
            "Opened {} at line {}, column {} ({} lines total).\n{}",
            rel_path, actual_line, actual_col, total_lines, snip
        ))
    }

    fn handle_select_text(&mut self, args: SelectTextArgs) -> ToolResult {
        let start_line = args.start_line;
        let end_line = args.end_line;

        if start_line < 1 {
            return ToolResult::Error("'start_line' is required (>= 1)".to_string());
        }
        if end_line < 1 {
            return ToolResult::Error("'end_line' is required (>= 1)".to_string());
        }

        if start_line > end_line {
            return ToolResult::Error(format!(
                "start_line ({start_line}) must be <= end_line ({end_line})"
            ));
        }

        let line_count = self.buffer().rope().len_lines();
        if start_line > line_count {
            return ToolResult::Error(format!(
                "start_line ({start_line}) exceeds buffer line count ({line_count})"
            ));
        }
        let end_line = end_line.min(line_count);

        // Convert 1-indexed to 0-indexed
        let start_line_0 = start_line - 1;
        let end_line_0 = end_line - 1;

        let start_col = args.start_column.saturating_sub(1);

        let end_col = match args.end_column {
            // The public value is the 1-indexed last included column. Its
            // numeric value is therefore the zero-based exclusive boundary.
            Some(n) => n,
            None => {
                // Default to end of the end_line — grapheme count of the
                // line *content* (terminator excluded), so the selection
                // doesn't run past the `\n` into the next line.
                let rope = self.buffer().rope();
                crate::unicode::grapheme_count(&crate::display::line_content(rope, end_line_0))
            }
        };

        // Compute char offsets for the selection snapshot
        let rope = self.buffer().rope();
        let start_char = rope.line_to_char(start_line_0)
            + crate::unicode::grapheme_to_char_col(
                &rope.line(start_line_0).to_string(),
                GraphemeCol(start_col),
            )
            .0;
        let end_char = rope.line_to_char(end_line_0)
            + crate::unicode::grapheme_to_char_col(
                &rope.line(end_line_0).to_string(),
                GraphemeCol(end_col),
            )
            .0;

        // Store as the active selection used by chat tools and rendering.
        let selected_text = if end_char > start_char {
            rope.slice(start_char..end_char.min(rope.len_chars()))
                .to_string()
        } else {
            String::new()
        };

        self.ai_state.active_selection = Some(crate::editor::ai_state::AiSelectionSnapshot {
            buffer_id: self.buffer().id(),
            start_line: start_line_0,
            start_col,
            end_line: end_line_0,
            end_col,
            start_char,
            end_char,
            anchor_line: start_line_0,
            selected_text: selected_text.clone(),
            selection_mode: self.mode(),
        });

        // Move cursor to the midpoint and center
        let mid_line = (start_line_0 + end_line_0) / 2;
        self.buffer_mut()
            .cursor_mut()
            .set_position(mid_line, GraphemeCol(0));
        self.center_cursor_in_viewport();

        let file_label = self.buffer().file_path().unwrap_or("[No Name]").to_string();
        let snip = snippet_around(self.buffer().rope(), start_line, end_line, 2);
        ToolResult::Success(format!(
            "Selected lines {start_line}-{end_line} in {file_label} ({} chars).\n{snip}",
            selected_text.len()
        ))
    }

    // -----------------------------------------------------------------
    // Mutation tool dispatch
    // -----------------------------------------------------------------

    /// Execute a mutation tool with `&mut self` access.
    pub(crate) fn execute_mutation_tool(
        &mut self,
        name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        // Verify the tool exists and is a mutation
        let tool_def = match self.ai_state.tool_registry.get(name) {
            Some(t) => t.clone(),
            None => return ToolResult::Error(format!("unknown tool: {name}")),
        };

        // Check capabilities allow this side effect
        let caps = self.build_chat_capabilities();
        if !caps.allows_side_effect(tool_def.side_effect) {
            return ToolResult::Error(format!(
                "tool '{}' blocked: mutations not allowed in current context",
                name
            ));
        }
        if !caps.contains(&tool_def.required_scope) {
            return ToolResult::Error(format!(
                "tool '{}' requires scope not granted by current context",
                name
            ));
        }

        // Resolve target buffer from the chat session, then dispatch.
        let original = self.current_buffer_index;
        let target = match self.ai_state.chat.as_ref() {
            Some(chat) => match self.find_buffer_index_by_id(chat.active_buffer_id) {
                Some(idx) => idx,
                None => {
                    return ToolResult::Error(format!(
                        "Active chat target is no longer available (buffer id {}). Re-open the target file with open_file before mutating.",
                        chat.active_buffer_id
                    ))
                }
            },
            None => original,
        };

        // Validate target buffer index
        if target >= self.buffers.len() {
            return ToolResult::Error(format!(
                "target buffer index {} is out of range ({})",
                target,
                self.buffers.len()
            ));
        }
        let target_buffer_id = self.buffers[target].id();

        self.current_buffer_index = target;

        let revision_before = self.buffer().version();
        let file_label = self.buffer().file_path().unwrap_or("[No Name]").to_string();
        let revision_error = (name != "snapshot_file")
            .then(|| validate_expected_revision(args, revision_before, &file_label))
            .and_then(Result::err);
        if let Some(error) = revision_error {
            self.current_buffer_index = original;
            return error;
        }
        let content_before = self.buffer().rope().to_string();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.last_save_outcome = None;
        }
        let mut result = match name {
            "edit_range" => match parse_args(args) {
                Ok(a) => self.handle_edit_range(a),
                Err(e) => e,
            },
            "insert_lines" => match parse_args(args) {
                Ok(a) => self.handle_insert_lines(a),
                Err(e) => e,
            },
            "delete_lines" => match parse_args(args) {
                Ok(a) => self.handle_delete_lines(a),
                Err(e) => e,
            },
            "write_file_at_path" => match parse_args(args) {
                Ok(a) => self.handle_write_file_at_path(a, false),
                Err(e) => e,
            },
            "create_file" => match parse_args(args) {
                Ok(a) => self.handle_write_file_at_path(a, true),
                Err(e) => e,
            },
            "apply_patch_at_path" => match parse_args(args) {
                Ok(a) => self.handle_apply_patch_at_path(a),
                Err(e) => e,
            },
            "snapshot_file" => match parse_args(args) {
                Ok(a) => self.handle_snapshot_file(a),
                Err(e) => e,
            },
            "restore_file" => match parse_args(args) {
                Ok(a) => self.handle_restore_file(a),
                Err(e) => e,
            },
            _ => ToolResult::Error(format!("unknown mutation tool: {name}")),
        };

        if let ToolResult::Success(message) = &mut result {
            let revision_after = self.buffer().version();
            let content_after = self.buffer().rope().to_string();
            let diffstat = mutation_diffstat(&content_before, &content_after);
            let save_outcome = self
                .ai_state
                .chat
                .as_ref()
                .and_then(|chat| chat.last_save_outcome.as_deref());
            append_mutation_envelope(
                message,
                name,
                &file_label,
                revision_before,
                revision_after,
                &diffstat,
                save_outcome,
            );
        }

        // Only restore if active_buffer_id didn't change during the mutation
        // (e.g., open_file could have changed it)
        let current_active = self.ai_state.chat.as_ref().map(|c| c.active_buffer_id);
        if current_active == Some(target_buffer_id) {
            self.current_buffer_index = original;
        }
        result
    }

    /// Shared post-mutation refresh: rehighlight, diagnostics, mark dirty.
    fn post_mutation_refresh(&mut self) {
        if self.buffer().needs_rehighlight() {
            self.process_viewport_rehighlight();
        }
        self.request_diagnostics_refresh();
        self.mark_dirty();
    }

    fn record_ai_chat_save_outcome(&mut self, outcome: impl Into<String>) -> String {
        let outcome = outcome.into();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.last_save_outcome = Some(outcome.clone());
        }
        outcome
    }

    fn auto_save_current_buffer_if_configured(&mut self) -> String {
        if self.buffer().file_path().is_none() {
            return self.record_ai_chat_save_outcome(
                "not saved (target buffer has no file path)".to_string(),
            );
        }

        let should_save = self
            .ai_state
            .chat
            .as_ref()
            .is_some_and(|c| c.buffer_was_clean_at_chat_start);
        if !should_save {
            return self.record_ai_chat_save_outcome(
                "not auto-saved (policy only_if_clean_at_start)".to_string(),
            );
        }

        match self.buffer_mut().save() {
            Ok(()) => {
                self.mark_saved();
                self.record_ai_chat_save_outcome("auto-saved".to_string())
            }
            Err(e) => self.record_ai_chat_save_outcome(format!("auto-save failed: {e}")),
        }
    }

    // -----------------------------------------------------------------
    // Mutation handlers
    // -----------------------------------------------------------------

    fn force_save_current_buffer(&mut self, path_hint: &str) -> Result<String, ToolResult> {
        let Some(target_path) = self.buffer().file_path().map(std::path::PathBuf::from) else {
            self.record_ai_chat_save_outcome("save failed (target buffer has no file path)");
            return Err(ToolResult::Error(format!(
                "cannot save '{path_hint}': target buffer has no file path"
            )));
        };
        if let Some(parent) = target_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.record_ai_chat_save_outcome(format!("save failed: {e}"));
                return Err(ToolResult::Error(format!(
                    "failed to create parent directory '{}': {}",
                    parent.display(),
                    e
                )));
            }
        }
        if let Err(e) = self.buffer_mut().save() {
            self.record_ai_chat_save_outcome(format!("save failed: {e}"));
            return Err(ToolResult::Error(format!(
                "failed to save '{}': {}",
                path_hint, e
            )));
        }
        self.mark_saved();
        Ok(self.record_ai_chat_save_outcome("saved to disk"))
    }

    fn handle_write_file_at_path(&mut self, args: WriteFileArgs, create_only: bool) -> ToolResult {
        let path = args.path;
        let created_new_file = self
            .buffer()
            .file_path()
            .is_some_and(|target| !std::path::Path::new(target).exists());
        let mut content = args.content;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        if create_only
            && self
                .buffer()
                .file_path()
                .is_some_and(|p| std::path::Path::new(p).exists())
        {
            return ToolResult::Error(format!(
                "'{}' already exists. Use write_file_at_path to overwrite.",
                path
            ));
        }

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();
        let existing_chars = self.buffer().rope().len_chars();

        let ((), edits) = self.buffer_mut().record(|buf| {
            if existing_chars > 0 {
                buf.delete_char_range(0, existing_chars);
            }
            if !content.is_empty() {
                buf.insert_text_at(0, CharCol::ZERO, &content);
            }
        });

        if !edits.is_empty() {
            let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
                .min(self.buffer().rope().len_chars());
            self.set_cursor_from_abs_char(cursor_abs_after);
            let cursor_after = self.cursor_position();
            self.push_recorded_undo(edits, cursor_before, cursor_after);
            self.post_mutation_refresh();
        }

        let save_outcome = match self.force_save_current_buffer(&path) {
            Ok(s) => s,
            Err(e) => return e,
        };
        if created_new_file {
            if let Some(target) = self.buffer().file_path().map(std::path::PathBuf::from) {
                self.record_current_session_temp_file(&target);
            }
        }

        // Mark full file as edited for review mode.
        let buffer_id = self.buffer().id();
        let end_line = self.buffer().line_count().saturating_sub(1);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.agent_edits.all_edits.insert(buffer_id, Vec::new());
            chat.agent_edits.record_edit(buffer_id, 0, end_line);
        }

        let line_count = if content.is_empty() {
            0
        } else {
            content.lines().count()
        };
        let action = if create_only { "Created" } else { "Wrote" };
        let file_label = self.buffer().file_path().unwrap_or(path.as_str());
        ToolResult::Success(format!(
            "{action} {file_label} ({line_count} line{}).\nSave: {save_outcome}.",
            if line_count == 1 { "" } else { "s" },
        ))
    }

    fn handle_apply_patch_at_path(&mut self, args: ApplyPatchArgs) -> ToolResult {
        let path = args.path;
        let diff = args.diff;

        let file_edits = match parse_apply_patch(&diff) {
            Ok(edits) => edits,
            Err(e) => {
                return ToolResult::Error(format!(
                "invalid apply_patch diff: {e}. Expected *** Begin Patch / *** End Patch format."
            ))
            }
        };
        if file_edits.len() != 1 {
            return ToolResult::Error(format!(
                "apply_patch_at_path expects exactly one file section, got {}",
                file_edits.len()
            ));
        }

        let file_edit = &file_edits[0];
        if let Some(diff_path) = file_edit.path.as_ref() {
            let expected = normalize_path_label(&path);
            let provided = normalize_path_label(diff_path);
            if expected != provided {
                return ToolResult::Error(format!(
                    "patch path '{}' does not match requested path '{}'",
                    diff_path, path
                ));
            }
        }

        if file_edit.hunks.is_empty() {
            return ToolResult::Success("Patch contained no hunks; no changes made.".to_string());
        }

        let original = self.buffer().rope().to_string();
        let patched = match apply_patch_hunks_to_text(original, &file_edit.hunks) {
            Ok(text) => text,
            Err(e) => return e,
        };

        let write_args = WriteFileArgs {
            path: path.clone(),
            content: patched,
        };

        match self.handle_write_file_at_path(write_args, false) {
            ToolResult::Success(msg) => ToolResult::Success(format!(
                "Applied {} patch hunk(s) to {}.\n{}",
                file_edit.hunks.len(),
                path,
                msg
            )),
            ToolResult::Error(err) => ToolResult::Error(err),
        }
    }

    fn handle_snapshot_file(&mut self, args: PathArgs) -> ToolResult {
        let path = args.path;
        let snapshot_path = self
            .buffer()
            .file_path()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(path.clone()));
        let snapshot_content = self.buffer().rope().to_string();
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return ToolResult::Error("no active chat session".to_string());
        };

        let snapshot_id = format!("snap_{}", chat.next_snapshot_id);
        chat.next_snapshot_id = chat.next_snapshot_id.saturating_add(1);
        chat.file_snapshots.insert(
            snapshot_id.clone(),
            super::ai_chat_state::FileSnapshot {
                path: snapshot_path,
                content: snapshot_content,
            },
        );

        ToolResult::Success(format!(
            "Snapshot created: {} for {}",
            snapshot_id,
            self.buffer().file_path().unwrap_or("[No Name]")
        ))
    }

    fn handle_restore_file(&mut self, args: RestoreFileArgs) -> ToolResult {
        let path = args.path;
        let snapshot_id = args.snapshot_id;
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return ToolResult::Error("no active chat session".to_string());
        };
        let Some(snapshot) = chat.file_snapshots.get(&snapshot_id).cloned() else {
            return ToolResult::Error(format!("unknown snapshot_id '{}'", snapshot_id));
        };

        let current_path = self
            .buffer()
            .file_path()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(path.clone()));
        let current_path = current_path
            .canonicalize()
            .unwrap_or_else(|_| current_path.clone());
        let snapshot_path = snapshot
            .path
            .canonicalize()
            .unwrap_or_else(|_| snapshot.path.clone());
        if current_path != snapshot_path {
            return ToolResult::Error(format!(
                "snapshot '{}' was captured for '{}', not '{}'",
                snapshot_id,
                snapshot.path.display(),
                current_path.display()
            ));
        }

        let restore_args = WriteFileArgs {
            path: path.to_string(),
            content: snapshot.content,
        };
        self.handle_write_file_at_path(restore_args, false)
    }

    fn handle_edit_range(&mut self, args: EditRangeArgs) -> ToolResult {
        let start_line = args.start_line;
        let end_line = args.end_line;
        let new_text = args.new_text;

        if start_line < 1 {
            return ToolResult::Error("'start_line' is required (>= 1)".to_string());
        }
        if end_line < 1 {
            return ToolResult::Error("'end_line' is required (>= 1)".to_string());
        }

        if start_line > end_line {
            return ToolResult::Error(format!(
                "start_line ({start_line}) must be <= end_line ({end_line})"
            ));
        }

        let line_count = self.buffer().rope().len_lines();
        if start_line > line_count {
            return ToolResult::Error(format!(
                "start_line ({start_line}) exceeds buffer line count ({line_count})"
            ));
        }
        let end_line = end_line.min(line_count);

        // Convert 1-indexed lines to char range
        let start_char = self.buffer().rope().line_to_char(start_line - 1);
        let end_char = if end_line >= line_count {
            self.buffer().rope().len_chars()
        } else {
            self.buffer().rope().line_to_char(end_line)
        };

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();

        let ((), edits) = self.buffer_mut().record(|buf| {
            if end_char > start_char {
                buf.delete_char_range(start_char, end_char);
            }
            let insert_pos = start_char.min(buf.rope().len_chars());
            let line = buf.rope().char_to_line(insert_pos);
            let col = insert_pos - buf.rope().line_to_char(line);
            if !new_text.is_empty() {
                let mut text = new_text.clone();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                buf.insert_text_at(line, CharCol(col), &text);
            }
        });

        if edits.is_empty() {
            return ToolResult::Success("No changes made (content identical).".to_string());
        }

        let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
            .min(self.buffer().rope().len_chars());
        self.set_cursor_from_abs_char(cursor_abs_after);
        let cursor_after = self.cursor_position();
        self.push_recorded_undo(edits, cursor_before, cursor_after);
        self.post_mutation_refresh();
        let save_outcome = self.auto_save_current_buffer_if_configured();

        let new_line_count = self.buffer().rope().len_lines();
        let new_text_lines = new_text.lines().count().max(1);
        let new_end = start_line + new_text_lines - 1;

        // Record agent edit (convert 1-indexed to 0-indexed)
        let buffer_id = self.buffer().id();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.agent_edits
                .record_edit(buffer_id, start_line - 1, new_end - 1);
        }

        let snippet = snippet_around(self.buffer().rope(), start_line, new_end, 3);
        ToolResult::Success(format!(
            "Replaced lines {start_line}-{end_line} (buffer now has {new_line_count} lines).\nSave: {save_outcome}.\n{snippet}"
        ))
    }

    fn handle_insert_lines(&mut self, args: InsertLinesArgs) -> ToolResult {
        let after_line = args.after_line;
        let text = args.text;

        let line_count = self.buffer().rope().len_lines();
        if after_line > line_count {
            return ToolResult::Error(format!(
                "after_line ({after_line}) exceeds buffer line count ({line_count})"
            ));
        }

        // Compute insertion offset
        let insert_char = if after_line == 0 {
            0
        } else if after_line >= line_count {
            self.buffer().rope().len_chars()
        } else {
            self.buffer().rope().line_to_char(after_line)
        };

        let mut insert_text = text;
        if !insert_text.ends_with('\n') {
            insert_text.push('\n');
        }
        // If inserting at end and buffer doesn't end with newline, prepend one
        if after_line >= line_count && self.buffer().rope().len_chars() > 0 {
            let last_char = self
                .buffer()
                .rope()
                .char(self.buffer().rope().len_chars() - 1);
            if last_char != '\n' {
                insert_text = format!("\n{insert_text}");
            }
        }

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();

        let ((), edits) = self.buffer_mut().record(|buf| {
            let pos = insert_char.min(buf.rope().len_chars());
            let line = buf.rope().char_to_line(pos);
            let col = pos - buf.rope().line_to_char(line);
            buf.insert_text_at(line, CharCol(col), &insert_text);
        });

        if edits.is_empty() {
            return ToolResult::Success("No changes made.".to_string());
        }

        let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
            .min(self.buffer().rope().len_chars());
        self.set_cursor_from_abs_char(cursor_abs_after);
        let cursor_after = self.cursor_position();
        self.push_recorded_undo(edits, cursor_before, cursor_after);
        self.post_mutation_refresh();
        let save_outcome = self.auto_save_current_buffer_if_configured();

        let new_line_count = self.buffer().rope().len_lines();
        let inserted_lines = insert_text.lines().count().max(1);
        let ins_start = after_line + 1;
        let ins_end = after_line + inserted_lines;

        // Record agent edit and adjust existing ranges (convert 1-indexed to 0-indexed)
        let buffer_id = self.buffer().id();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.agent_edits
                .adjust_for_insert(buffer_id, after_line, inserted_lines);
            chat.agent_edits
                .record_edit(buffer_id, ins_start - 1, ins_end - 1);
        }

        let snippet = snippet_around(self.buffer().rope(), ins_start, ins_end, 3);
        ToolResult::Success(format!(
            "Inserted text after line {after_line} (buffer now has {new_line_count} lines).\nSave: {save_outcome}.\n{snippet}"
        ))
    }

    fn handle_delete_lines(&mut self, args: DeleteLinesArgs) -> ToolResult {
        let start_line = args.start_line;
        let end_line = args.end_line;

        if start_line < 1 {
            return ToolResult::Error("'start_line' is required (>= 1)".to_string());
        }
        if end_line < 1 {
            return ToolResult::Error("'end_line' is required (>= 1)".to_string());
        }

        if start_line > end_line {
            return ToolResult::Error(format!(
                "start_line ({start_line}) must be <= end_line ({end_line})"
            ));
        }

        let line_count = self.buffer().rope().len_lines();
        if start_line > line_count {
            return ToolResult::Error(format!(
                "start_line ({start_line}) exceeds buffer line count ({line_count})"
            ));
        }
        let end_line = end_line.min(line_count);

        let start_char = self.buffer().rope().line_to_char(start_line - 1);
        let end_char = if end_line >= line_count {
            self.buffer().rope().len_chars()
        } else {
            self.buffer().rope().line_to_char(end_line)
        };

        if end_char <= start_char {
            return ToolResult::Success("No lines to delete.".to_string());
        }

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();

        let ((), edits) = self.buffer_mut().record(|buf| {
            buf.delete_char_range(start_char, end_char);
        });

        if edits.is_empty() {
            return ToolResult::Success("No changes made.".to_string());
        }

        let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
            .min(self.buffer().rope().len_chars());
        self.set_cursor_from_abs_char(cursor_abs_after);
        let cursor_after = self.cursor_position();
        self.push_recorded_undo(edits, cursor_before, cursor_after);
        self.post_mutation_refresh();
        let save_outcome = self.auto_save_current_buffer_if_configured();

        let deleted_count = end_line - start_line + 1;
        let new_line_count = self.buffer().rope().len_lines();

        // Adjust existing ranges for deletion (convert 1-indexed to 0-indexed)
        let buffer_id = self.buffer().id();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.agent_edits
                .adjust_for_delete(buffer_id, start_line - 1, end_line - 1);
        }

        // Show context around the deletion point
        let snippet = snippet_around(self.buffer().rope(), start_line, start_line, 3);
        ToolResult::Success(format!(
            "Deleted {deleted_count} line(s) ({start_line}-{end_line}). Buffer now has {new_line_count} lines.\nSave: {save_outcome}.\n{snippet}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::formats::Hunk;
    use crate::ai::tools::ToolResult;

    fn hunk(search: &str, replace: &str) -> Hunk {
        Hunk {
            search: search.to_string(),
            replace: replace.to_string(),
        }
    }

    fn buf_content(editor: &Editor) -> String {
        editor.buffer().rope().to_string()
    }

    #[test]
    fn select_text_explicit_end_column_is_inclusive_and_grapheme_safe() {
        let mut editor = Editor::with_content("a🙂b\n");

        let result = editor.handle_select_text(SelectTextArgs {
            start_line: 1,
            start_column: 2,
            end_line: 1,
            end_column: Some(2),
        });

        assert!(matches!(result, ToolResult::Success(_)));
        let selection = editor.ai_state.active_selection.as_ref().unwrap();
        assert_eq!(selection.selected_text, "🙂");
        assert_eq!((selection.start_col, selection.end_col), (1, 2));
    }

    #[test]
    fn mutation_diffstat_reports_single_changed_region() {
        assert_eq!(
            mutation_diffstat("one\ntwo\nthree\n", "one\nsecond\nthird\nthree\n"),
            MutationDiffstat {
                added: 2,
                removed: 1,
                current_range: Some((2, 3)),
                deletion_point: None,
            }
        );
    }

    #[test]
    fn mutation_diffstat_reports_pure_deletion_point() {
        assert_eq!(
            mutation_diffstat("one\ntwo\nthree\n", "one\nthree\n"),
            MutationDiffstat {
                added: 0,
                removed: 1,
                current_range: None,
                deletion_point: Some(2),
            }
        );
    }

    #[test]
    fn mutation_envelope_has_stable_fields() {
        let mut message = "Inserted text.".to_string();
        append_mutation_envelope(
            &mut message,
            "insert_lines",
            "src/lib.rs",
            7,
            8,
            &MutationDiffstat {
                added: 2,
                removed: 0,
                current_range: Some((4, 5)),
                deletion_point: None,
            },
            Some("auto-saved"),
        );

        assert!(message.contains("Mutation: insert_lines"));
        assert!(message.contains("File: src/lib.rs"));
        assert!(message.contains("Changed range: lines 4-5"));
        assert!(message.contains("Diffstat: +2 -0"));
        assert!(message.contains("Buffer revision: 7 -> 8"));
        assert!(message.contains("Diagnostics: refresh requested"));
        assert!(message.contains("Save: auto-saved"));
        assert!(message.contains("Provenance: agent"));
        assert!(message.contains("Rollback: editor undo"));
    }

    // ====================================================================
    // parse_args tests
    // ====================================================================

    #[test]
    fn parse_args_malformed_returns_clean_error() {
        let bad_json = serde_json::json!({"start_line": "not a number"});
        let result = parse_args::<EditRangeArgs>(&bad_json);
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolResult::Error(msg) => assert!(msg.starts_with("Invalid arguments:")),
            _ => panic!("expected ToolResult::Error"),
        }
    }

    #[test]
    fn parse_args_missing_required_field() {
        let bad_json = serde_json::json!({"start_line": 1});
        let result = parse_args::<EditRangeArgs>(&bad_json);
        assert!(result.is_err());
    }

    #[test]
    fn expected_revision_accepts_current_buffer_revision() {
        let args = serde_json::json!({"expected_revision": 12});
        assert!(validate_expected_revision(&args, 12, "src/lib.rs").is_ok());
    }

    #[test]
    fn expected_revision_is_required() {
        let error =
            validate_expected_revision(&serde_json::json!({}), 12, "src/lib.rs").unwrap_err();
        match error {
            ToolResult::Error(message) => {
                assert!(message.contains("'expected_revision' is required"));
                assert!(message.contains("Re-read"));
            }
            _ => panic!("expected ToolResult::Error"),
        }
    }

    #[test]
    fn expected_revision_rejects_stale_buffer_revision() {
        let args = serde_json::json!({"expected_revision": 12});
        let error = validate_expected_revision(&args, 14, "src/lib.rs").unwrap_err();
        match error {
            ToolResult::Error(message) => {
                assert!(message.contains("advanced from revision 12 to 14"));
                assert!(message.contains("Re-read"));
            }
            _ => panic!("expected ToolResult::Error"),
        }
    }

    #[test]
    fn expected_revision_must_be_non_negative_integer() {
        let args = serde_json::json!({"expected_revision": -1});
        assert!(validate_expected_revision(&args, 0, "src/lib.rs").is_err());
    }

    // ====================================================================
    // edit_range tests
    // ====================================================================

    #[test]
    fn edit_range_replaces_single_line() {
        let mut editor = Editor::with_content("line 1\nline 2\nline 3\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 2,
            end_line: 2,
            new_text: "replaced\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "line 1\nreplaced\nline 3\n");
    }

    #[test]
    fn edit_range_replaces_multiple_lines() {
        let mut editor = Editor::with_content("aaa\nbbb\nccc\nddd\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 2,
            end_line: 3,
            new_text: "XXX\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "aaa\nXXX\nddd\n");
    }

    #[test]
    fn edit_range_adds_trailing_newline() {
        let mut editor = Editor::with_content("line 1\nline 2\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 1,
            end_line: 1,
            new_text: "no newline".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        // Should auto-add trailing newline
        assert!(buf_content(&editor).starts_with("no newline\n"));
    }

    #[test]
    fn edit_range_start_exceeds_buffer() {
        let mut editor = Editor::with_content("only line\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 99,
            end_line: 99,
            new_text: "nope".into(),
        });
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[test]
    fn edit_range_end_clamped_to_buffer() {
        let mut editor = Editor::with_content("aaa\nbbb\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 1,
            end_line: 999,
            new_text: "replaced all\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "replaced all\n");
    }

    #[test]
    fn edit_range_start_greater_than_end() {
        let mut editor = Editor::with_content("line 1\nline 2\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 3,
            end_line: 1,
            new_text: "bad".into(),
        });
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[test]
    fn edit_range_with_empty_new_text() {
        let mut editor = Editor::with_content("keep\ndelete me\nalso keep\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 2,
            end_line: 2,
            new_text: "".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "keep\nalso keep\n");
    }

    #[test]
    fn edit_range_replace_last_line() {
        let mut editor = Editor::with_content("first\nlast\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 2,
            end_line: 2,
            new_text: "new last\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "first\nnew last\n");
    }

    #[test]
    fn edit_range_expand_single_to_multiple() {
        let mut editor = Editor::with_content("before\ntarget\nafter\n");
        let result = editor.handle_edit_range(EditRangeArgs {
            start_line: 2,
            end_line: 2,
            new_text: "line a\nline b\nline c\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(
            buf_content(&editor),
            "before\nline a\nline b\nline c\nafter\n"
        );
    }

    // ====================================================================
    // insert_lines tests
    // ====================================================================

    #[test]
    fn insert_lines_at_beginning() {
        let mut editor = Editor::with_content("existing\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 0,
            text: "prepended\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "prepended\nexisting\n");
    }

    #[test]
    fn insert_lines_at_end() {
        let mut editor = Editor::with_content("existing\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 1,
            text: "appended\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "existing\nappended\n");
    }

    #[test]
    fn insert_lines_in_middle() {
        let mut editor = Editor::with_content("aaa\nccc\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 1,
            text: "bbb\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "aaa\nbbb\nccc\n");
    }

    #[test]
    fn insert_lines_beyond_buffer_is_error() {
        let mut editor = Editor::with_content("one line\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 99,
            text: "nope\n".into(),
        });
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[test]
    fn insert_lines_adds_trailing_newline() {
        let mut editor = Editor::with_content("existing\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 0,
            text: "no newline".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert!(buf_content(&editor).contains("no newline\n"));
    }

    #[test]
    fn insert_lines_multiple_lines() {
        let mut editor = Editor::with_content("start\nend\n");
        let result = editor.handle_insert_lines(InsertLinesArgs {
            after_line: 1,
            text: "mid 1\nmid 2\nmid 3\n".into(),
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "start\nmid 1\nmid 2\nmid 3\nend\n");
    }

    // ====================================================================
    // delete_lines tests
    // ====================================================================

    #[test]
    fn delete_lines_single_line() {
        let mut editor = Editor::with_content("keep\ndelete\nalso keep\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 2,
            end_line: 2,
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "keep\nalso keep\n");
    }

    #[test]
    fn delete_lines_multiple() {
        let mut editor = Editor::with_content("a\nb\nc\nd\ne\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 2,
            end_line: 4,
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "a\ne\n");
    }

    #[test]
    fn delete_lines_all() {
        let mut editor = Editor::with_content("a\nb\nc\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 1,
            end_line: 3,
        });
        assert!(matches!(result, ToolResult::Success(_)));
        // Buffer should be empty or have a single empty line
        assert!(buf_content(&editor).is_empty() || buf_content(&editor) == "\n");
    }

    #[test]
    fn delete_lines_start_greater_than_end() {
        let mut editor = Editor::with_content("a\nb\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 3,
            end_line: 1,
        });
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[test]
    fn delete_lines_start_beyond_buffer() {
        let mut editor = Editor::with_content("a\nb\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 99,
            end_line: 99,
        });
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[test]
    fn delete_lines_end_clamped() {
        let mut editor = Editor::with_content("a\nb\nc\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 2,
            end_line: 999,
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "a\n");
    }

    #[test]
    fn delete_lines_last_line() {
        let mut editor = Editor::with_content("first\nlast\n");
        let result = editor.handle_delete_lines(DeleteLinesArgs {
            start_line: 2,
            end_line: 2,
        });
        assert!(matches!(result, ToolResult::Success(_)));
        assert_eq!(buf_content(&editor), "first\n");
    }

    #[test]
    fn apply_single_hunk_exact_match() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n".to_string();
        let hunks = vec![hunk(
            "    println!(\"hello\");\n",
            "    println!(\"world\");\n",
        )];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert!(result.contains("world"));
        assert!(!result.contains("hello"));
    }

    #[test]
    fn apply_multiple_hunks_sequentially() {
        let content = "aaa\nbbb\nccc\n".to_string();
        let hunks = vec![hunk("aaa\n", "AAA\n"), hunk("ccc\n", "CCC\n")];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert_eq!(result, "AAA\nbbb\nCCC\n");
    }

    #[test]
    fn apply_hunk_not_found_returns_error() {
        let content = "fn foo() {}\n".to_string();
        let hunks = vec![hunk("fn bar() {}\n", "fn baz() {}\n")];
        let result = apply_patch_hunks_to_text(content, &hunks);
        assert!(result.is_err());
    }

    #[test]
    fn apply_empty_search_appends() {
        let content = "existing\n".to_string();
        let hunks = vec![hunk("", "appended\n")];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert_eq!(result, "existing\nappended\n");
    }

    #[test]
    fn apply_deletion_hunk() {
        let content = "keep\nremove_me\nalso_keep\n".to_string();
        let hunks = vec![hunk("remove_me\n", "")];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert_eq!(result, "keep\nalso_keep\n");
    }

    #[test]
    fn apply_hunk_replaces_first_occurrence() {
        // When there are duplicate matches, only the first should be replaced
        let content = "dup\ndup\n".to_string();
        let hunks = vec![hunk("dup\n", "REPLACED\n")];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        // First "dup\n" replaced, second remains
        assert_eq!(result, "REPLACED\ndup\n");
    }

    #[test]
    fn apply_cumulative_hunks() {
        // Second hunk should match against the result of the first hunk
        let content = "old_value\n".to_string();
        let hunks = vec![
            hunk("old_value\n", "mid_value\n"),
            hunk("mid_value\n", "new_value\n"),
        ];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert_eq!(result, "new_value\n");
    }

    #[test]
    fn apply_multiline_hunk() {
        let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n".to_string();
        let hunks = vec![hunk(
            "    let x = 1;\n    let y = 2;\n",
            "    let x = 10;\n    let y = 20;\n    let z = 30;\n",
        )];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert!(result.contains("let x = 10;"));
        assert!(result.contains("let z = 30;"));
        assert!(!result.contains("let x = 1;"));
    }

    #[test]
    fn apply_hunk_preserves_surrounding_content() {
        let content = "before\ntarget\nafter\n".to_string();
        let hunks = vec![hunk("target\n", "replaced\n")];
        let result = apply_patch_hunks_to_text(content, &hunks).unwrap();
        assert_eq!(result, "before\nreplaced\nafter\n");
    }
}
