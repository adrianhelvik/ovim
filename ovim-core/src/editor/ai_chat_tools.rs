use crate::ai::chat_types::{ToolCallInfo, ToolSummaryKind};
use crate::ai::path_policy::sensitive_path_reason;
use crate::ai::scope::{Capabilities, ScopeContext};
use crate::ai::skills::ACTIVATE_SKILL_TOOL;
use crate::ai::tools::builtins::{OpenBufferState, ToolExecutionContext};
use crate::ai::tools::schema;
use crate::ai::tools::{SideEffect, ToolResult};
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
            .tools_for_profile(profile, &caps)
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
                    "Narratively ordered, bite-sized concept and code pages. A concept page may occupy at most {concept_rows} wrapped body rows; use it for one introductory mental model, prerequisite, transition, or synthesis without a code location. Prefer several focused concept pages over one dense page, and move to concrete code as soon as the reader is oriented. Code pages use project-relative paths and 1-indexed inclusive lines. Each optional range may occupy at most {safe_range} visual code rows after soft wrapping, and each comment may occupy at most 5 wrapped rows. Use single-line anchors for handoffs or invariants. Otherwise select the smallest condition, assignment, call, or block that proves the comment; never include the full surrounding function by default. Give every page one new idea and one necessary connection. If one page needs two ideas, split it into two pages. Repeating a range is encouraged when a later page adds a distinct perspective rather than restating the earlier comment."
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
                path: b.file_path().unwrap_or("[No Name]").to_string(),
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
                    let root = normalize_path(root);
                    if path.starts_with(&root) {
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
            "list_files" | "search_project" | "workspace_context"
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
            if let Some(chat) = self.ai_state.chat.as_mut() {
                let root = normalize_path(&pending.approval_root);
                if !chat
                    .approved_external_roots
                    .iter()
                    .any(|p| normalize_path(p) == root)
                {
                    chat.approved_external_roots.push(root);
                }
            }
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
            super::ai_chat_state::ShellExecutionContinuation::Batch {
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
            self.set_status_message(format!(
                "Approved AI tool access for folder: {}",
                root.display()
            ));
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
                let continuation = super::ai_chat_state::SubagentControlContinuation::Batch {
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
                                    super::ai_chat_state::ShellExecutionContinuation::Batch {
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

            if matches!(tc.name.as_str(), "web_search" | "web_fetch")
                && self.ai_chat_uses_direct_codex()
            {
                let call = tc.clone();
                let worker_call = call.clone();
                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                let task = tokio::task::spawn_blocking(move || {
                    let outcome =
                        crate::ai::exa::execute(&worker_call.name, &worker_call.arguments);
                    let _ = result_tx.send(outcome);
                });
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.tool_call_count = chat.tool_call_count.saturating_add(executed_in_batch);
                    chat.pending_web_execution = Some(super::ai_chat_state::PendingWebExecution {
                        tool_call: call,
                        runtime_tool: runtime_tool.as_ref().map(|(_, tool)| tool.clone()),
                        runtime_turn: runtime_tool.as_ref().map(|(turn, _)| turn.clone()),
                        remaining_tool_calls: tool_calls[idx + 1..].to_vec(),
                        model_name,
                        receiver: result_rx,
                        task,
                    });
                    chat.waiting = true;
                }
                self.set_status_message("Searching the web with Exa".to_string());
                return true;
            }

            let outcome = if let Some(outcome) = subagent_control_outcome {
                outcome
            } else if let Some(outcome) = shell_outcome {
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
mod tests {
    use super::*;
    use crate::ai::chat_types::{ChatOpts, ToolCallInfo};
    use crate::ai::skills::{SkillCatalog, ACTIVATED_SKILL_MARKER};
    use crate::ai::{FileScope, ToolApprovalMode};
    use crate::editor::ai_tool_execution::find_enclosing_symbol;
    use crate::editor::ai_tool_path::normalize_path;
    use std::fs;

    fn set_active_profile_project_scope(editor: &mut Editor) {
        let profile_name = editor.ai_state.active_profile.clone();
        if let Some(profile) = editor.ai_state.config.profiles.get_mut(&profile_name) {
            profile.scope.files = FileScope::Project;
        }
    }

    fn make_symbol(
        name: &str,
        kind: lsp_types::SymbolKind,
        start_line: u32,
        end_line: u32,
        children: Option<Vec<lsp_types::DocumentSymbol>>,
    ) -> lsp_types::DocumentSymbol {
        #[allow(deprecated)]
        lsp_types::DocumentSymbol {
            name: name.to_string(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: lsp_types::Range {
                start: lsp_types::Position::new(start_line, 0),
                end: lsp_types::Position::new(end_line, 0),
            },
            selection_range: lsp_types::Range {
                start: lsp_types::Position::new(start_line, 0),
                end: lsp_types::Position::new(start_line, 10),
            },
            children,
        }
    }

    #[test]
    fn find_enclosing_symbol_finds_deepest() {
        let symbols = vec![make_symbol(
            "MyStruct",
            lsp_types::SymbolKind::STRUCT,
            10,
            50,
            Some(vec![
                make_symbol("new", lsp_types::SymbolKind::FUNCTION, 15, 25, None),
                make_symbol("update", lsp_types::SymbolKind::FUNCTION, 30, 45, None),
            ]),
        )];

        // Cursor inside `new` function
        let result = find_enclosing_symbol(&symbols, 20);
        assert_eq!(result.unwrap().name, "new");

        // Cursor inside `update` function
        let result = find_enclosing_symbol(&symbols, 35);
        assert_eq!(result.unwrap().name, "update");

        // Cursor inside struct but outside any function
        let result = find_enclosing_symbol(&symbols, 48);
        assert_eq!(result.unwrap().name, "MyStruct");

        // Cursor outside all symbols
        let result = find_enclosing_symbol(&symbols, 5);
        assert!(result.is_none());
    }

    #[test]
    fn find_enclosing_symbol_empty() {
        assert!(find_enclosing_symbol(&[], 10).is_none());
    }

    #[test]
    fn skill_schema_is_lazy_and_restricts_activation_to_discovered_names() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("learn.md"),
            "---\nname: learn-codebase\ndescription: Teach the codebase.\n---\nOne concept at a time.\n",
        )
        .unwrap();
        let mut editor = Editor::default();
        editor.ai_state.skill_catalog = SkillCatalog::load_from_dir(directory.path());
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .unwrap()
            .clone();

        let schemas = editor.build_tool_schemas_for_chat(&profile);
        let activation = schemas
            .iter()
            .find(|schema| {
                schema.pointer("/function/name").and_then(|v| v.as_str())
                    == Some(ACTIVATE_SKILL_TOOL)
            })
            .expect("activation schema");
        assert_eq!(
            activation.pointer("/function/parameters/properties/name/enum"),
            Some(&serde_json::json!(["learn-codebase"]))
        );
        assert!(
            !activation.to_string().contains("One concept at a time."),
            "skill instructions must not be included in the tool schema"
        );
    }

    #[test]
    fn activating_skill_returns_catalog_instructions_without_a_file_target() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("learn.md"),
            "---\nname: learn-codebase\ndescription: Teach the codebase.\n---\nOne concept at a time.\n",
        )
        .unwrap();
        let mut editor = Editor::default();
        editor.ai_state.skill_catalog = SkillCatalog::load_from_dir(directory.path());
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let call = ToolCallInfo {
            id: "skill-1".into(),
            name: ACTIVATE_SKILL_TOOL.into(),
            arguments: serde_json::json!({"name": "learn-codebase"}),
        };

        match editor.dispatch_tool_call_with_approval(&call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
                assert!(content.starts_with(&format!("{ACTIVATED_SKILL_MARKER}learn-codebase")));
                assert!(content.contains("One concept at a time."));
            }
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                panic!("unexpected activation error: {error}")
            }
            ToolDispatchOutcome::ApprovalRequired(_) => {
                panic!("skill activation must not require path approval")
            }
        }
    }

    #[test]
    fn tool_summary_for_edit_range_reports_plus_minus_delta() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        let tc = ToolCallInfo {
            id: "call_1".to_string(),
            name: "edit_range".to_string(),
            arguments: serde_json::json!({
                "start_line": 10,
                "end_line": 12,
                "new_text": "a\nb\n"
            }),
        };
        let summary = editor.build_tool_event_summary(&tc, &ToolResult::Success("ok".to_string()));
        assert_eq!(summary.kind, ToolSummaryKind::Mutation);
        assert!(summary.label.contains("+0 -1"), "{}", summary.label);
    }

    #[test]
    fn edit_range_on_active_target_does_not_require_approval() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("main.rs");
            fs::write(&file, "line1\nline2\n").expect("seed");

            let mut editor = Editor::default();
            editor.open_file(&file).expect("open file");
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");

            let tool_call = ToolCallInfo {
                id: "call_edit".to_string(),
                name: "edit_range".to_string(),
                arguments: serde_json::json!({
                    "start_line": 1,
                    "end_line": 1,
                    "new_text": "updated",
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn same_revision_mutations_conflict_deterministically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("main.rs");
        fs::write(&file, "original\n").expect("seed");
        let mut editor = Editor::default();
        editor.open_file(&file).expect("open file");
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        let expected_revision = editor.build_tool_execution_context().buffer_revision;
        let call = |id: &str, new_text: &str| ToolCallInfo {
            id: id.to_string(),
            name: "edit_range".to_string(),
            arguments: serde_json::json!({
                "start_line": 1,
                "end_line": 1,
                "new_text": new_text,
                "expected_revision": expected_revision
            }),
        };

        assert!(matches!(
            editor.dispatch_tool_call_with_approval(&call("first", "first"), None),
            ToolDispatchOutcome::Completed(ToolResult::Success(_))
        ));
        match editor.dispatch_tool_call_with_approval(&call("second", "second"), None) {
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                assert!(error.contains("advanced from revision"), "{error}");
            }
            ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                panic!("stale mutation unexpectedly succeeded: {output}");
            }
            ToolDispatchOutcome::ApprovalRequired(request) => {
                panic!("unexpected approval: {}", request.message);
            }
        }
        assert!(editor.buffer().rope().to_string().starts_with("first\n"));
    }
    #[test]
    fn edit_range_with_other_path_requires_approval_in_sensitive_mode() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let main = dir.path().join("main.rs");
            let other = dir.path().join("other.rs");
            fs::write(&main, "line1\nline2\n").expect("seed main");
            fs::write(&other, "alpha\nbeta\n").expect("seed other");

            let mut editor = Editor::default();
            editor.open_file(&main).expect("open main");
            let original_target = editor.buffer().id();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            // Editor::default() loads the developer's real AI config; pin the
            // approval mode so the test doesn't depend on the host machine.
            editor.ai_state.config.tool_approval_mode = ToolApprovalMode::SensitivePrompt;

            let tool_call = ToolCallInfo {
                id: "call_edit_other".to_string(),
                name: "edit_range".to_string(),
                arguments: serde_json::json!({
                    "path": other.to_string_lossy().to_string(),
                    "start_line": 1,
                    "end_line": 1,
                    "new_text": "updated",
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    let requested = req
                        .requested_path
                        .canonicalize()
                        .unwrap_or_else(|_| normalize_path(&req.requested_path));
                    let expected = other
                        .canonicalize()
                        .unwrap_or_else(|_| normalize_path(&other));
                    assert_eq!(requested, expected);
                }
                ToolDispatchOutcome::Completed(ToolResult::Success(ok)) => {
                    panic!("expected approval request, got success: {ok}");
                }
                ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                    panic!("expected approval request, got error: {err}");
                }
            }

            assert_eq!(editor.ai_chat_attention_generation(), 0);
            assert!(editor.execute_tool_call_batch(vec![tool_call], "test".into()));
            assert!(editor.ai_chat_has_pending_tool_approval());
            assert_eq!(editor.ai_chat_attention_generation(), 1);

            assert_eq!(
                editor.ai_state.chat.as_ref().unwrap().active_buffer_id,
                original_target,
                "an approval request must not switch the chat target"
            );
            assert!(
                editor
                    .buffers
                    .iter()
                    .all(
                        |buffer| buffer
                            .file_path()
                            .is_none_or(|path| normalize_path(std::path::Path::new(path))
                                != normalize_path(&other))
                    ),
                "an approval request must not open the proposed target"
            );
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn yolo_mode_bypasses_outside_project_approval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("repo marker");
        let main = repo.join("main.rs");
        let outside = dir.path().join("outside.rs");
        fs::write(&main, "fn main() {}\n").expect("seed main");
        fs::write(&outside, "outside\n").expect("seed outside");

        let mut editor = Editor::default();
        editor.open_file(&main).expect("open main");
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        set_active_profile_project_scope(&mut editor);
        editor.ai_state.config.tool_approval_mode = ToolApprovalMode::SensitivePrompt;
        assert!(editor.set_ai_chat_yolo_mode(true));

        let call = ToolCallInfo {
            id: "outside-read".into(),
            name: "read_file_at_path".into(),
            arguments: serde_json::json!({"path": outside}),
        };
        match editor.dispatch_tool_call_with_approval(&call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(result)) => {
                assert!(result.contains("outside"), "{result}");
            }
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                panic!("YOLO read failed: {error}");
            }
            ToolDispatchOutcome::ApprovalRequired(request) => {
                panic!("YOLO requested approval: {}", request.message);
            }
        }
    }

    #[test]
    fn ai_repo_root_prefers_active_target_file() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo_a = dir.path().join("repo_a");
            let repo_b = dir.path().join("repo_b");
            fs::create_dir_all(repo_a.join(".git")).expect("mkdir repo_a/.git");
            fs::create_dir_all(repo_b.join(".git")).expect("mkdir repo_b/.git");
            let file_a = repo_a.join("a.rs");
            let file_b = repo_b.join("b.rs");
            fs::write(&file_a, "fn a() {}\n").expect("seed a");
            fs::write(&file_b, "fn b() {}\n").expect("seed b");

            let mut editor = Editor::default();
            editor.open_file(&file_a).expect("open a");
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");

            editor.open_file(&file_b).expect("open b");
            let file_b_buffer_id = editor.buffer().id();
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.active_buffer_id = file_b_buffer_id;
            }

            let detected = editor.ai_repo_root().expect("repo root");
            let detected = detected
                .canonicalize()
                .unwrap_or_else(|_| normalize_path(&detected));
            let expected = repo_b
                .canonicalize()
                .unwrap_or_else(|_| normalize_path(&repo_b));
            assert_eq!(detected, expected);
        });
    }

    #[test]
    fn ai_repo_root_detects_git_file_marker() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo = dir.path().join("worktree_like_repo");
            fs::create_dir_all(repo.join("src")).expect("mkdir src");
            fs::write(repo.join(".git"), "gitdir: /tmp/fake\n").expect("write .git marker");
            let file = repo.join("src").join("main.rs");
            fs::write(&file, "fn main() {}\n").expect("write file");

            let mut editor = Editor::default();
            editor.open_file(&file).expect("open file");

            let detected = editor.ai_repo_root().expect("repo root");
            let detected = detected
                .canonicalize()
                .unwrap_or_else(|_| normalize_path(&detected));
            let expected = repo
                .canonicalize()
                .unwrap_or_else(|_| normalize_path(&repo));
            assert_eq!(detected, expected);
        });
    }

    #[test]
    fn write_file_at_path_creates_missing_file() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("planning/nested/new_module.rs");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
                let canonical =
                    std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                if canonical != dir.path() {
                    chat.approved_external_roots.push(canonical);
                }
            }

            let tool_call = ToolCallInfo {
                id: "call_write".to_string(),
                name: "write_file_at_path".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "content": "pub fn generated() {}\n",
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let content = fs::read_to_string(&target).expect("read target");
            assert!(content.contains("pub fn generated() {}"));
            assert!(target.parent().unwrap().is_dir());
        });
    }

    #[test]
    fn missing_expected_revision_does_not_prepare_path_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("not_opened.rs");
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        let tool_call = ToolCallInfo {
            id: "call_write_without_revision".to_string(),
            name: "write_file_at_path".to_string(),
            arguments: serde_json::json!({
                "path": target.to_string_lossy().to_string(),
                "content": "must not be written\n"
            }),
        };
        match editor.dispatch_tool_call_with_approval(&tool_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                assert!(error.contains("'expected_revision' is required"));
            }
            ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                panic!("expected revision error, got success: {output}");
            }
            ToolDispatchOutcome::ApprovalRequired(request) => {
                panic!("expected revision error, got approval: {}", request.message);
            }
        }
        assert!(!target.exists());
        assert!(editor.buffers.iter().all(|buffer| {
            buffer
                .file_path()
                .is_none_or(|path| normalize_path(std::path::Path::new(path)) != target)
        }));
    }

    #[test]
    fn edit_range_with_path_updates_target_file() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("target.rs");
            fs::write(&target, "line1\nline2\n").expect("seed");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
                let canonical =
                    std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                if canonical != dir.path() {
                    chat.approved_external_roots.push(canonical);
                }
            }

            let tool_call = ToolCallInfo {
                id: "call_edit".to_string(),
                name: "edit_range".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "start_line": 1,
                    "end_line": 1,
                    "new_text": "updated",
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let content = fs::read_to_string(&target).expect("read target");
            assert!(content.starts_with("updated\n"));
        });
    }

    #[test]
    fn apply_patch_at_path_updates_target_file() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("patch_target.rs");
            fs::write(&target, "fn main() {\n    old_call();\n}\n").expect("seed");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
                let canonical =
                    std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                if canonical != dir.path() {
                    chat.approved_external_roots.push(canonical);
                }
            }

            let diff = format!(
                "*** Begin Patch\n*** Update File: {}\n@@ @@\n fn main() {{\n-    old_call();\n+    new_call();\n }}\n*** End Patch\n",
                target.to_string_lossy()
            );

            let tool_call = ToolCallInfo {
                id: "call_patch".to_string(),
                name: "apply_patch_at_path".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "diff": diff,
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected patch error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let content = fs::read_to_string(&target).expect("read target");
            assert!(content.contains("new_call();"));
            assert!(!content.contains("old_call();"));
        });
    }

    #[test]
    fn apply_patch_at_path_adds_file_in_missing_directory() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("planning/notes/design.md");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
            }

            let diff = format!(
                "*** Begin Patch\n*** Add File: {}\n+Design notes\n*** End Patch\n",
                target.to_string_lossy()
            );
            let tool_call = ToolCallInfo {
                id: "call_add_patch".to_string(),
                name: "apply_patch_at_path".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "diff": diff,
                    "expected_revision": 0
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected add-file patch error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            assert_eq!(fs::read_to_string(&target).unwrap(), "Design notes\n");
        });
    }

    #[test]
    fn snapshot_and_restore_file_round_trip() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("restore.rs");
            fs::write(&target, "alpha\nbeta\n").expect("seed");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            set_active_profile_project_scope(&mut editor);
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
                let canonical =
                    std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                if canonical != dir.path() {
                    chat.approved_external_roots.push(canonical);
                }
            }

            let snapshot_call = ToolCallInfo {
                id: "call_snap".to_string(),
                name: "snapshot_file".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string()
                }),
            };
            match editor.dispatch_tool_call_with_approval(&snapshot_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected snapshot error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let snapshot_id = editor
                .ai_state
                .chat
                .as_ref()
                .and_then(|c| c.file_snapshots.keys().next().cloned())
                .expect("snapshot id");

            let edit_call = ToolCallInfo {
                id: "call_edit".to_string(),
                name: "edit_range".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "start_line": 1,
                    "end_line": 1,
                    "new_text": "changed",
                    "expected_revision": 0
                }),
            };
            match editor.dispatch_tool_call_with_approval(&edit_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected edit error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let restore_revision = editor.build_tool_execution_context().buffer_revision;
            let restore_call = ToolCallInfo {
                id: "call_restore".to_string(),
                name: "restore_file".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "snapshot_id": snapshot_id,
                    "expected_revision": restore_revision
                }),
            };
            match editor.dispatch_tool_call_with_approval(&restore_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected restore error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            let content = fs::read_to_string(&target).expect("read target");
            assert_eq!(content, "alpha\nbeta\n");
        });
    }

    #[test]
    fn tool_context_uses_active_chat_target_buffer() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let file_a = dir.path().join("a.rs");
            let file_b = dir.path().join("b.rs");
            fs::write(&file_a, "from_a\n").expect("seed a");
            fs::write(&file_b, "from_b\n").expect("seed b");

            let mut editor = Editor::default();
            set_active_profile_project_scope(&mut editor);
            editor.open_file(&file_a).expect("open a");
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            let active_buffer_id = editor
                .ai_state
                .chat
                .as_ref()
                .map(|c| c.active_buffer_id)
                .expect("chat");

            // User switches current buffer, but active chat target should stay on file_a.
            editor.open_file(&file_b).expect("open b");
            let active_idx = editor
                .find_buffer_index_by_id(active_buffer_id)
                .expect("active buffer index");
            assert_ne!(editor.current_buffer_index(), active_idx);

            let ctx = editor.build_tool_execution_context();
            assert!(ctx
                .file_path
                .as_deref()
                .is_some_and(|p| p.ends_with("a.rs")));
            assert!(ctx.buffer_content.contains("from_a"));
            assert!(ctx
                .visible_file_path
                .as_deref()
                .is_some_and(|p| p.ends_with("b.rs")));
            assert!(ctx.visible_buffer_content.contains("from_b"));

            let read_call = ToolCallInfo {
                id: "call_read_visible".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            };
            match editor.dispatch_tool_call_with_approval(&read_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                    assert!(output.contains("from_b"), "{output}");
                    assert!(!output.contains("from_a"), "{output}");
                }
                _ => panic!("unexpected read outcome"),
            }

            let mut workspace_ctx = editor.build_tool_execution_context();
            workspace_ctx.scope_context.project_root = Some(dir.path().to_path_buf());
            match crate::ai::tools::builtins::execute_builtin(
                "workspace_context",
                &serde_json::json!({
                    "include_git": false,
                    "include_projects": false,
                    "include_diagnostics_summary": false,
                }),
                &workspace_ctx,
            ) {
                ToolResult::Success(output) => {
                    assert!(output.contains("Visible buffer:\n"), "{output}");
                    assert!(output.contains("b.rs"), "{output}");
                    assert!(output.contains("Chat target:\n"), "{output}");
                    assert!(output.contains("a.rs"), "{output}");
                }
                ToolResult::Error(error) => panic!("unexpected workspace error: {error}"),
            }
        });
    }

    #[test]
    fn open_file_with_create_opens_missing_target() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("new_file.rs");

            let mut editor = Editor::default();
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            if let Some(chat) = editor.ai_state.chat.as_mut() {
                chat.approved_external_roots.push(dir.path().to_path_buf());
                let canonical =
                    std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                if canonical != dir.path() {
                    chat.approved_external_roots.push(canonical);
                }
            }

            let tool_call = ToolCallInfo {
                id: "call_open".to_string(),
                name: "open_file".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "create": true
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
                ToolDispatchOutcome::Completed(ToolResult::Error(e)) => {
                    panic!("unexpected error: {e}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }

            assert!(editor
                .buffer()
                .file_path()
                .is_some_and(|p| p.ends_with("new_file.rs")));
            assert_eq!(
                editor.registers().get(Some('%')),
                target.to_string_lossy().to_string()
            );
        });
    }

    #[test]
    fn no_file_open_limits_toolset_to_file_scope_and_keeps_open_file() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        let active = editor.ai_state.active_profile.clone();
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&active)
            .expect("profile");
        let caps = editor.build_chat_capabilities();
        let names: Vec<&str> = editor
            .ai_state
            .tool_registry
            .tools_for_profile(profile, &caps)
            .into_iter()
            .map(|t| t.name.as_str())
            .collect();

        assert!(names.contains(&"open_file"));
        assert!(!names.contains(&"list_files"));
        assert!(!names.contains(&"search_project"));
    }

    #[test]
    fn editable_chat_enables_bash_tool_by_default() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        let caps = editor.build_chat_capabilities();
        assert!(caps.shell, "editable chat should enable shell capability");

        let active = editor.ai_state.active_profile.clone();
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&active)
            .expect("profile");
        let names: Vec<&str> = editor
            .ai_state
            .tool_registry
            .tools_for_profile(profile, &caps)
            .into_iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(names.contains(&"bash"));
    }

    #[test]
    fn editable_codex_chat_advertises_shell_and_mutation_dynamic_tools() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        let mut profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .expect("profile")
            .clone();
        profile.provider = crate::ai::AiProviderKind::Codex;
        profile.tools.clear();

        let schemas = editor.build_tool_schemas_for_chat(&profile);
        let names = schemas
            .iter()
            .filter_map(|schema| schema.get("function"))
            .filter_map(|function| function.get("name"))
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash"), "schemas: {schemas:?}");
        assert!(names.contains(&"edit_range"), "schemas: {schemas:?}");
        assert!(names.contains(&"insert_lines"), "schemas: {schemas:?}");
    }

    #[test]
    fn view_image_loads_a_project_image_for_the_tool_result() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            git2::Repository::init(directory.path()).unwrap();
            let source = directory.path().join("main.rs");
            let image = directory.path().join("mockup.png");
            fs::write(&source, "fn main() {}\n").unwrap();
            fs::write(&image, b"\x89PNG\r\n\x1a\nminimal").unwrap();

            let mut editor = Editor::default();
            editor.open_file(&source).unwrap();
            editor.open_ai_chat(ChatOpts::default()).unwrap();
            set_active_profile_project_scope(&mut editor);
            let mut profile = editor
                .ai_state
                .config
                .resolve_profile(&editor.ai_state.active_profile)
                .unwrap()
                .clone();
            profile.provider = crate::ai::AiProviderKind::Codex;
            profile.tools.clear();
            let schemas = editor.build_tool_schemas_for_chat(&profile);
            let schema_names = schemas
                .iter()
                .filter_map(|schema| schema.get("function"))
                .filter_map(|function| function.get("name"))
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>();
            assert!(
                schema_names.contains(&"view_image"),
                "schemas: {schema_names:?}"
            );
            let call = ToolCallInfo {
                id: "call_image".into(),
                name: "view_image".into(),
                arguments: serde_json::json!({"path":"mockup.png"}),
            };

            match editor.dispatch_tool_call_with_approval(&call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(message)) => {
                    assert!(message.contains("mockup.png"));
                }
                ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                    panic!("unexpected image tool error: {error}");
                }
                ToolDispatchOutcome::ApprovalRequired(request) => {
                    panic!("unexpected approval request: {}", request.message);
                }
            }

            let images = editor.take_tool_result_images(&call.id);
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].path, image.canonicalize().unwrap());
            assert_eq!(images[0].mime_type, "image/png");
            editor
                .conversation_mut()
                .unwrap()
                .append_tool_result_with_images(call.id, "Image attached".into(), images);
            assert_eq!(
                editor
                    .conversation()
                    .unwrap()
                    .messages()
                    .last()
                    .unwrap()
                    .images
                    .len(),
                1
            );
        });
    }

    #[test]
    fn bash_tool_executes_shell_composition_after_policy() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("main.rs");
            fs::write(&file, "fn main() {}\n").expect("seed");

            let mut editor = Editor::default();
            editor.open_file(&file).expect("open file");
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            editor.ai_state.config.tool_approval_mode = ToolApprovalMode::Auto;
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());

            let tool_call = ToolCallInfo {
                id: "call_bash".to_string(),
                name: "bash".to_string(),
                arguments: serde_json::json!({
                    "command": "printf 'alpha\\nbeta\\n' | tail -n 1"
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(ok)) => {
                    assert!(ok.contains("beta"), "{ok}");
                }
                ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                    panic!("expected compound shell program to run: {err}");
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }
        });
    }

    #[test]
    fn bash_tool_executes_simple_program_in_project_root() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("main.rs");
            fs::write(&file, "fn main() {}\n").expect("seed");

            let mut editor = Editor::default();
            editor.open_file(&file).expect("open file");
            editor
                .open_ai_chat(ChatOpts {
                    name: "chat".to_string(),
                    allow_edits: true,
                    ..Default::default()
                })
                .expect("open chat");
            editor.ai_state.config.tool_approval_mode = ToolApprovalMode::Auto;
            editor.ai_state.no_repo_session_allowed_root = Some(dir.path().to_path_buf());

            let tool_call = ToolCallInfo {
                id: "call_bash_pwd".to_string(),
                name: "bash".to_string(),
                arguments: serde_json::json!({
                    "command": "pwd"
                }),
            };

            match editor.dispatch_tool_call_with_approval(&tool_call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(ok)) => {
                    assert!(ok.contains("succeeded"), "{ok}");
                }
                ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                    assert!(
                        err.contains("failed to execute"),
                        "expected execution-attempt error, got: {err}"
                    );
                }
                ToolDispatchOutcome::ApprovalRequired(req) => {
                    panic!("unexpected approval request: {}", req.message);
                }
            }
        });
    }

    #[test]
    fn no_file_open_returns_consistent_guidance_for_non_open_tools() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        let tool_call = ToolCallInfo {
            id: "call_read".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        };

        match editor.dispatch_tool_call_with_approval(&tool_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                assert!(err.contains("No file open."));
                assert!(err.contains("open_file(path, create=true)"));
            }
            ToolDispatchOutcome::Completed(ToolResult::Success(ok)) => {
                panic!("expected guidance error, got success: {ok}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }
    }

    #[test]
    fn project_tools_work_from_unnamed_buffer_when_project_root_is_known() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        set_active_profile_project_scope(&mut editor);
        // Pin the approval mode; Editor::default() loads the developer's real
        // AI config, and AlwaysPrompt would fail this test with an approval.
        editor.ai_state.config.tool_approval_mode = ToolApprovalMode::Auto;

        let list_call = ToolCallInfo {
            id: "call_list".to_string(),
            name: "list_files".to_string(),
            arguments: serde_json::json!({}),
        };
        match editor.dispatch_tool_call_with_approval(&list_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                assert!(output.contains("Cargo.toml"), "{output}");
            }
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                panic!("expected list_files success, got: {err}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }

        let search_call = ToolCallInfo {
            id: "call_search".to_string(),
            name: "search_project".to_string(),
            arguments: serde_json::json!({
                "query": "project_tools_work_from_unnamed_buffer_when_project_root_is_known"
            }),
        };
        match editor.dispatch_tool_call_with_approval(&search_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                assert!(output.contains("ai_chat_tools.rs"), "{output}");
            }
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                panic!("expected search_project success, got: {err}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }

        let workspace_context_call = ToolCallInfo {
            id: "call_workspace_context".to_string(),
            name: "workspace_context".to_string(),
            arguments: serde_json::json!({"include_git": false}),
        };
        match editor.dispatch_tool_call_with_approval(&workspace_context_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                assert!(output.contains("Workspace:"), "{output}");
                assert!(output.contains("Visible buffer:\n  [No Name]"), "{output}");
            }
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                panic!("expected workspace_context success, got: {err}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn invalid_chat_target_allows_visible_reads_but_rejects_implicit_mutations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let visible_file = dir.path().join("visible.rs");
        fs::write(&visible_file, "visible\n").expect("seed visible file");
        let mut editor = Editor::default();
        editor.open_file(&visible_file).expect("open visible file");
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.active_buffer_id = u64::MAX;
        }

        let tool_call = ToolCallInfo {
            id: "call_read".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        };

        match editor.dispatch_tool_call_with_approval(&tool_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                panic!("expected visible-buffer read, got: {err}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }

        let mutation_call = ToolCallInfo {
            id: "call_edit".to_string(),
            name: "edit_range".to_string(),
            arguments: serde_json::json!({}),
        };
        match editor.dispatch_tool_call_with_approval(&mutation_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Error(err)) => {
                assert!(err.contains("Active chat target is no longer available"));
            }
            ToolDispatchOutcome::Completed(ToolResult::Success(ok)) => {
                panic!("expected invalid-target error, got success: {ok}");
            }
            ToolDispatchOutcome::ApprovalRequired(req) => {
                panic!("unexpected approval request: {}", req.message);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn structured_tool_batch_emits_runtime_intent_start_and_result() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".into(),
                allow_edits: true,
                ..Default::default()
            })
            .unwrap();
        let turn = editor.begin_ai_runtime_turn("check diagnostics").unwrap();
        let run_id = turn.run_id.clone();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));

        editor.execute_tool_call_batch(
            vec![ToolCallInfo {
                id: "structured-call-1".into(),
                name: "read_diagnostics".into(),
                arguments: serde_json::json!({}),
            }],
            "test".into(),
        );

        let events = editor.ai_state.agent_runtime.events(&run_id).unwrap();
        let labels = events
            .iter()
            .filter_map(|event| match &event.kind {
                crate::run_log::EventKind::ToolIntent(_) => Some("intent"),
                crate::run_log::EventKind::ToolStarted(_) => Some("started"),
                crate::run_log::EventKind::ToolResult(_) => Some("result"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["intent", "started", "result"]);
        editor.close_ai_chat();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_bash_tool_batch_does_not_block_editor_polling() {
        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        let file = repo.path().join("main.rs");
        fs::write(&file, "fn main() {}\n// second line\n").unwrap();
        let runs = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .unwrap();
        editor.open_file(&file).unwrap();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".into(),
                allow_edits: true,
                ..Default::default()
            })
            .unwrap();
        editor.ai_state.chat.as_mut().unwrap().yolo_mode = true;
        let turn = editor
            .begin_ai_runtime_turn("run the browser checks")
            .unwrap();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));

        let started = std::time::Instant::now();
        assert!(editor.execute_tool_call_batch(
            vec![ToolCallInfo {
                id: "batch-shell".into(),
                name: "bash".into(),
                arguments: serde_json::json!({
                    "command": "while [ ! -f release-gate ]; do sleep 0.01; done; touch batch-finished"
                }),
            }],
            "test".into(),
        ));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert_eq!(
            editor.ai_chat_activity(),
            super::super::AiChatActivity::RunningShell
        );
        assert!(!editor.poll_pending_ai_chat_job());

        editor.set_mode(crate::mode::Mode::Normal);
        crate::editor::InputHandler::handle_key_event(
            &mut editor,
            crate::KeyEvent::new(crate::KeyCode::Char('j'), crate::Modifiers::NONE),
        )
        .unwrap();
        assert_eq!(editor.cursor_position().line, 1);

        fs::write(repo.path().join("release-gate"), "go").unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .is_some()
        {
            editor.poll_pending_ai_chat_job();
            assert!(
                tokio::time::Instant::now() < deadline,
                "batch shell did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(repo.path().join("batch-finished").exists());
        editor.close_ai_chat();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn batch_shell_unknown_outcome_clears_waiting_and_closes_tool_calls() {
        use crate::ai::chat_types::ChatRole;

        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".into(),
                allow_edits: true,
                ..Default::default()
            })
            .unwrap();

        let shell_call = ToolCallInfo {
            id: "shell-1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "true" }),
        };
        let follow_up = ToolCallInfo {
            id: "read-2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        // Commit the assistant tool_use blocks like process_tool_calls does.
        editor
            .conversation_mut()
            .unwrap()
            .append_assistant_message_with_tools(
                String::new(),
                "test".into(),
                vec![shell_call.clone(), follow_up.clone()],
            );

        // Park a batch shell whose sender is dropped: the observation channel
        // closes without a result and the outcome is unknown.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<
            super::super::ai_chat_state::ShellExecutionObservation,
        >();
        drop(result_tx);
        let task = tokio::spawn(async {});
        if let Some(chat) = editor.ai_state.chat.as_mut() {
            chat.pending_shell_execution =
                Some(super::super::ai_chat_state::PendingShellExecution {
                    tool_call: shell_call,
                    continuation: super::super::ai_chat_state::ShellExecutionContinuation::Batch {
                        runtime_tool: None,
                        runtime_turn: None,
                        remaining_tool_calls: vec![follow_up],
                        model_name: "test".into(),
                    },
                    receiver: result_rx,
                    progress: tokio::sync::mpsc::unbounded_channel().1,
                    task,
                    kill: std::sync::Arc::new(
                        super::super::ai_chat_state::ShellKillHandle::default(),
                    ),
                });
            chat.waiting = true;
        }

        assert!(editor.poll_pending_ai_chat_job());

        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(
            !chat.waiting,
            "unknown batch shell outcome must not re-arm the waiting spinner"
        );
        assert!(chat.pending_shell_execution.is_none());
        assert!(chat.pending_job.is_none());
        assert!(!editor.ai_chat_has_pending_work());

        // Every committed tool_use got a closing tool_result, and the failure
        // itself is surfaced as an error message.
        let messages = editor.conversation().unwrap().messages();
        let tool_ids: Vec<_> = messages
            .iter()
            .filter(|m| m.role == ChatRole::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(tool_ids, ["shell-1", "read-2"]);
        assert!(messages
            .iter()
            .filter(|m| m.role == ChatRole::Tool)
            .all(|m| m.content.contains("Outcome unknown")));
        assert!(messages.iter().any(|m| m.role == ChatRole::Error));
        editor.close_ai_chat();
    }

    async fn run_batch_shell_to_completion(yolo: bool) -> String {
        use crate::ai::chat_types::ChatRole;

        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        let file = repo.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        let runs = tempfile::tempdir().unwrap();
        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .unwrap();
        editor.open_file(&file).unwrap();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".into(),
                allow_edits: true,
                ..Default::default()
            })
            .unwrap();
        editor.ai_state.config.tool_approval_mode = ToolApprovalMode::SensitivePrompt;
        editor.ai_state.chat.as_mut().unwrap().yolo_mode = yolo;
        let turn = editor.begin_ai_runtime_turn("run a shell check").unwrap();
        editor.ai_state.chat.as_mut().unwrap().runtime_turn = Some(Box::new(turn));

        let call = ToolCallInfo {
            id: "batch-shell".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "printf 'approved path check'" }),
        };
        let started = std::time::Instant::now();
        assert!(editor.execute_tool_call_batch(vec![call], "test".into()));

        if !yolo {
            // SensitivePrompt mode: batch shell pauses for approval and must
            // resume through the asynchronous parking path, not a synchronous
            // dispatch on the editor thread.
            assert!(editor
                .ai_state
                .chat
                .as_ref()
                .unwrap()
                .pending_tool_approval
                .is_some());
            assert!(editor
                .ai_state
                .chat
                .as_ref()
                .unwrap()
                .pending_shell_execution
                .is_none());
            assert!(editor.ai_chat_resolve_pending_tool_approval(true, false));
        }
        assert!(
            editor
                .ai_state
                .chat
                .as_ref()
                .unwrap()
                .pending_shell_execution
                .is_some(),
            "batch shell must park on pending_shell_execution"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "shell execution must not block the editor thread"
        );

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while editor
            .ai_state
            .chat
            .as_ref()
            .unwrap()
            .pending_shell_execution
            .is_some()
        {
            editor.poll_pending_ai_chat_job();
            assert!(
                tokio::time::Instant::now() < deadline,
                "batch shell did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(editor.ai_state.chat.as_ref().unwrap().tool_call_count, 1);
        let envelope = editor
            .conversation()
            .unwrap()
            .messages()
            .iter()
            .find(|m| m.role == ChatRole::Tool && m.tool_call_id.as_deref() == Some("batch-shell"))
            .map(|m| m.content.clone())
            .expect("tool result recorded");
        editor.close_ai_chat();
        envelope
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_batch_shell_parks_and_matches_yolo_result_envelope() {
        let yolo_envelope = run_batch_shell_to_completion(true).await;
        let approved_envelope = run_batch_shell_to_completion(false).await;
        // The first line carries the target path (tempdir-specific); the rest
        // of the envelope must be identical to the unprompted yolo path.
        let body = |envelope: &str| {
            envelope
                .split_once('\n')
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_default()
        };
        assert!(body(&yolo_envelope).contains("approved path check"));
        assert_eq!(body(&yolo_envelope), body(&approved_envelope));
    }
}
