//! LSP code actions, formatting, and refactoring
//!
//! This module handles:
//! - Document formatting
//! - Code actions (quick fixes, refactors)
//! - Organize imports
//! - Rename symbol
//! - Semantic tokens

use super::super::Editor;
use crate::editor::lsp_state::AvailableCodeAction;
use anyhow::Result;

fn fallback_code_action_character(
    current_character: u32,
    diagnostics: &[lsp_types::Diagnostic],
) -> Option<u32> {
    let min_start = diagnostics.iter().map(|d| d.range.start.character).min()?;
    if min_start == current_character {
        None
    } else {
        Some(min_start)
    }
}

fn is_organize_imports_action(action: &lsp_types::CodeActionOrCommand) -> bool {
    fn is_organize_imports_kind(kind: &lsp_types::CodeActionKind) -> bool {
        let kind = kind.as_str();
        let organize_imports_kind = lsp_types::CodeActionKind::SOURCE_ORGANIZE_IMPORTS;
        let organize_imports = organize_imports_kind.as_str();
        kind == organize_imports
            || kind
                .strip_prefix(organize_imports)
                .is_some_and(|suffix| suffix.starts_with('.'))
    }

    match action {
        lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action
                .kind
                .as_ref()
                .is_some_and(is_organize_imports_kind)
                || code_action
                    .command
                    .as_ref()
                    .is_some_and(|cmd| cmd.command.contains("organizeImports"))
        }
        lsp_types::CodeActionOrCommand::Command(cmd) => cmd.command.contains("organizeImports"),
    }
}

pub(in crate::editor) fn code_action_title(action: &lsp_types::CodeActionOrCommand) -> String {
    match action {
        lsp_types::CodeActionOrCommand::CodeAction(ca) => ca.title.clone(),
        lsp_types::CodeActionOrCommand::Command(cmd) => cmd.title.clone(),
    }
}

fn needs_code_action_resolve(action: &lsp_types::CodeActionOrCommand) -> bool {
    match action {
        lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.edit.is_none() && code_action.data.is_some()
        }
        lsp_types::CodeActionOrCommand::Command(_) => false,
    }
}

fn build_available_code_action(
    server_id: String,
    action: lsp_types::CodeActionOrCommand,
) -> AvailableCodeAction {
    AvailableCodeAction {
        server_id,
        resolved: !needs_code_action_resolve(&action),
        action,
    }
}

async fn resolve_available_code_actions(
    lsp: &crate::lsp::LspManager,
    actions: Vec<AvailableCodeAction>,
) -> Vec<AvailableCodeAction> {
    let mut resolved_actions = Vec::with_capacity(actions.len());
    for mut candidate in actions {
        if !candidate.resolved {
            if let lsp_types::CodeActionOrCommand::CodeAction(code_action) = &candidate.action {
                match lsp
                    .resolve_code_action_on_server_id(&candidate.server_id, code_action.clone())
                    .await
                {
                    Ok(resolved) => {
                        candidate.action = lsp_types::CodeActionOrCommand::CodeAction(resolved);
                        candidate.resolved = true;
                    }
                    Err(e) => {
                        crate::lsp_debug!(
                            "LSP-ACTION",
                            "codeAction/resolve failed on {}: {}",
                            candidate.server_id,
                            e
                        );
                    }
                }
            }
        }
        resolved_actions.push(candidate);
    }
    resolved_actions
}

impl Editor {
    pub(in crate::editor) async fn format_document_impl(&mut self) -> Result<bool> {
        let ctx = self.prepare_lsp_request("format").await?;

        self.set_lsp_status("Formatting document...".to_string());

        let tab_size = self.options.tab_width as u32;
        let insert_spaces = self.options.expand_tab;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = ctx
                .lsp
                .format_document(&ctx.uri, &ctx.language_id, tab_size, insert_spaces)
                .await;
            let _ = tx.send(result.map(|edits| crate::editor::lsp_slot::FormatResult { edits }));
        });

        self.lsp.slots.format.fire(task, rx);
        Ok(true)
    }

    pub(in crate::editor) async fn code_actions_impl(&mut self) -> Result<bool> {
        let ctx = self.prepare_lsp_request("code actions").await?;

        self.set_lsp_status("Fetching code actions...".to_string());

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            // Get diagnostics for the current line to provide context for code actions
            let diagnostics = ctx.lsp.get_diagnostics_for_line(&ctx.uri, ctx.line).await;
            let result = if ctx.server_ids.len() > 1 {
                ctx.lsp
                    .code_actions_multi_with_sources(
                        &ctx.uri,
                        ctx.line,
                        ctx.character,
                        &ctx.server_ids,
                        diagnostics.clone(),
                    )
                    .await
            } else {
                ctx.lsp
                    .code_actions(
                        &ctx.uri,
                        ctx.line,
                        ctx.character,
                        &ctx.language_id,
                        diagnostics.clone(),
                    )
                    .await
                    .map(|actions| {
                        actions
                            .into_iter()
                            .map(|action| (ctx.language_id.clone(), action))
                            .collect()
                    })
            };
            let result = match result {
                Ok(actions) if actions.is_empty() && !diagnostics.is_empty() => {
                    // Some servers only return quickfixes when the request position
                    // intersects the diagnostic span, not just the diagnostic line.
                    if let Some(fallback_character) =
                        fallback_code_action_character(ctx.character, &diagnostics)
                    {
                        let retry = if ctx.server_ids.len() > 1 {
                            ctx.lsp
                                .code_actions_multi_with_sources(
                                    &ctx.uri,
                                    ctx.line,
                                    fallback_character,
                                    &ctx.server_ids,
                                    diagnostics.clone(),
                                )
                                .await
                        } else {
                            ctx.lsp
                                .code_actions(
                                    &ctx.uri,
                                    ctx.line,
                                    fallback_character,
                                    &ctx.language_id,
                                    diagnostics.clone(),
                                )
                                .await
                                .map(|retry_actions| {
                                    retry_actions
                                        .into_iter()
                                        .map(|action| (ctx.language_id.clone(), action))
                                        .collect()
                                })
                        };

                        match retry {
                            Ok(retry_actions) if !retry_actions.is_empty() => Ok(retry_actions),
                            Ok(_) => Ok(actions),
                            Err(e) => {
                                crate::lsp_debug!(
                                    "LSP-ACTION",
                                    "Code action fallback retry failed: {}",
                                    e
                                );
                                Ok(actions)
                            }
                        }
                    } else {
                        Ok(actions)
                    }
                }
                other => other,
            };

            let task_result = match result {
                Ok(actions) if !actions.is_empty() => {
                    let available: Vec<_> = actions
                        .iter()
                        .map(|(server_id, action)| {
                            build_available_code_action(server_id.clone(), action.clone())
                        })
                        .collect();
                    let available =
                        resolve_available_code_actions(ctx.lsp.as_ref(), available).await;
                    Ok(crate::editor::lsp_slot::CodeActionsResult { actions: available })
                }
                Ok(_) => Ok(crate::editor::lsp_slot::CodeActionsResult {
                    actions: Vec::new(),
                }),
                Err(e) => Err(e),
            };

            let _ = tx.send(task_result);
        });

        self.lsp.slots.code_actions.fire(task, rx);
        Ok(true)
    }

    fn execute_code_action_command(
        &mut self,
        source_server_id: &str,
        command: lsp_types::Command,
    ) -> bool {
        let lsp = match &self.lsp.state.lsp_manager {
            Some(lsp) => lsp.clone(),
            None => {
                self.set_lsp_status("LSP not available".to_string());
                return false;
            }
        };

        let command_name = command.command.clone();
        let command_args = command.arguments.clone();
        let server_id = source_server_id.to_string();

        tokio::spawn(async move {
            if let Err(e) = lsp
                .execute_command_on_server_id(command_name.clone(), command_args, &server_id)
                .await
            {
                crate::lsp_warn!(
                    "LSP-CODE-ACTION",
                    "Failed to execute code action command '{}' on server '{}': {}",
                    command_name,
                    server_id,
                    e
                );
            }
        });

        true
    }

    /// Apply a code action by index from available code actions
    pub fn apply_code_action(&mut self, action_index: usize) {
        if action_index >= self.lsp.state.available_code_actions.len() {
            self.set_lsp_status("Invalid code action index".to_string());
            return;
        }

        let available = self.lsp.state.available_code_actions[action_index].clone();

        match available.action {
            lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
                let mut applied_edit = false;
                if let Some(workspace_edit) = code_action.edit {
                    match self.apply_workspace_edit(workspace_edit) {
                        Ok(applied) => {
                            applied_edit = applied;
                        }
                        Err(e) => {
                            self.set_lsp_status(format!("Failed to apply code action edit: {}", e));
                        }
                    }
                }

                let (had_command, executed_command) = match code_action.command {
                    Some(command) => (
                        true,
                        self.execute_code_action_command(&available.server_id, command),
                    ),
                    None => (false, false),
                };

                if applied_edit && executed_command {
                    self.set_lsp_status("Code action applied and command executed".to_string());
                } else if applied_edit {
                    self.set_lsp_status("Code action applied".to_string());
                } else if executed_command {
                    self.set_lsp_status("Executing code action command...".to_string());
                } else if !had_command && !available.resolved {
                    self.set_lsp_status(
                        "Code action unresolved and has no edit or command".to_string(),
                    );
                } else if !had_command {
                    self.set_lsp_status("Code action has no edit or command".to_string());
                }
            }
            lsp_types::CodeActionOrCommand::Command(command) => {
                if self.execute_code_action_command(&available.server_id, command) {
                    self.set_lsp_status("Executing code action command...".to_string());
                }
            }
        }

        self.lsp.state.available_code_actions.clear();
    }

    pub(in crate::editor) async fn organize_imports_impl(&mut self) -> Result<bool> {
        let ctx = self.prepare_lsp_request("organize imports").await?;

        self.set_lsp_status("Organizing imports...".to_string());

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            // Request code actions for organize imports (at file start, no diagnostics needed)
            let diagnostics = Vec::new();
            let result = if ctx.server_ids.len() > 1 {
                ctx.lsp
                    .code_actions_multi_with_sources(&ctx.uri, 0, 0, &ctx.server_ids, diagnostics)
                    .await
            } else {
                ctx.lsp
                    .code_actions(&ctx.uri, 0, 0, &ctx.language_id, diagnostics)
                    .await
                    .map(|actions| {
                        actions
                            .into_iter()
                            .map(|action| (ctx.language_id.clone(), action))
                            .collect()
                    })
            };

            let task_result = match result {
                Ok(actions) => {
                    let available: Vec<_> = actions
                        .into_iter()
                        .map(|(server_id, action)| build_available_code_action(server_id, action))
                        .collect();
                    let available =
                        resolve_available_code_actions(ctx.lsp.as_ref(), available).await;
                    let organize_action = available
                        .into_iter()
                        .find(|action| is_organize_imports_action(&action.action));

                    Ok(crate::editor::lsp_slot::OrganizeImportsResult {
                        action: organize_action,
                    })
                }
                Err(e) => Err(e),
            };

            let _ = tx.send(task_result);
        });

        self.lsp.slots.organize_imports.fire(task, rx);
        Ok(true)
    }

    pub(in crate::editor) async fn rename_impl(&mut self, new_name: String) -> Result<bool> {
        let ctx = self.prepare_lsp_request("rename").await?;

        self.set_lsp_status(format!("Renaming to '{}'...", new_name));
        let new_name_clone = new_name.clone();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = ctx
                .lsp
                .rename(
                    &ctx.uri,
                    ctx.line,
                    ctx.character,
                    &ctx.language_id,
                    new_name_clone,
                )
                .await;
            let _ = tx
                .send(result.map(|edit| crate::editor::lsp_slot::RenameResult { edit, new_name }));
        });

        self.lsp.slots.rename.fire(task, rx);
        Ok(true)
    }

    pub(in crate::editor) async fn semantic_tokens_impl(&mut self) -> Result<bool> {
        let ctx = self.prepare_lsp_request("semantic tokens").await?;

        self.set_lsp_status("Fetching semantic tokens...".to_string());

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let result = ctx
                .lsp
                .semantic_tokens_full(&ctx.uri, &ctx.language_id)
                .await;

            let task_result = match result {
                Ok(tokens) => {
                    let legend = ctx
                        .lsp
                        .get_semantic_tokens_legend(&ctx.language_id)
                        .await
                        .ok()
                        .flatten();
                    Ok(crate::editor::lsp_slot::SemanticTokensSlotResult { tokens, legend })
                }
                Err(e) => Err(e),
            };

            let _ = tx.send(task_result);
        });

        self.lsp.slots.semantic_tokens.fire(task, rx);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{fallback_code_action_character, is_organize_imports_action};
    use crate::editor::lsp_state::AvailableCodeAction;
    use crate::editor::Editor;
    use lsp_types::{
        CodeAction, CodeActionKind, CodeActionOrCommand, Command, Diagnostic, Position, Range,
        WorkspaceEdit,
    };

    fn diagnostic(start_character: u32, end_character: u32) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 10,
                    character: start_character,
                },
                end: Position {
                    line: 10,
                    character: end_character,
                },
            },
            ..Default::default()
        }
    }

    fn code_action_with_command(command: &str) -> CodeActionOrCommand {
        CodeActionOrCommand::CodeAction(CodeAction {
            title: "test".to_string(),
            kind: None,
            diagnostics: None,
            edit: None,
            command: Some(Command {
                title: "test".to_string(),
                command: command.to_string(),
                arguments: None,
            }),
            is_preferred: None,
            disabled: None,
            data: None,
        })
    }

    fn available(action: CodeActionOrCommand) -> AvailableCodeAction {
        AvailableCodeAction {
            server_id: "rust".to_string(),
            action,
            resolved: true,
        }
    }

    #[test]
    fn fallback_code_action_character_uses_min_diagnostic_start() {
        let diags = vec![diagnostic(12, 16), diagnostic(4, 8), diagnostic(7, 9)];
        assert_eq!(fallback_code_action_character(20, &diags), Some(4));
    }

    #[test]
    fn fallback_code_action_character_none_when_cursor_already_at_min_start() {
        let diags = vec![diagnostic(4, 8), diagnostic(10, 12)];
        assert_eq!(fallback_code_action_character(4, &diags), None);
    }

    #[test]
    fn fallback_code_action_character_none_with_no_diagnostics() {
        assert_eq!(fallback_code_action_character(5, &[]), None);
    }

    #[test]
    fn organize_imports_matches_command_variant() {
        let action = CodeActionOrCommand::Command(Command {
            title: "Organize Imports".to_string(),
            command: "rust-analyzer.applySourceChange.organizeImports".to_string(),
            arguments: None,
        });
        assert!(is_organize_imports_action(&action));
    }

    #[test]
    fn organize_imports_matches_code_action_with_embedded_command() {
        let action = code_action_with_command("typescript.organizeImports");
        assert!(is_organize_imports_action(&action));
    }

    #[test]
    fn organize_imports_matches_standard_and_child_kinds() {
        for kind in [
            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
            CodeActionKind::new("source.organizeImports.ts"),
        ] {
            let action = CodeActionOrCommand::CodeAction(CodeAction {
                title: "Organize imports".to_string(),
                kind: Some(kind),
                edit: Some(WorkspaceEdit::default()),
                ..Default::default()
            });

            assert!(is_organize_imports_action(&action));
        }
    }

    #[test]
    fn organize_imports_rejects_unrelated_code_action_with_edit() {
        let action = CodeActionOrCommand::CodeAction(CodeAction {
            title: "Apply quick fix".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            edit: Some(WorkspaceEdit::default()),
            ..Default::default()
        });

        assert!(!is_organize_imports_action(&action));
    }

    #[test]
    fn apply_code_action_command_only_uses_command_path() {
        let mut editor = Editor::new();
        editor.lsp.state.available_code_actions = vec![available(code_action_with_command(
            "rust-analyzer.applySourceChange",
        ))];

        editor.apply_code_action(0);

        assert_eq!(editor.lsp_status(), "LSP not available");
        assert!(editor.lsp.state.available_code_actions.is_empty());
    }

    #[test]
    fn apply_code_action_without_edit_or_command_reports_specific_status() {
        let mut editor = Editor::new();
        editor.lsp.state.available_code_actions =
            vec![available(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Noop".to_string(),
                kind: None,
                diagnostics: None,
                edit: None,
                command: None,
                is_preferred: None,
                disabled: None,
                data: None,
            }))];

        editor.apply_code_action(0);

        assert_eq!(editor.lsp_status(), "Code action has no edit or command");
        assert!(editor.lsp.state.available_code_actions.is_empty());
    }
}
