use crate::ai::chat_types::{ToolCallInfo, ToolSummaryKind};
use crate::ai::path_policy::{canonicalize_or_normalize, sensitive_path_reason};
use crate::ai::scope::{Capabilities, ScopeContext};
use crate::ai::skills::ACTIVATE_SKILL_TOOL;
use crate::ai::tools::builtins::{OpenBufferState, ToolExecutionContext};
use crate::ai::tools::schema;
use crate::ai::tools::{RuntimeServices, SideEffect, ToolResult};
use crate::ai::{redact_high_risk_tokens, truncate_utf8_with_notice, ToolApprovalMode};
use std::path::{Path, PathBuf};

use super::ai_chat_state::{CodeExplanationContinuation, PendingToolApproval, ToolEventSummary};
use super::ai_tool_path::{compact_tool_label, compact_tool_path, normalize_path};
use super::Editor;

#[derive(Debug, Clone)]
pub(super) struct ToolApprovalRequest {
    pub(super) requested_path: PathBuf,
    pub(super) approval_root: PathBuf,
    pub(super) reason: String,
    pub(super) message: String,
}

pub(super) enum ToolDispatchOutcome {
    Completed(ToolResult),
    ApprovalRequired(ToolApprovalRequest),
}

pub(super) enum ToolPathResolution {
    Allowed {
        absolute_path: PathBuf,
        boundary_root: PathBuf,
    },
    NeedsApproval(ToolApprovalRequest),
}

impl Editor {
    // -----------------------------------------------------------------
    // Tool execution helpers
    // -----------------------------------------------------------------

    /// Build capabilities for the current chat session.
    pub(crate) fn build_chat_capabilities(&self) -> Capabilities {
        let profile_name = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|c| c.opts.profile.clone())
            .unwrap_or_else(|| self.ai_state.active_profile.clone());
        let profile_scope = self
            .ai_state
            .config
            .resolve_profile(&profile_name)
            .map(|p| p.scope.clone())
            .unwrap_or_default();

        let allow_edits = self
            .ai_state
            .chat
            .as_ref()
            .map(|c| c.allow_edits)
            .unwrap_or(false);

        // Base capabilities from profile scope
        let mut caps = Capabilities {
            file_scope: profile_scope.files,
            // Enable shell capability for editable chats by default. External
            // execution remains constrained by durable auto-mode policy.
            shell: profile_scope.shell || allow_edits,
            network: profile_scope.network,
            allow_mutations: allow_edits,
        };

        // Without an approved project boundary, force file-scoped access for
        // project tools to prevent broad accidental traversal from process CWD.
        if self.ai_effective_project_root().is_none()
            && caps.file_scope >= crate::ai::FileScope::Project
        {
            caps.file_scope = crate::ai::FileScope::File;
        }

        // If edits not allowed, disable shell but keep file_scope at profile level
        // so read-only project tools (search_project, list_files, read_file_at_path)
        // remain available.
        if !allow_edits {
            caps.shell = false;
        }

        // Mutating/external tools require durable intent/outcome history. A
        // storage/catalog failure leaves project reads and navigation useful,
        // but fails closed before any agent-controlled effect is advertised.
        if !self.durable_ai_mutations_available() {
            caps.allow_mutations = false;
            caps.shell = false;
            caps.network = false;
        }

        // Web search belongs to the Ovim-owned direct Codex harness. It is a
        // read effect and needs neither shell permission nor Codex sandbox
        // access, but is advertised only when usable Exa credentials exist.
        caps.network |= self.ai_chat_uses_direct_codex() && crate::ai::exa::credential().is_some();

        caps
    }

    pub(crate) fn build_chat_runtime_services(&self) -> RuntimeServices {
        let profile_name = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.clone())
            .unwrap_or_else(|| self.ai_state.active_profile.clone());
        let browser_authorized = self
            .ai_state
            .config
            .resolve_profile(&profile_name)
            .is_some_and(|profile| profile.scope.network);
        RuntimeServices {
            browser: browser_authorized && self.services().browser().is_some(),
        }
    }

    /// Build tool JSON schemas for the current chat session's provider.
    pub(crate) fn build_tool_schemas_for_chat(
        &self,
        profile: &crate::ai::AiProfileConfig,
    ) -> Vec<serde_json::Value> {
        let caps = self.build_chat_capabilities();
        let direct_codex = profile.provider == crate::ai::AiProviderKind::Codex;
        let walkthrough_answer = self.ai_code_explanation_answering();
        let mut tools = self
            .ai_state
            .tool_registry
            .tools_for_profile_with_services(profile, &caps, self.build_chat_runtime_services())
            .into_iter()
            .filter(|tool| {
                !crate::ai::tools::subagents::is_parent_control_tool(&tool.name)
                    && (direct_codex
                        || !matches!(
                            tool.name.as_str(),
                            "web_search" | "web_fetch" | "view_image"
                        ))
            })
            .cloned()
            .collect::<Vec<_>>();
        // Compaction changes only Ovim's model-context projection and grants no
        // workspace capability, so keep it available even when a profile uses
        // an explicit operational-tool allowlist. This also makes `/compact`
        // reliable for narrowly configured profiles.
        if !tools
            .iter()
            .any(|tool| tool.name == super::ai_compaction::COMPACT_TOOL)
        {
            if let Some(tool) = self
                .ai_state
                .tool_registry
                .get(super::ai_compaction::COMPACT_TOOL)
            {
                tools.push(tool.clone());
            }
        }
        if walkthrough_answer {
            tools.retain(|tool| tool.side_effect == SideEffect::Read);
        } else {
            tools.extend(self.ai_subagent_parent_tools());
        }
        if self.ai_chat_comprehension_policy() == super::ai_chat_state::ComprehensionPolicy::Off {
            tools.retain(|tool| {
                tool.name != super::ai_comprehension::RECORD_COMPREHENSION_CHECKPOINT_TOOL
            });
        }
        if self.ai_state.skill_catalog.is_empty() {
            tools.retain(|tool| tool.name != ACTIVATE_SKILL_TOOL);
        } else if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool.name == ACTIVATE_SKILL_TOOL)
        {
            if let Some(name) = tool
                .parameters
                .iter_mut()
                .find(|param| param.name == "name")
            {
                name.param_type = crate::ai::tools::ParamType::StringEnum(
                    crate::ai::tools::StringEnum::new(self.ai_state.skill_catalog.names())
                        .expect("a non-empty skill catalog has at least one name"),
                );
            }
        }
        let safe_range = self.ai_code_explanation_safe_range_lines();
        let concept_rows = self.ai_code_explanation_concept_page_rows();
        if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool.name == "explain_with_codebase")
        {
            tool.description.push_str(&format!(
                " The current walkthrough can reliably show at most {concept_rows} wrapped body rows on each concept page and at most {safe_range} visual code rows per code page; every inclusive start_line..end_line range must stay within that limit after soft wrapping. Keep each code comment within 5 wrapped rows. Treat all limits as maximums, not targets: choose the fewest words and code rows that establish one idea. Prefer one or two introductory concept pages when a mental model will reduce cognitive load, then move to concrete code. If a concept page needs two ideas or exceeds its row budget, split it into multiple consecutive concept pages. Never expand a selection to the full function merely for context. Split before the reader must retain two new facts at once, and freely revisit a range when each visit teaches a different relationship or consequence. If validation rejects a page, use its measured rows and suggestions to choose a semantic split, then retry."
            ));
            if let Some(steps) = tool
                .parameters
                .iter_mut()
                .find(|param| param.name == "steps")
            {
                steps.description = format!(
                    "Narratively ordered, bite-sized concept and code pages. A concept page may occupy at most {concept_rows} wrapped body rows; use it for one introductory mental model, prerequisite, transition, or synthesis without a code location. Prefer several focused concept pages over one dense page, and move to concrete code as soon as the reader is oriented. Code pages use project-relative or absolute paths and 1-indexed inclusive lines; outside-project paths must already be approved for the chat. Each optional range may occupy at most {safe_range} visual code rows after soft wrapping, and each comment may occupy at most 5 wrapped rows. Use single-line anchors for handoffs or invariants. Otherwise select the smallest condition, assignment, call, or block that proves the comment; never include the full surrounding function by default. Give every page one new idea and one necessary connection. If one page needs two ideas, split it into two pages. Repeating a range is encouraged when a later page adds a distinct perspective rather than restating the earlier comment."
                );
            }
        }
        let tools = tools.iter().collect::<Vec<_>>();
        // Codex itself remains `approvalPolicy: never` and read-only. Effects
        // are advertised only when the durable ovim harness granted the
        // corresponding capability; app-server calls them as dynamic tools and
        // ovim records intent and applies policy before touching live state.
        if tools.is_empty() {
            return vec![];
        }

        match profile.provider {
            crate::ai::AiProviderKind::Codex
            | crate::ai::AiProviderKind::CodexAppServer
            | crate::ai::AiProviderKind::OpenAi
            | crate::ai::AiProviderKind::Ollama => schema::tools_to_openai_schema(&tools),
            crate::ai::AiProviderKind::Anthropic => schema::tools_to_anthropic_schema(&tools),
        }
    }

    /// Snapshot current editor state into a ToolExecutionContext.
    pub(crate) fn build_tool_execution_context(&self) -> ToolExecutionContext {
        let target_index = self.active_chat_target_buffer_index();
        let buf = &self.buffers[target_index];
        let buffer_content = buf.rope().to_string();
        let file_path = buf.file_path().map(|p| p.to_string());
        let buffer_revision = buf.version();
        let cursor = {
            let c = buf.cursor();
            (c.line(), c.col().0)
        };
        let visible_index = self.current_buffer_index;
        let visible_buf = &self.buffers[visible_index];
        let visible_buffer_content = visible_buf.rope().to_string();
        let visible_file_path = visible_buf.file_path().map(ToString::to_string);
        let visible_buffer_revision = visible_buf.version();
        let visible_cursor = {
            let c = visible_buf.cursor();
            (c.line(), c.col().0)
        };
        let visible_diagnostics = self.get_diagnostics_for_buffer_index(visible_index);
        let visible_current_file = visible_buf
            .file_path()
            .map(PathBuf::from)
            .map(|path| self.absolutize_path(&path));

        // Try to get selection from visual mode or last selection
        let selection = self
            .ai_state
            .active_selection
            .as_ref()
            .filter(|selection| selection.buffer_id == visible_buf.id())
            .map(|s| (s.start_line, s.start_col, s.end_line, s.end_col));

        // Get diagnostics for active target buffer
        let diagnostics = self.get_diagnostics_for_buffer_index(target_index);
        let project_diagnostics = self.get_project_diagnostics_for_chat();

        let current_file = buf
            .file_path()
            .map(PathBuf::from)
            .map(|p| self.absolutize_path(&p));
        let project_root = self.ai_effective_project_root();

        // Snapshot all open buffers so read_file_at_path can read
        // in-memory content instead of potentially stale disk files.
        let mut open_buffers = std::collections::HashMap::new();
        let mut open_buffer_revisions = std::collections::HashMap::new();
        let mut open_buffer_states = Vec::with_capacity(self.buffers.len());
        for (index, b) in self.buffers.iter().enumerate() {
            open_buffer_states.push(OpenBufferState {
                buffer_id: b.id(),
                path: b
                    .file_path()
                    .or_else(|| b.display_name())
                    .unwrap_or("[No Name]")
                    .to_string(),
                unnamed: b.file_path().is_none(),
                modified: b.is_modified(),
                revision: b.version(),
                visible: index == visible_index,
                chat_target: index == target_index,
            });
            if let Some(p) = b.file_path() {
                let path = std::path::Path::new(p);
                let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                open_buffers.insert(key, b.rope().to_string());
                open_buffer_revisions.insert(
                    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
                    b.version(),
                );
            }
        }
        let approved_path_roots = self
            .ai_state
            .chat
            .as_ref()
            .map(|c| c.approved_external_roots.clone())
            .unwrap_or_default();
        let lsp_manager = self.lsp_manager();
        let mut lsp_languages = lsp_manager
            .as_ref()
            .map(|manager| manager.active_server_languages())
            .unwrap_or_default();
        lsp_languages.sort();

        ToolExecutionContext {
            visible_buffer_content,
            visible_file_path,
            visible_buffer_revision,
            visible_cursor,
            visible_diagnostics,
            visible_current_file,
            buffer_content,
            file_path,
            buffer_revision,
            cursor,
            selection,
            diagnostics,
            project_diagnostics,
            scope_context: ScopeContext {
                current_file,
                project_root,
            },
            capabilities: self.build_chat_capabilities(),
            approved_path_roots,
            bypass_path_approvals: self.ai_chat_yolo_mode(),
            open_buffers,
            open_buffer_revisions,
            open_buffer_states,
            lsp_enabled: lsp_manager.is_some(),
            lsp_languages,
            lsp_status: self.lsp_status().to_string(),
        }
    }

    pub(super) fn active_chat_target_buffer_index(&self) -> usize {
        let current = self.current_buffer_index;
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| chat.active_buffer_id)
            .and_then(|buffer_id| self.find_buffer_index_by_id(buffer_id))
            .unwrap_or(current)
    }

    fn active_chat_target_buffer_index_strict(&self) -> std::result::Result<usize, String> {
        let Some(chat) = self.ai_state.chat.as_ref() else {
            return Ok(self.current_buffer_index);
        };
        self.find_buffer_index_by_id(chat.active_buffer_id).ok_or_else(|| {
            format!(
                "Active chat target is no longer available (buffer id {}). Re-open the target file with open_file before continuing.",
                chat.active_buffer_id
            )
        })
    }

    pub(super) fn active_chat_target_has_file_path(&self) -> bool {
        let Ok(target_index) = self.active_chat_target_buffer_index_strict() else {
            return false;
        };
        self.buffers
            .get(target_index)
            .and_then(|b| b.file_path())
            .is_some()
    }

    pub(super) fn no_file_open_guidance(&self) -> String {
        "No file open. Open or select a file first, then retry. Tip: use open_file(path, create=true) if you know the target path.".to_string()
    }

    fn active_chat_provider(&self) -> crate::ai::AiProviderKind {
        let profile_name = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.clone())
            .unwrap_or_else(|| self.ai_state.active_profile.clone());
        self.ai_state
            .config
            .resolve_profile(&profile_name)
            .map(|p| p.provider)
            .unwrap_or(crate::ai::AiProviderKind::Ollama)
    }

    fn active_chat_provider_is_remote(&self) -> bool {
        self.active_chat_provider() != crate::ai::AiProviderKind::Ollama
    }

    fn active_chat_tool_approval_mode(&self) -> ToolApprovalMode {
        self.ai_state.config.tool_approval_mode
    }

    pub(super) fn active_chat_target_absolute_path(&self) -> Option<PathBuf> {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|c| self.get_buffer_by_id(c.active_buffer_id))
            .and_then(|b| b.file_path())
            .map(PathBuf::from)
            .map(|p| self.absolutize_path(&p))
            .or_else(|| {
                self.buffer()
                    .file_path()
                    .map(PathBuf::from)
                    .map(|p| self.absolutize_path(&p))
            })
    }

    fn is_active_chat_target_path(&self, path: &Path) -> bool {
        let requested = normalize_path(path);
        self.active_chat_target_absolute_path()
            .map(|target| normalize_path(&target) == requested)
            .unwrap_or(false)
    }

    pub(super) fn maybe_require_tool_policy_approval(
        &self,
        tc: &ToolCallInfo,
        requested_path: Option<PathBuf>,
        is_project_scan: bool,
        approved_once_root: Option<&PathBuf>,
    ) -> Option<ToolApprovalRequest> {
        self.maybe_require_tool_policy_approval_with_original_target(
            tc,
            requested_path,
            is_project_scan,
            approved_once_root,
            None,
        )
    }

    pub(super) fn maybe_require_tool_policy_approval_with_original_target(
        &self,
        tc: &ToolCallInfo,
        requested_path: Option<PathBuf>,
        is_project_scan: bool,
        approved_once_root: Option<&PathBuf>,
        original_active_target: Option<&Path>,
    ) -> Option<ToolApprovalRequest> {
        if self.ai_chat_yolo_mode() {
            return None;
        }
        let mode = self.active_chat_tool_approval_mode();
        if mode != ToolApprovalMode::AlwaysPrompt
            && (requested_path
                .as_deref()
                .is_some_and(|path| self.current_session_created_temp_file(path))
                || (tc.name == "bash"
                    && tc
                        .arguments
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|command| {
                            self.current_session_authorizes_temp_shell_command(command)
                        })))
        {
            return None;
        }
        if mode == ToolApprovalMode::Auto {
            return None;
        }

        let tool_def = self.ai_state.tool_registry.get(&tc.name)?;
        let is_mutation = tool_def.side_effect == SideEffect::Mutation;
        let is_external = tool_def.side_effect == SideEffect::External;
        let is_sensitive = requested_path
            .as_ref()
            .and_then(|p| sensitive_path_reason(p))
            .is_some();
        let is_current_target = requested_path.as_ref().is_some_and(|p| {
            if let Some(orig) = original_active_target {
                normalize_path(p) == normalize_path(orig)
            } else {
                self.is_active_chat_target_path(p)
            }
        });

        let requires = match mode {
            ToolApprovalMode::Auto => false,
            ToolApprovalMode::SensitivePrompt => {
                is_sensitive || is_external || (is_mutation && !is_current_target)
            }
            ToolApprovalMode::AlwaysPrompt => true,
        };
        if !requires {
            return None;
        }

        if mode != ToolApprovalMode::AlwaysPrompt {
            if let Some(path) = requested_path.as_ref() {
                if let Some(root) = approved_once_root {
                    let root = canonicalize_or_normalize(root);
                    if canonicalize_or_normalize(path).starts_with(&root) {
                        return None;
                    }
                }
                if self.current_session_approved_root_for(path).is_some() {
                    return None;
                }
            }
        }

        let requested_path = requested_path
            .or_else(|| self.ai_effective_project_root())
            .unwrap_or_else(|| PathBuf::from("."));
        let approval_root = if requested_path.is_dir() {
            requested_path.clone()
        } else {
            requested_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| requested_path.clone())
        };
        let reason = if mode == ToolApprovalMode::AlwaysPrompt {
            "policy requires explicit approval"
        } else if is_external {
            "shell command execution requires approval"
        } else if is_mutation {
            "mutation tools require approval"
        } else if is_project_scan {
            "project-wide read requires approval"
        } else if is_sensitive {
            "sensitive path requires approval"
        } else {
            "approval required"
        };

        Some(ToolApprovalRequest {
            requested_path: requested_path.clone(),
            approval_root,
            reason: reason.to_string(),
            message: format!(
                "Approval required: {} ({}) for {}. Press Ctrl-Y to allow once, Ctrl-A to allow for this chat session, Ctrl-N to deny.",
                tc.name,
                reason,
                requested_path.display()
            ),
        })
    }

    fn remember_tool_approval(&mut self, tool_call: &ToolCallInfo, approval_root: &Path) {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return;
        };
        if tool_call.name == "read_buffer" {
            if let Some(buffer_id) = tool_call
                .arguments
                .get("buffer_id")
                .and_then(|value| value.as_u64())
            {
                chat.approved_unnamed_buffers.insert(buffer_id);
            }
            return;
        }

        let root = normalize_path(approval_root);
        if !chat
            .approved_external_roots
            .iter()
            .any(|path| normalize_path(path) == root)
        {
            chat.approved_external_roots.push(root);
        }
    }

    fn execute_read_buffer_tool(
        &mut self,
        tc: &ToolCallInfo,
        approved_once_token: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let Some(buffer_id) = tc
            .arguments
            .get("buffer_id")
            .and_then(|value| value.as_u64())
            .filter(|buffer_id| *buffer_id > 0)
        else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'buffer_id' parameter is required and must be a positive integer".to_string(),
            ));
        };
        let Some(buffer) = self.get_buffer_by_id(buffer_id) else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(format!(
                "buffer {buffer_id} is no longer open; call workspace_context for current buffer IDs"
            )));
        };
        if buffer.file_path().is_some() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(format!(
                "buffer {buffer_id} has a file path; use read_file_at_path so path safety rules apply"
            )));
        }

        let visible = self.buffer().id() == buffer_id;
        let chat_target = self
            .ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.active_buffer_id == buffer_id);
        let session_approved = self
            .ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.approved_unnamed_buffers.contains(&buffer_id));
        let approval_token = PathBuf::from(format!("unnamed-buffer:{buffer_id}"));
        let approved_once = approved_once_token == Some(&approval_token);

        if !visible
            && !chat_target
            && !session_approved
            && !approved_once
            && !self.ai_chat_yolo_mode()
        {
            let message = format!(
                "Allow this chat to read unnamed buffer {buffer_id}? Ovim cannot determine whether a pathless buffer contains sensitive information. Press Ctrl-Y to allow once, Ctrl-A to allow for this chat session, Ctrl-N to deny."
            );
            return ToolDispatchOutcome::ApprovalRequired(ToolApprovalRequest {
                requested_path: approval_token.clone(),
                approval_root: approval_token,
                reason: "unnamed buffer content requires explicit approval".to_string(),
                message,
            });
        }

        // Resolve content only after the approval check. This keeps unapproved
        // pathless text out of the generic tool execution snapshot.
        let buffer = self
            .get_buffer_by_id(buffer_id)
            .expect("buffer existence checked before approval");
        let content = buffer.rope().to_string();
        let revision = buffer.version();
        let label = buffer.display_name().unwrap_or("[No Name]");
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return ToolDispatchOutcome::Completed(ToolResult::Success(format!(
                "[empty buffer] Buffer {buffer_id} has no content.\nBuffer revision: {revision}"
            )));
        }

        let total = lines.len();
        let start = tc
            .arguments
            .get("start_line")
            .and_then(|value| value.as_u64())
            .map(|line| line.saturating_sub(1) as usize)
            .unwrap_or(0)
            .min(total);
        let end = tc
            .arguments
            .get("end_line")
            .and_then(|value| value.as_u64())
            .map(|line| line as usize)
            .unwrap_or(total)
            .min(total);
        if start >= end {
            return ToolDispatchOutcome::Completed(ToolResult::Success(
                "[empty range]".to_string(),
            ));
        }

        let mut output = format!(
            "Buffer: {label} (id {buffer_id}, lines {}-{} of {total})\nBuffer revision: {revision}\n",
            start + 1,
            end,
        );
        for (offset, line) in lines[start..end].iter().enumerate() {
            output.push_str(&format!("{:>4} | {}\n", start + offset + 1, line));
        }
        ToolDispatchOutcome::Completed(ToolResult::Success(output))
    }

    /// Dispatch a single tool call by side effect. Read tools get a snapshot,
    /// mutation tools get `&mut self`.
    ///
    /// `approved_once_root` temporarily allows one outside-project access for the call.
    pub(super) fn dispatch_tool_call_with_approval(
        &mut self,
        tc: &ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        if self.ai_code_explanation_answering()
            && self
                .ai_state
                .tool_registry
                .get(&tc.name)
                .is_none_or(|tool| tool.side_effect != SideEffect::Read)
        {
            return ToolDispatchOutcome::Completed(ToolResult::Error(format!(
                "tool '{}' is unavailable while answering a walkthrough question; use read-only investigation",
                tc.name
            )));
        }
        if self.is_ai_subagent_control_tool(&tc.name) {
            return ToolDispatchOutcome::Completed(self.execute_ai_subagent_control_tool(tc));
        }
        if tc.name == ACTIVATE_SKILL_TOOL {
            return ToolDispatchOutcome::Completed(self.execute_activate_skill_tool(&tc.arguments));
        }
        if tc.name == super::ai_compaction::COMPACT_TOOL {
            return ToolDispatchOutcome::Completed(self.execute_compact_tool(&tc.arguments));
        }
        if tc.name == "read_buffer" {
            let caps = self.build_chat_capabilities();
            let authorized = self
                .ai_state
                .config
                .profiles
                .get(&self.ai_state.active_profile)
                .is_some_and(|profile| {
                    self.ai_state
                        .tool_registry
                        .tools_for_profile(profile, &caps)
                        .iter()
                        .any(|tool| tool.name == "read_buffer")
                });
            if !authorized {
                return ToolDispatchOutcome::Completed(ToolResult::Error(
                    "tool 'read_buffer' is unavailable for the active profile or scope".to_string(),
                ));
            }
            return self.execute_read_buffer_tool(tc, approved_once_root);
        }
        if tc.name == super::ai_comprehension::RECORD_COMPREHENSION_CHECKPOINT_TOOL {
            return ToolDispatchOutcome::Completed(
                self.execute_record_comprehension_checkpoint(&tc.arguments),
            );
        }
        let has_explicit_path = tc
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        let implicit_mutation = !has_explicit_path
            && self
                .ai_state
                .tool_registry
                .get(&tc.name)
                .is_some_and(|tool| tool.side_effect != SideEffect::Read);
        if implicit_mutation {
            if let Err(err) = self.active_chat_target_buffer_index_strict() {
                return ToolDispatchOutcome::Completed(ToolResult::Error(err));
            }
        }
        let path_scoped_without_open_file = has_explicit_path
            && matches!(
                tc.name.as_str(),
                "read_file_at_path"
                    | "read_diagnostics"
                    | "view_image"
                    | "list_files"
                    | "edit_range"
                    | "insert_lines"
                    | "delete_lines"
                    | "write_file_at_path"
                    | "create_file"
                    | "apply_patch_at_path"
                    | "snapshot_file"
                    | "restore_file"
            );
        let project_scoped_without_open_file = matches!(
            tc.name.as_str(),
            "list_files"
                | "search_project"
                | "workspace_context"
                | "gdiff_review"
                | "gdiff_comment"
                | "strok_vector"
        );
        let visible_buffer_read = matches!(
            tc.name.as_str(),
            "read_file"
                | "read_selection"
                | "read_diagnostics"
                | "document_symbols"
                | "hover"
                | "goto_definition"
        ) && self.buffer().file_path().is_some();

        if !self.active_chat_target_has_file_path()
            && tc.name != "open_file"
            && tc.name != "bash"
            && tc.name != "web_search"
            && tc.name != "web_fetch"
            && !path_scoped_without_open_file
            && !project_scoped_without_open_file
            && !visible_buffer_read
        {
            return ToolDispatchOutcome::Completed(ToolResult::Error(self.no_file_open_guidance()));
        }

        if tc.name == "read_file_at_path" {
            return self.execute_read_file_at_path_tool(tc, approved_once_root);
        }
        if tc.name == "view_image" {
            return self.execute_view_image_tool(tc, approved_once_root);
        }
        if tc.name == "list_files" {
            return self.execute_list_files_tool(tc, approved_once_root);
        }
        if tc.name == "open_file" {
            return self.execute_open_file_tool(tc, approved_once_root);
        }
        if matches!(
            tc.name.as_str(),
            "edit_range"
                | "insert_lines"
                | "delete_lines"
                | "write_file_at_path"
                | "create_file"
                | "apply_patch_at_path"
                | "snapshot_file"
                | "restore_file"
        ) {
            return self.execute_path_scoped_mutation_tool(tc, approved_once_root);
        }

        let generic_requested_path = self.active_chat_target_absolute_path();
        let generic_project_scan = tc.name == "read_project_diagnostics";
        if let Some(req) = self.maybe_require_tool_policy_approval(
            tc,
            generic_requested_path,
            generic_project_scan,
            approved_once_root,
        ) {
            return ToolDispatchOutcome::ApprovalRequired(req);
        }

        let result = match self
            .ai_state
            .tool_registry
            .get(&tc.name)
            .map(|t| t.side_effect)
        {
            Some(SideEffect::Read) => match tc.name.as_str() {
                "web_search" | "web_fetch" => {
                    if !self.ai_chat_uses_direct_codex() {
                        return ToolDispatchOutcome::Completed(ToolResult::Error(
                            "Exa web tools are available only with the direct Codex/Ovim harness"
                                .to_string(),
                        ));
                    }
                    let outcome = crate::ai::exa::execute(&tc.name, &tc.arguments);
                    if outcome.credential_rejected {
                        self.note_exa_credential_rejected(outcome.environment_override);
                    } else if let Some(error) = outcome.setup_error.clone() {
                        self.open_exa_setup_dialog(Some(error));
                    }
                    outcome.result
                }
                "document_symbols" | "hover" | "goto_definition" => {
                    self.execute_lsp_tool(&tc.name, &tc.arguments)
                }
                _ => {
                    let ctx = self.build_tool_execution_context();
                    self.execute_tool_call(tc, &ctx)
                }
            },
            Some(SideEffect::Navigation) => self.execute_navigation_tool(&tc.name, &tc.arguments),
            Some(SideEffect::Mutation) => self.execute_mutation_tool(&tc.name, &tc.arguments),
            Some(SideEffect::External) => self.execute_external_tool(&tc.name, &tc.arguments),
            None => ToolResult::Error(format!("unknown tool: {}", tc.name)),
        };
        ToolDispatchOutcome::Completed(result)
    }

    pub(super) fn execute_view_image_tool(
        &mut self,
        tc: &ToolCallInfo,
        approved_once_root: Option<&PathBuf>,
    ) -> ToolDispatchOutcome {
        let Some(raw_path) = tc.arguments.get("path").and_then(|value| value.as_str()) else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' parameter is required and must be non-empty".to_string(),
            ));
        };
        if raw_path.trim().is_empty() {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "'path' parameter is required and must be non-empty".to_string(),
            ));
        }
        let Some(tool) = self.ai_state.tool_registry.get("view_image") else {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "unknown tool: view_image".to_string(),
            ));
        };
        if !self
            .build_chat_capabilities()
            .contains(&tool.required_scope)
        {
            return ToolDispatchOutcome::Completed(ToolResult::Error(
                "tool 'view_image' requires project file scope".to_string(),
            ));
        }

        let resolution = match self.resolve_tool_path_policy(
            raw_path,
            false,
            "view_image",
            approved_once_root,
        ) {
            Ok(resolution) => resolution,
            Err(error) => return ToolDispatchOutcome::Completed(ToolResult::Error(error)),
        };
        let absolute_path = match resolution {
            ToolPathResolution::Allowed { absolute_path, .. } => absolute_path,
            ToolPathResolution::NeedsApproval(request) => {
                return ToolDispatchOutcome::ApprovalRequired(request)
            }
        };
        if let Some(request) = self.maybe_require_tool_policy_approval(
            tc,
            Some(absolute_path.clone()),
            false,
            approved_once_root,
        ) {
            return ToolDispatchOutcome::ApprovalRequired(request);
        }

        match super::ai_chat_images::load_image(absolute_path) {
            Ok(image) => {
                let label = image.file_name();
                if !tc.id.is_empty() {
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.tool_result_images.insert(tc.id.clone(), vec![image]);
                    }
                }
                ToolDispatchOutcome::Completed(ToolResult::Success(format!(
                    "Image attached for visual inspection: {label}"
                )))
            }
            Err(error) => ToolDispatchOutcome::Completed(ToolResult::Error(error.to_string())),
        }
    }

    /// Execute tool calls from a completed stream response, record results,
    /// and continue the conversation. Returns true to signal state changed.
    pub(crate) fn process_tool_calls(
        &mut self,
        tool_calls: Vec<ToolCallInfo>,
        content: String,
        provider_state: Vec<serde_json::Value>,
        model_name: &str,
    ) -> bool {
        let used = self
            .ai_state
            .chat
            .as_ref()
            .map(|c| c.tool_call_count)
            .unwrap_or(0);
        let max_tool_calls = self.ai_chat_tool_call_limit();

        if max_tool_calls.is_some_and(|limit| used >= limit) {
            // Hit limit — commit what we have and stop
            if !content.is_empty() {
                if let Some(conv) = self.conversation_mut() {
                    conv.append_assistant_message(content, model_name.to_string());
                }
            }
            if let Some(conv) = self.conversation_mut() {
                conv.append_error("Tool call iteration limit reached.".to_string());
            }
            self.ai_runtime_fail_turn("tool call iteration limit reached");
            self.clear_streaming_state();
            return true;
        }

        // Set up undo group for this tool call batch
        if let Some(chat) = self.ai_state.chat.as_mut() {
            if chat.current_undo_group.is_none() {
                let gid = chat.next_undo_group_id;
                chat.next_undo_group_id += 1;
                chat.current_undo_group = Some(gid);
            }
        }

        // 1. Commit content + tool_calls as assistant message
        let event_id = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.runtime_last_content_event.clone());
        let node_id = self.conversation_mut().map(|conv| {
            conv.append_assistant_message_with_tools_and_state(
                content,
                model_name.to_string(),
                tool_calls.clone(),
                provider_state,
            )
        });
        if let (Some(node_id), Some(event_id)) = (node_id, event_id) {
            self.record_ai_chat_node(node_id, event_id);
        }

        // 2. Execute tools. May pause for user approval.
        self.execute_tool_call_batch(tool_calls, model_name.to_string())
    }

    /// Resolve a paused outside-project tool request.
    pub fn ai_chat_resolve_pending_tool_approval(&mut self, allow: bool, remember: bool) -> bool {
        let pending = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|c| c.pending_tool_approval.take());

        let Some(pending) = pending else {
            return false;
        };

        if let Some(response) = pending.dynamic_response {
            let Some(turn) = pending.dynamic_turn else {
                let _ = response.send(Err("dynamic approval lost its runtime turn".into()));
                return true;
            };
            let Some(tool) = pending.runtime_tool else {
                let _ = response.send(Err("dynamic approval lost its runtime tool".into()));
                return true;
            };
            if allow {
                let tool_name = pending.tool_call.name.clone();
                if remember {
                    self.remember_tool_approval(&pending.tool_call, &pending.approval_root);
                }
                self.execute_dynamic_tool_after_policy(
                    turn,
                    tool,
                    pending.tool_call,
                    response,
                    Some(pending.approval_root),
                    pending.runtime_tool_started,
                );
                self.set_status_message(format!("Approved {tool_name} for this invocation"));
            } else {
                let tool_name = pending.tool_call.name.clone();
                self.finish_dynamic_tool(
                    &turn,
                    &tool,
                    &pending.tool_call,
                    response,
                    ToolResult::Error(format!("user denied {tool_name}")),
                );
                self.set_status_message(format!("Denied {tool_name}"));
            }
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.waiting = true;
            }
            return true;
        }

        if !allow {
            let denied_result = ToolResult::Error(format!(
                "user denied outside-project access for '{}'",
                pending.requested_path.display()
            ));
            if let (Some(turn), Some(runtime_tool)) =
                (self.active_ai_runtime_turn(), pending.runtime_tool.as_ref())
            {
                if let Err(error) = self.ai_runtime_finish_tool(&turn, runtime_tool, &denied_result)
                {
                    crate::log_warn!("agent_runtime", "failed to record denied tool: {error}");
                }
            }
            self.record_tool_event_summary(&pending.tool_call, &denied_result);
            let result_content = self.format_tool_result_with_target(
                &pending.tool_call,
                &ToolResult::Error(format!(
                    "user denied outside-project access for '{}'",
                    pending.requested_path.display()
                )),
            );
            if let Some(conv) = self.conversation_mut() {
                conv.append_tool_result(pending.tool_call.id.clone(), result_content);
            }
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                chat.waiting = true;
            }
            self.set_status_message("Denied outside-project tool access".to_string());
            return self.execute_tool_call_batch(pending.remaining_tool_calls, pending.model_name);
        }

        if remember {
            self.remember_tool_approval(&pending.tool_call, &pending.approval_root);
        }

        if pending.tool_call.name == "bash" {
            // Approved batch shell must run through the same asynchronous,
            // workspace-captured parking path as unprompted batch shell calls.
            // Dispatching synchronously would block the editor thread and
            // escape the run log entirely.
            return self.resume_approved_batch_shell(pending);
        }

        let outcome =
            self.dispatch_tool_call_with_approval(&pending.tool_call, Some(&pending.approval_root));
        match outcome {
            ToolDispatchOutcome::Completed(result) => {
                if let (Some(turn), Some(runtime_tool)) =
                    (self.active_ai_runtime_turn(), pending.runtime_tool.as_ref())
                {
                    if let Err(error) = self.ai_runtime_finish_tool(&turn, runtime_tool, &result) {
                        crate::log_warn!(
                            "agent_runtime",
                            "failed to record approved tool: {error}"
                        );
                    }
                }
                self.record_tool_event_summary(&pending.tool_call, &result);
                let result_content =
                    self.format_tool_result_with_target(&pending.tool_call, &result);
                if let Some(conv) = self.conversation_mut() {
                    conv.append_tool_result(pending.tool_call.id.clone(), result_content);
                }
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                    chat.waiting = true;
                }
                self.set_status_message(format!(
                    "Approved outside-project access: {}",
                    pending.requested_path.display()
                ));
                self.execute_tool_call_batch(pending.remaining_tool_calls, pending.model_name)
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                self.pause_for_tool_approval(PendingToolApproval {
                    tool_call: pending.tool_call,
                    reason: req.reason,
                    runtime_tool: pending.runtime_tool,
                    runtime_tool_started: pending.runtime_tool_started,
                    remaining_tool_calls: pending.remaining_tool_calls,
                    model_name: pending.model_name,
                    requested_path: req.requested_path.clone(),
                    approval_root: req.approval_root.clone(),
                    dynamic_response: None,
                    dynamic_turn: None,
                });
                self.set_status_message(req.message);
                true
            }
        }
    }

    /// Resume an approved batch `bash` tool call by parking it on
    /// `pending_shell_execution`, exactly like the unprompted batch path in
    /// `execute_tool_call_batch`. The user already approved, so no policy
    /// re-check happens here.
    fn resume_approved_batch_shell(&mut self, pending: PendingToolApproval) -> bool {
        let authorized = self
            .ai_state
            .tool_registry
            .get(&pending.tool_call.name)
            .is_some_and(|definition| {
                let capabilities = self.build_chat_capabilities();
                capabilities.allows_side_effect(definition.side_effect)
                    && capabilities.contains(&definition.required_scope)
            });
        let command = pending
            .tool_call
            .arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let workdir = self.ai_effective_project_root();
        let runtime_turn = self.active_ai_runtime_turn();
        let artifact_store = runtime_turn.as_ref().and_then(|turn| {
            self.ai_state.durable_runs.as_ref().and_then(|services| {
                services
                    .store
                    .layout()
                    .ensure_run_directory(&turn.run_id)
                    .ok()?;
                crate::run_log::ArtifactStore::open(
                    services.store.layout().artifact_directory(&turn.run_id),
                )
                .ok()
            })
        });

        let error = if !authorized {
            Some("shell access is not authorized for this chat".to_string())
        } else if command.is_empty() {
            Some("'command' is required and must be non-empty".to_string())
        } else if let Some(reason) = self.comprehension_gate_for_bash(&command) {
            Some(reason)
        } else if workdir.is_none() {
            Some(self.no_project_root_error())
        } else if artifact_store.is_none() {
            Some(
                "shell program was not executed because replay artifact storage is unavailable"
                    .to_string(),
            )
        } else {
            None
        };
        if let Some(error) = error {
            let result = ToolResult::Error(error);
            if let (Some(turn), Some(runtime_tool)) =
                (runtime_turn.as_ref(), pending.runtime_tool.as_ref())
            {
                if let Err(error) = self.ai_runtime_finish_tool(turn, runtime_tool, &result) {
                    crate::log_warn!("agent_runtime", "failed to record approved tool: {error}");
                }
            }
            self.record_tool_event_summary(&pending.tool_call, &result);
            let result_content = self.format_tool_result_with_target(&pending.tool_call, &result);
            if let Some(conv) = self.conversation_mut() {
                conv.append_tool_result(pending.tool_call.id.clone(), result_content);
            }
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.tool_call_count = chat.tool_call_count.saturating_add(1);
                chat.waiting = true;
            }
            return self.execute_tool_call_batch(pending.remaining_tool_calls, pending.model_name);
        }

        self.start_pending_shell_execution(
            pending.tool_call,
            super::ai_chat_state::ToolExecutionContinuation::Batch {
                runtime_tool: pending.runtime_tool,
                runtime_turn,
                remaining_tool_calls: pending.remaining_tool_calls,
                model_name: pending.model_name,
            },
            command,
            workdir.expect("checked above"),
            artifact_store.expect("checked above"),
        );
        true
    }

    /// On first chat open in a no-repo session, ask once whether project tools
    /// may access the current folder as the project boundary.
    pub(crate) fn maybe_prompt_no_repo_session_folder_access_on_chat_open(&mut self) {
        if self.ai_repo_root().is_some() || self.ai_state.no_repo_session_prompted {
            return;
        }
        let Some(folder) = self.ai_no_repo_candidate_root() else {
            return;
        };

        self.ai_state.no_repo_session_prompted = true;
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.pending_no_repo_folder_approval = Some(folder.clone());
        }
        self.set_status_message(format!(
            "You're not in a git repo. Allow AI tool access to folder: {}? Press Ctrl-Y to allow, Ctrl-N to deny.",
            folder.display()
        ));
    }

    /// Resolve the first-chat-open no-repo folder access prompt.
    pub fn ai_chat_resolve_pending_no_repo_folder_approval(&mut self, allow: bool) -> bool {
        let pending_folder = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|c| c.pending_no_repo_folder_approval.take());

        let Some(folder) = pending_folder else {
            return false;
        };

        self.ai_state.no_repo_session_prompted = true;
        if allow {
            let root = normalize_path(&folder);
            self.ai_state.no_repo_session_allowed_root = Some(root.clone());
            let durable_key = self
                .ai_state
                .chat
                .as_ref()
                .map(|chat| (chat.origin_buffer_id, chat.opts.name.clone()));
            let durable = if let Some((buffer_id, name)) = durable_key.as_ref() {
                self.prepare_durable_ai_chat(*buffer_id, name)
            } else {
                Ok(())
            };
            match durable {
                Ok(()) => {
                    if let Some(key) = durable_key {
                        let runtime_branch = self
                            .ai_state
                            .durable_chat_bindings
                            .get(&key)
                            .and_then(|binding| {
                                self.ai_state
                                    .agent_runtime
                                    .selected_branch(&binding.locator)
                                    .map(|(locator, _)| locator.clone())
                            });
                        if let (Some(chat), Some(runtime_branch)) =
                            (self.ai_state.chat.as_mut(), runtime_branch)
                        {
                            chat.runtime_branch = runtime_branch;
                        }
                    }
                    self.set_status_message(format!(
                        "Approved durable AI tool access for folder: {}",
                        root.display()
                    ));
                }
                Err(error) => self.set_status_message(format!(
                    "Folder approved, but durable agent edits remain disabled: {error}. Check Ovim's run-storage permissions and reopen the chat."
                )),
            }
        } else {
            self.ai_state.no_repo_session_allowed_root = None;
            self.set_status_message("Denied no-repo folder tool access".to_string());
        }
        true
    }

    pub(super) fn execute_tool_call_batch(
        &mut self,
        tool_calls: Vec<ToolCallInfo>,
        model_name: String,
    ) -> bool {
        let max_tool_calls = self.ai_chat_tool_call_limit();

        let mut executed_in_batch: u64 = 0;

        for (idx, tc) in tool_calls.iter().enumerate() {
            let used = self
                .ai_state
                .chat
                .as_ref()
                .map(|c| c.tool_call_count)
                .unwrap_or(0);
            if max_tool_calls.is_some_and(|limit| used.saturating_add(executed_in_batch) >= limit) {
                self.append_synthetic_tool_results(&tool_calls[idx..], "Tool call limit reached");
                if let Some(conv) = self.conversation_mut() {
                    conv.append_error("Tool call iteration limit reached.".to_string());
                }
                self.ai_runtime_fail_turn("tool call iteration limit reached");
                self.clear_streaming_state();
                return true;
            }

            let runtime_tool = match self.active_ai_runtime_turn() {
                Some(turn) => match self.ai_runtime_record_tool_intent(&turn, tc) {
                    Ok(tool) => {
                        if let Err(error) = self.ai_runtime_start_tool(&turn, &tool) {
                            self.ai_runtime_fail_turn(format!(
                                "failed to record tool start: {error}"
                            ));
                            self.clear_streaming_state();
                            return true;
                        }
                        Some((turn, tool))
                    }
                    Err(error) => {
                        self.ai_runtime_fail_turn(format!("failed to record tool intent: {error}"));
                        self.clear_streaming_state();
                        return true;
                    }
                },
                None => None,
            };

            let subagent_control_outcome = if matches!(
                tc.name.as_str(),
                crate::ai::tools::subagents::WAIT_AGENT_TOOL
                    | crate::ai::tools::subagents::INTERRUPT_AGENT_TOOL
                    | crate::ai::tools::subagents::FOLLOWUP_AGENT_TOOL
            ) {
                let continuation = super::ai_chat_state::ToolExecutionContinuation::Batch {
                    runtime_tool: runtime_tool.as_ref().map(|(_, tool)| tool.clone()),
                    runtime_turn: runtime_tool.as_ref().map(|(turn, _)| turn.clone()),
                    remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                    model_name: model_name.clone(),
                };
                match self.begin_pending_ai_subagent_control(tc.clone(), continuation) {
                    Ok(()) => {
                        if let Some(chat) = self.ai_state.chat.as_mut() {
                            chat.tool_call_count =
                                chat.tool_call_count.saturating_add(executed_in_batch);
                        }
                        return true;
                    }
                    Err((result, _continuation)) => Some(ToolDispatchOutcome::Completed(result)),
                }
            } else {
                None
            };

            let shell_outcome = if tc.name == "bash" {
                if let Some(request) = self.maybe_require_tool_policy_approval(
                    tc,
                    self.active_chat_target_absolute_path(),
                    false,
                    None,
                ) {
                    Some(ToolDispatchOutcome::ApprovalRequired(request))
                } else {
                    let authorized =
                        self.ai_state
                            .tool_registry
                            .get(&tc.name)
                            .is_some_and(|definition| {
                                let capabilities = self.build_chat_capabilities();
                                capabilities.allows_side_effect(definition.side_effect)
                                    && capabilities.contains(&definition.required_scope)
                            });
                    let command = tc
                        .arguments
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    let workdir = self.ai_effective_project_root();
                    let artifact_store = runtime_tool.as_ref().and_then(|(turn, _)| {
                        self.ai_state.durable_runs.as_ref().and_then(|services| {
                            services
                                .store
                                .layout()
                                .ensure_run_directory(&turn.run_id)
                                .ok()?;
                            crate::run_log::ArtifactStore::open(
                                services.store.layout().artifact_directory(&turn.run_id),
                            )
                            .ok()
                        })
                    });

                    if !authorized {
                        Some(ToolDispatchOutcome::Completed(ToolResult::Error(
                            "shell access is not authorized for this chat".into(),
                        )))
                    } else if command.is_empty() {
                        Some(ToolDispatchOutcome::Completed(ToolResult::Error(
                            "'command' is required and must be non-empty".into(),
                        )))
                    } else if let Some(reason) = self.comprehension_gate_for_bash(&command) {
                        Some(ToolDispatchOutcome::Completed(ToolResult::Error(reason)))
                    } else {
                        match (workdir, artifact_store) {
                            (None, _) => Some(ToolDispatchOutcome::Completed(ToolResult::Error(
                                self.no_project_root_error(),
                            ))),
                            (_, None) => Some(ToolDispatchOutcome::Completed(ToolResult::Error(
                                "shell program was not executed because replay artifact storage is unavailable"
                                    .into(),
                            ))),
                            (Some(workdir), Some(artifact_store)) => {
                                if let Some(chat) = self.ai_state.chat.as_mut() {
                                    chat.tool_call_count =
                                        chat.tool_call_count.saturating_add(executed_in_batch);
                                }
                                self.start_pending_shell_execution(
                                    tc.clone(),
                                    super::ai_chat_state::ToolExecutionContinuation::Batch {
                                        runtime_tool: runtime_tool
                                            .as_ref()
                                            .map(|(_, tool)| tool.clone()),
                                        runtime_turn: runtime_tool
                                            .as_ref()
                                            .map(|(turn, _)| turn.clone()),
                                        remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                                        model_name,
                                    },
                                    command,
                                    workdir,
                                    artifact_store,
                                );
                                return true;
                            }
                        }
                    }
                }
            } else {
                None
            };

            let background_outcome = if self.is_background_ai_tool(&tc.name) {
                let continuation = super::ai_chat_state::ToolExecutionContinuation::Batch {
                    runtime_tool: runtime_tool.as_ref().map(|(_, tool)| tool.clone()),
                    runtime_turn: runtime_tool.as_ref().map(|(turn, _)| turn.clone()),
                    remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                    model_name: model_name.clone(),
                };
                match self.begin_pending_background_tool(tc.clone(), continuation) {
                    Ok(()) => {
                        if let Some(chat) = self.ai_state.chat.as_mut() {
                            chat.tool_call_count =
                                chat.tool_call_count.saturating_add(executed_in_batch);
                        }
                        return true;
                    }
                    Err((result, _continuation)) => Some(ToolDispatchOutcome::Completed(result)),
                }
            } else {
                None
            };

            let outcome = if let Some(outcome) = subagent_control_outcome {
                outcome
            } else if let Some(outcome) = shell_outcome {
                outcome
            } else if let Some(outcome) = background_outcome {
                outcome
            } else if tc.name == "explain_with_codebase" {
                let continuation = CodeExplanationContinuation::Batch {
                    runtime_tool: runtime_tool.as_ref().map(|(_, tool)| tool.clone()),
                    runtime_turn: runtime_tool.as_ref().map(|(turn, _)| turn.clone()),
                    remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                    model_name: model_name.clone(),
                };
                match self.begin_code_explanation(tc.clone(), continuation) {
                    Ok(()) => {
                        if let Some(chat) = self.ai_state.chat.as_mut() {
                            chat.tool_call_count =
                                chat.tool_call_count.saturating_add(executed_in_batch);
                        }
                        return true;
                    }
                    Err((result, _continuation)) => ToolDispatchOutcome::Completed(result),
                }
            } else {
                self.dispatch_tool_call_with_approval(tc, None)
            };

            match outcome {
                ToolDispatchOutcome::Completed(result) => {
                    if let Some((turn, tool)) = runtime_tool.as_ref() {
                        if let Err(error) = self.ai_runtime_finish_tool(turn, tool, &result) {
                            self.ai_runtime_fail_turn(format!(
                                "failed to record tool result: {error}"
                            ));
                            self.clear_streaming_state();
                            return true;
                        }
                    }
                    self.record_tool_event_summary(tc, &result);
                    let result_content = self.format_tool_result_with_target(tc, &result);
                    let images = self.take_tool_result_images(&tc.id);
                    if let Some(conv) = self.conversation_mut() {
                        conv.append_tool_result_with_images(tc.id.clone(), result_content, images);
                    }
                    executed_in_batch = executed_in_batch.saturating_add(1);
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    if let Some(chat) = self.ai_state.chat.as_mut() {
                        chat.tool_call_count =
                            chat.tool_call_count.saturating_add(executed_in_batch);
                    }
                    self.pause_for_tool_approval(PendingToolApproval {
                        tool_call: tc.clone(),
                        reason: req.reason,
                        runtime_tool: runtime_tool.map(|(_, tool)| tool),
                        runtime_tool_started: true,
                        remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                        model_name,
                        requested_path: req.requested_path,
                        approval_root: req.approval_root,
                        dynamic_response: None,
                        dynamic_turn: None,
                    });
                    self.set_status_message(req.message);
                    return true;
                }
            }
        }

        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.tool_call_count = chat.tool_call_count.saturating_add(executed_in_batch);
            chat.waiting = true;
        }

        if let Err(error) = self.apply_local_ai_chat_steers() {
            self.ai_runtime_fail_turn(format!("failed to apply queued steer: {error}"));
            if let Some(conv) = self.conversation_mut() {
                conv.append_error(format!("Failed to apply queued steer: {error}"));
            }
            self.clear_streaming_state();
            return true;
        }

        if let Err(e) = self.spawn_streaming_request() {
            self.ai_runtime_fail_turn(format!("failed to continue after tools: {e}"));
            if let Some(conv) = self.conversation_mut() {
                conv.append_error(format!("Failed to continue: {e}"));
            }
            if let Some(chat) = self.ai_state.chat.as_mut() {
                chat.waiting = false;
                chat.pending_job = None;
            }
        }

        true
    }

    fn pause_for_tool_approval(&mut self, pending: PendingToolApproval) {
        let mut installed = false;
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.pending_tool_approval = Some(pending);
            chat.waiting = false;
            chat.pending_job = None;
            chat.streaming_content = None;
            chat.streaming_thinking = None;
            installed = true;
        }
        if installed {
            self.ai_state.ai_attention_generation =
                self.ai_state.ai_attention_generation.saturating_add(1);
        }
    }

    /// Close out tool calls that will never execute (cancelled, unknown
    /// outcome, limit reached) with synthetic error results. An assistant
    /// message with `tool_use` blocks and no matching `tool_result` bricks
    /// the conversation on the next provider request.
    pub(super) fn append_synthetic_tool_results(
        &mut self,
        tool_calls: &[ToolCallInfo],
        detail: &str,
    ) {
        for tc in tool_calls {
            if tc.id.is_empty() {
                continue;
            }
            let result = ToolResult::Error(detail.to_string());
            self.record_tool_event_summary(tc, &result);
            let content = self.format_tool_result_with_target(tc, &result);
            if let Some(conv) = self.conversation_mut() {
                conv.append_tool_result(tc.id.clone(), content);
            }
        }
    }

    pub(super) fn record_tool_event_summary(&mut self, tc: &ToolCallInfo, result: &ToolResult) {
        if tc.id.is_empty() {
            return;
        }
        let summary = self.build_tool_event_summary(tc, result);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.tool_event_summaries.insert(tc.id.clone(), summary);
        }
    }

    fn take_tool_result_images(
        &mut self,
        tool_call_id: &str,
    ) -> Vec<crate::ai::chat_types::ImageAttachment> {
        self.ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.tool_result_images.remove(tool_call_id))
            .unwrap_or_default()
    }

    pub(super) fn format_tool_result_with_target(
        &self,
        tc: &ToolCallInfo,
        result: &ToolResult,
    ) -> String {
        let target = tc
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(compact_tool_path)
            .unwrap_or_else(|| self.active_chat_target_display_path());
        let raw_body = match result {
            ToolResult::Success(s) => s.as_str().to_string(),
            ToolResult::Error(s) => format!("Error: {s}"),
        };
        if tc.name == ACTIVATE_SKILL_TOOL {
            let body = if self.active_chat_provider_is_remote() {
                redact_high_risk_tokens(&raw_body)
            } else {
                raw_body
            };
            return truncate_utf8_with_notice(&body, 40 * 1024);
        }
        let body = if self.active_chat_provider_is_remote() {
            let redacted = redact_high_risk_tokens(&raw_body);
            truncate_utf8_with_notice(&redacted, 8 * 1024)
        } else {
            truncate_utf8_with_notice(&raw_body, 64 * 1024)
        };
        format!("Target: {target}\n{body}")
    }

    fn build_tool_event_summary(&self, tc: &ToolCallInfo, result: &ToolResult) -> ToolEventSummary {
        if let ToolResult::Error(err) = result {
            return ToolEventSummary {
                kind: ToolSummaryKind::Error,
                label: format!("{} {}", tc.name, compact_tool_label(err)),
                call: tc.clone(),
            };
        }

        let target_path = self.active_chat_target_display_path();
        let explicit_path = tc
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .map(compact_tool_path);
        let mutation_target = explicit_path.clone().unwrap_or_else(|| target_path.clone());

        let (kind, label) = match tc.name.as_str() {
            "activate_skill" => {
                let name = tc
                    .arguments
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                (ToolSummaryKind::Read, format!("skill {name}"))
            }
            "edit_range" => {
                let start = tc
                    .arguments
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let end = tc
                    .arguments
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(start as u64) as usize;
                let old_lines = end.saturating_sub(start).saturating_add(1);
                let new_lines = tc
                    .arguments
                    .get("new_text")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        if s.is_empty() {
                            0
                        } else {
                            s.lines().count().max(1)
                        }
                    })
                    .unwrap_or(0);
                let added = new_lines.saturating_sub(old_lines);
                let removed = old_lines.saturating_sub(new_lines);
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +{added} -{removed}"),
                )
            }
            "insert_lines" => {
                let added = tc
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        if s.is_empty() {
                            0
                        } else {
                            s.lines().count().max(1)
                        }
                    })
                    .unwrap_or(0);
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +{added} -0"),
                )
            }
            "delete_lines" => {
                let start = tc
                    .arguments
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let end = tc
                    .arguments
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(start as u64) as usize;
                let removed = end.saturating_sub(start).saturating_add(1);
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +0 -{removed}"),
                )
            }
            "write_file_at_path" => {
                let written = tc
                    .arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        if s.is_empty() {
                            0
                        } else {
                            s.lines().count().max(1)
                        }
                    })
                    .unwrap_or(0);
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +{written} -*"),
                )
            }
            "create_file" => {
                let written = tc
                    .arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        if s.is_empty() {
                            0
                        } else {
                            s.lines().count().max(1)
                        }
                    })
                    .unwrap_or(0);
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +{written} -0"),
                )
            }
            "apply_patch_at_path" => {
                let (added, removed) = tc
                    .arguments
                    .get("diff")
                    .and_then(|v| v.as_str())
                    .map(diff_line_deltas)
                    .unwrap_or((0, 0));
                (
                    ToolSummaryKind::Mutation,
                    format!("{mutation_target} +{added} -{removed}"),
                )
            }
            "open_file" => {
                let path = tc
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(compact_tool_path)
                    .unwrap_or_else(|| target_path.clone());
                let line = tc
                    .arguments
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                (ToolSummaryKind::Navigation, format!("{path}:{line}"))
            }
            "explain_with_codebase" => {
                let count = tc
                    .arguments
                    .get("steps")
                    .and_then(|steps| steps.as_array())
                    .map(Vec::len)
                    .unwrap_or(0);
                (
                    ToolSummaryKind::Navigation,
                    format!("walkthrough · {count} pages"),
                )
            }
            "select_text" => {
                let start = tc
                    .arguments
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let end = tc
                    .arguments
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(start);
                (
                    ToolSummaryKind::Navigation,
                    format!("{target_path}:{start}-{end}"),
                )
            }
            "read_file_at_path" => {
                let path = tc
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(compact_tool_path)
                    .unwrap_or_else(|| target_path.clone());
                let range = tool_line_range_suffix(&tc.arguments);
                (ToolSummaryKind::Read, format!("{path}{range}"))
            }
            "read_file" => {
                let range = tool_line_range_suffix(&tc.arguments);
                (ToolSummaryKind::Read, format!("{target_path}{range}"))
            }
            "list_files" => {
                let dir = tc
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(compact_tool_path)
                    .unwrap_or_else(|| ".".to_string());
                let count = tool_result_success(result)
                    .and_then(|s| first_number_in_text(s.lines().next().unwrap_or("")));
                let label = match count {
                    Some(n) => format!("{dir} {n} files"),
                    None => format!("{dir} files"),
                };
                (ToolSummaryKind::Search, label)
            }
            "search_project" => {
                let query = tc
                    .arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let count = tool_result_success(result)
                    .and_then(|s| first_number_in_text(s.lines().next().unwrap_or("")));
                let label = match count {
                    Some(n) => format!("\"{}\" {n} matches", compact_tool_label(query)),
                    None => format!("\"{}\" search", compact_tool_label(query)),
                };
                (ToolSummaryKind::Search, label)
            }
            "web_search" => {
                let query = tc
                    .arguments
                    .get("query")
                    .and_then(|value| value.as_str())
                    .unwrap_or("web");
                (ToolSummaryKind::Search, format!("Web: {query}"))
            }
            "web_fetch" => {
                let url = tc
                    .arguments
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("page");
                (ToolSummaryKind::Read, format!("Web page: {url}"))
            }
            "read_diagnostics" => {
                let success = tool_result_success(result).unwrap_or_default();
                if success.starts_with("No diagnostics.") {
                    (
                        ToolSummaryKind::Diagnostics,
                        "diagnostics E0 W0".to_string(),
                    )
                } else {
                    let errors = success.matches("[error]").count();
                    let warnings = success.matches("[warning]").count();
                    (
                        ToolSummaryKind::Diagnostics,
                        format!("diagnostics E{errors} W{warnings}"),
                    )
                }
            }
            "read_project_diagnostics" => {
                let success = tool_result_success(result).unwrap_or_default();
                let summary = success
                    .lines()
                    .next()
                    .unwrap_or("project diagnostics")
                    .to_string();
                (ToolSummaryKind::Diagnostics, summary)
            }
            "snapshot_file" => (
                ToolSummaryKind::Other,
                format!("snapshot {}", mutation_target),
            ),
            "restore_file" => (
                ToolSummaryKind::Mutation,
                format!("{} restored", mutation_target),
            ),
            "bash" => {
                let command = tc
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("bash");
                (
                    ToolSummaryKind::Other,
                    format!("bash {}", compact_tool_label(command)),
                )
            }
            "document_symbols" | "hover" | "goto_definition" => {
                (ToolSummaryKind::Read, tc.name.clone())
            }
            _ => (ToolSummaryKind::Other, tc.name.clone()),
        };

        ToolEventSummary {
            kind,
            label: compact_tool_label(&label),
            call: tc.clone(),
        }
    }
}

fn tool_result_success(result: &ToolResult) -> Option<&str> {
    match result {
        ToolResult::Success(s) => Some(s.as_str()),
        ToolResult::Error(_) => None,
    }
}

fn first_number_in_text(text: &str) -> Option<usize> {
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn tool_line_range_suffix(args: &serde_json::Value) -> String {
    let start = args.get("start_line").and_then(|v| v.as_u64());
    let end = args.get("end_line").and_then(|v| v.as_u64());
    match (start, end) {
        (Some(s), Some(e)) => format!(":{s}-{e}"),
        (Some(s), None) => format!(":{s}"),
        _ => String::new(),
    }
}

fn diff_line_deltas(diff: &str) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added = added.saturating_add(1);
        } else if line.starts_with('-') {
            removed = removed.saturating_add(1);
        }
    }
    (added, removed)
}

#[cfg(test)]
#[path = "ai_chat_tools_tests.rs"]
mod tests;
