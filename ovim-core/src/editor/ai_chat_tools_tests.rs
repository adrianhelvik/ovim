use super::*;
use crate::ai::chat_types::{ChatOpts, ToolCallInfo};
use crate::ai::path_policy::canonicalize_or_normalize;
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
            schema.pointer("/function/name").and_then(|v| v.as_str()) == Some(ACTIVATE_SKILL_TOOL)
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
fn unnamed_chat_can_discover_buffer_ids_and_read_buffer_tool() {
    let mut editor = Editor::default();
    let visible_id = editor.buffer().id();
    editor.open_ai_chat(ChatOpts::default()).expect("open chat");
    let profile = editor
        .ai_state
        .config
        .resolve_profile(&editor.ai_state.active_profile)
        .expect("active profile")
        .clone();

    let schemas = editor.build_tool_schemas_for_chat(&profile);
    for expected in ["workspace_context", "read_buffer"] {
        assert!(
            schemas.iter().any(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(|value| value.as_str())
                    == Some(expected)
            }),
            "{expected} should be available without a project root"
        );
    }

    let call = ToolCallInfo {
        id: "workspace-no-root".into(),
        name: "workspace_context".into(),
        arguments: serde_json::json!({"include_git": false, "include_projects": false}),
    };
    match editor.dispatch_tool_call_with_approval(&call, None) {
        ToolDispatchOutcome::Completed(ToolResult::Success(metadata)) => {
            assert!(metadata.contains("Workspace:"), "{metadata}");
            assert!(
                metadata.contains(&format!("buffer {visible_id}")),
                "{metadata}"
            );
        }
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            panic!("workspace_context failed without a root: {error}")
        }
        ToolDispatchOutcome::ApprovalRequired(request) => {
            panic!(
                "workspace metadata unexpectedly required approval: {}",
                request.message
            )
        }
    }
}

#[test]
fn read_buffer_requires_approval_before_reading_non_visible_unnamed_content() {
    let mut editor = Editor::default();
    let secret = crate::buffer::Buffer::new_from_str("TOKEN=do-not-leak\n");
    let secret_id = secret.id();
    editor.push_buffer(secret);
    editor.open_ai_chat(ChatOpts::default()).expect("open chat");

    let workspace = crate::ai::tools::builtins::execute_builtin(
        "workspace_context",
        &serde_json::json!({"include_git": false, "include_projects": false}),
        &editor.build_tool_execution_context(),
    );
    match workspace {
        ToolResult::Success(metadata) => {
            assert!(
                metadata.contains(&format!("buffer {secret_id}")),
                "{metadata}"
            );
            assert!(metadata.contains("approval required to read"), "{metadata}");
            assert!(
                !metadata.contains("TOKEN="),
                "workspace metadata leaked unnamed buffer content: {metadata}"
            );
        }
        ToolResult::Error(error) => panic!("workspace_context failed: {error}"),
    }

    let call = ToolCallInfo {
        id: "read-buffer-1".into(),
        name: "read_buffer".into(),
        arguments: serde_json::json!({
            "buffer_id": secret_id,
            "start_line": 1,
            "end_line": 20,
        }),
    };

    let approval = match editor.dispatch_tool_call_with_approval(&call, None) {
        ToolDispatchOutcome::ApprovalRequired(request) => request,
        ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
            panic!("unnamed content leaked before approval: {content}")
        }
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            panic!("expected approval request, got error: {error}")
        }
    };
    assert!(approval.message.contains("unnamed buffer"));
    assert!(approval.message.contains(&secret_id.to_string()));

    match editor.dispatch_tool_call_with_approval(&call, Some(&approval.approval_root)) {
        ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
            assert!(content.contains("TOKEN=do-not-leak"));
        }
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            panic!("approved read failed: {error}")
        }
        ToolDispatchOutcome::ApprovalRequired(request) => {
            panic!(
                "approved read requested approval again: {}",
                request.message
            )
        }
    }
}

#[test]
fn read_buffer_reads_visible_unnamed_buffer_without_extra_approval() {
    let mut editor = Editor::default();
    *editor.buffer_mut() = crate::buffer::Buffer::new_from_str("visible notes\n");
    let visible_id = editor.buffer().id();
    editor.open_ai_chat(ChatOpts::default()).expect("open chat");

    let call = ToolCallInfo {
        id: "read-buffer-visible".into(),
        name: "read_buffer".into(),
        arguments: serde_json::json!({
            "buffer_id": visible_id,
            "start_line": 1,
            "end_line": 20,
        }),
    };

    match editor.dispatch_tool_call_with_approval(&call, None) {
        ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
            assert!(content.contains("visible notes"));
        }
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            panic!("visible buffer read failed: {error}")
        }
        ToolDispatchOutcome::ApprovalRequired(request) => {
            panic!(
                "visible buffer unexpectedly required approval: {}",
                request.message
            )
        }
    }

    let profile_name = editor.ai_state.active_profile.clone();
    editor
        .ai_state
        .config
        .profiles
        .get_mut(&profile_name)
        .expect("active profile")
        .tools = vec!["read_file".to_string()];
    match editor.dispatch_tool_call_with_approval(&call, None) {
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            assert!(
                error.contains("unavailable for the active profile"),
                "{error}"
            );
        }
        ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
            panic!("profile-excluded tool still read content: {content}")
        }
        ToolDispatchOutcome::ApprovalRequired(request) => {
            panic!(
                "profile-excluded tool requested approval: {}",
                request.message
            )
        }
    }
}

#[test]
fn read_buffer_rechecks_pathless_status_after_approval() {
    let mut editor = Editor::default();
    let hidden = crate::buffer::Buffer::new_from_str("sensitive scratch\n");
    let hidden_id = hidden.id();
    editor.push_buffer(hidden);
    editor.open_ai_chat(ChatOpts::default()).expect("open chat");

    let call = ToolCallInfo {
        id: "read-buffer-recheck".into(),
        name: "read_buffer".into(),
        arguments: serde_json::json!({"buffer_id": hidden_id}),
    };
    let approval = match editor.dispatch_tool_call_with_approval(&call, None) {
        ToolDispatchOutcome::ApprovalRequired(request) => request,
        _ => panic!("hidden unnamed buffer should require approval"),
    };

    editor
        .get_buffer_by_id_mut(hidden_id)
        .expect("hidden buffer")
        .set_file_path("/tmp/.env".to_string());

    match editor.dispatch_tool_call_with_approval(&call, Some(&approval.approval_root)) {
        ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
            assert!(error.contains("use read_file_at_path"), "{error}");
        }
        ToolDispatchOutcome::Completed(ToolResult::Success(content)) => {
            panic!("path-backed content bypassed path safety: {content}")
        }
        ToolDispatchOutcome::ApprovalRequired(request) => {
            panic!(
                "expected path-safety rejection, got approval: {}",
                request.message
            )
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
    fs::create_dir_all(&repo).expect("repo directory");
    git2::Repository::init(&repo).expect("init repository");
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
        fs::create_dir_all(&repo_a).expect("mkdir repo_a");
        fs::create_dir_all(&repo_b).expect("mkdir repo_b");
        git2::Repository::init(&repo_a).expect("init repo_a");
        git2::Repository::init(&repo_b).expect("init repo_b");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ai_repo_root_ignores_empty_git_directory_markers() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join(".git")).expect("empty lookalike marker");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("project directory");
    let file = project.join("main.rs");
    fs::write(&file, "fn main() {}\n").expect("write file");

    let mut editor = Editor::default();
    editor.open_file(&file).expect("open file");

    assert_eq!(editor.ai_repo_root(), None);
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
fn session_created_temp_file_can_be_rewritten_and_executed_without_more_approval() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let repository = tempfile::tempdir().expect("repository");
        git2::Repository::init(repository.path()).expect("init repository");
        let source = repository.path().join("main.rs");
        fs::write(&source, "fn main() {}\n").expect("seed source");
        let temp = tempfile::tempdir_in(std::env::temp_dir()).expect("session temp parent");
        git2::Repository::init(temp.path()).expect("init unrelated temp repository");
        let script = temp.path().join("agent-script.sh");
        let runs = tempfile::tempdir().expect("run storage");

        let mut editor = Editor::default();
        *editor.ai_state = super::super::ai_state::AiState::with_run_storage_layout(
            crate::run_log::RunStorageLayout::new(runs.path()),
        )
        .expect("isolated run storage");
        editor.open_file(&source).expect("open source");
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        set_active_profile_project_scope(&mut editor);
        editor.ai_state.config.tool_approval_mode = ToolApprovalMode::SensitivePrompt;

        let create = ToolCallInfo {
            id: "create-temp-script".into(),
            name: "create_file".into(),
            arguments: serde_json::json!({
                "path": script.to_string_lossy(),
                "content": "#!/bin/sh\nprintf first",
                "expected_revision": 0,
            }),
        };
        match editor.dispatch_tool_call_with_approval(
            &create,
            Some(&temp.path().canonicalize().expect("canonical temp parent")),
        ) {
            ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                panic!("create failed: {error}")
            }
            ToolDispatchOutcome::ApprovalRequired(request) => {
                panic!("initial one-shot approval was ignored: {}", request.message)
            }
        }
        assert!(editor.current_session_created_temp_file(&script));
        assert_eq!(
            editor
                .ai_repo_root()
                .expect("origin repository remains in scope")
                .canonicalize()
                .expect("canonical detected repository"),
            repository
                .path()
                .canonicalize()
                .expect("canonical repository")
        );

        let rewrite = ToolCallInfo {
            id: "rewrite-temp-script".into(),
            name: "write_file_at_path".into(),
            arguments: serde_json::json!({
                "path": script.to_string_lossy(),
                "content": "#!/bin/sh\nprintf second",
                "expected_revision": editor.build_tool_execution_context().buffer_revision,
            }),
        };
        match editor.dispatch_tool_call_with_approval(&rewrite, None) {
            ToolDispatchOutcome::Completed(ToolResult::Success(_)) => {}
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                panic!("rewrite failed: {error}")
            }
            ToolDispatchOutcome::ApprovalRequired(request) => {
                panic!("owned temp rewrite requested approval: {}", request.message)
            }
        }

        for (id, command) in [
            (
                "chmod-temp-script",
                format!("chmod +x {}", script.display()),
            ),
            ("run-temp-script", script.display().to_string()),
        ] {
            let call = ToolCallInfo {
                id: id.into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": command}),
            };
            match editor.dispatch_tool_call_with_approval(&call, None) {
                ToolDispatchOutcome::Completed(ToolResult::Success(output)) => {
                    if id == "run-temp-script" {
                        assert!(output.contains("second"), "{output}");
                    }
                }
                ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                    panic!("{id} failed: {error}")
                }
                ToolDispatchOutcome::ApprovalRequired(request) => {
                    panic!("{id} requested approval: {}", request.message)
                }
            }
        }

        let unowned = temp.path().join("unowned.sh");
        fs::write(&unowned, "#!/bin/sh\n").expect("seed unowned sibling");
        let call = ToolCallInfo {
            id: "rewrite-unowned-temp".into(),
            name: "write_file_at_path".into(),
            arguments: serde_json::json!({
                "path": unowned.to_string_lossy(),
                "content": "must require approval",
                "expected_revision": 0,
            }),
        };
        assert!(matches!(
            editor.dispatch_tool_call_with_approval(&call, None),
            ToolDispatchOutcome::ApprovalRequired(_)
        ));
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
        let sync_state = editor
            .lsp
            .state
            .document_sync
            .values()
            .next()
            .expect("AI mutation should register document sync state");
        assert!(
            sync_state.should_send_save(),
            "auto-saving an AI mutation must queue textDocument/didSave"
        );
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
        let sync_state = editor
            .lsp
            .state
            .document_sync
            .values()
            .next()
            .expect("AI mutation should register document sync state");
        assert!(
            sync_state.should_send_save(),
            "saving an AI mutation must queue textDocument/didSave"
        );
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
            canonicalize_or_normalize(&target)
                .to_string_lossy()
                .to_string()
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
fn browser_tools_require_an_authorized_live_frontend_service() {
    fn schema_names(editor: &Editor) -> Vec<String> {
        let profile = editor
            .ai_state
            .config
            .resolve_profile(&editor.ai_state.active_profile)
            .expect("active profile");
        editor
            .build_tool_schemas_for_chat(profile)
            .into_iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    let mut unavailable = Editor::default();
    let active = unavailable.ai_state.active_profile.clone();
    unavailable
        .ai_state
        .config
        .profiles
        .get_mut(&active)
        .expect("active profile")
        .scope
        .network = true;
    unavailable
        .open_ai_chat(ChatOpts {
            allow_edits: true,
            ..ChatOpts::default()
        })
        .unwrap();
    assert!(!schema_names(&unavailable)
        .iter()
        .any(|name| crate::ai::tools::browser::is_browser_tool(name)));

    let (browser, _host) = crate::browser::browser_channel();
    let mut available = Editor::default()
        .with_services(crate::editor::EditorServices::default().with_browser(browser));
    let active = available.ai_state.active_profile.clone();
    available
        .ai_state
        .config
        .profiles
        .get_mut(&active)
        .expect("active profile")
        .scope
        .network = true;
    available
        .open_ai_chat(ChatOpts {
            allow_edits: true,
            ..ChatOpts::default()
        })
        .unwrap();
    let names = schema_names(&available);
    for expected in [
        crate::ai::tools::browser::BROWSER_SESSION_TOOL,
        crate::ai::tools::browser::BROWSER_NAVIGATE_TOOL,
        crate::ai::tools::browser::BROWSER_SNAPSHOT_TOOL,
        crate::ai::tools::browser::BROWSER_ACT_TOOL,
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "schemas: {names:?}"
        );
    }
}

#[test]
fn compact_is_advertised_even_with_an_explicit_tool_allowlist() {
    let mut editor = Editor::default();
    editor.open_ai_chat(ChatOpts::default()).expect("open chat");
    let mut profile = editor
        .ai_state
        .config
        .resolve_profile(&editor.ai_state.active_profile)
        .expect("profile")
        .clone();
    profile.tools = vec!["read_file".into()];

    let schemas = editor.build_tool_schemas_for_chat(&profile);
    let names = schemas
        .iter()
        .filter_map(|schema| schema.get("function"))
        .filter_map(|function| function.get("name"))
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert!(names.contains(&"compact"), "schemas: {schemas:?}");
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
    let profile_name = editor.ai_state.active_profile.clone();
    if let Some(profile) = editor.ai_state.config.profiles.get_mut(&profile_name) {
        profile.scope.shell = true;
    }
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
            assert!(output.contains("ai_chat_tools_tests.rs"), "{output}");
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

    for tool_call in [ToolCallInfo {
        id: "call_strok_intro".to_string(),
        name: "strok_vector".to_string(),
        arguments: serde_json::json!({"operation": "intro"}),
    }] {
        match editor.dispatch_tool_call_with_approval(&tool_call, None) {
            ToolDispatchOutcome::Completed(ToolResult::Error(error)) => {
                assert!(
                    !error.contains("No file open."),
                    "{} was incorrectly blocked by the unnamed buffer: {error}",
                    tool_call.name
                );
                assert!(
                    !error.contains("unavailable for the active profile or scope")
                        && !error.contains("requires scope not granted"),
                    "{} was not exposed with project-chat capabilities: {error}",
                    tool_call.name
                );
            }
            ToolDispatchOutcome::Completed(ToolResult::Success(_))
            | ToolDispatchOutcome::ApprovalRequired(_) => {}
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
    let (result_tx, result_rx) =
        tokio::sync::oneshot::channel::<super::super::ai_chat_state::ShellExecutionObservation>();
    drop(result_tx);
    let task = tokio::spawn(async {});
    if let Some(chat) = editor.ai_state.chat.as_mut() {
        chat.pending_shell_execution = Some(super::super::ai_chat_state::PendingShellExecution {
            tool_call: shell_call,
            continuation: super::super::ai_chat_state::ToolExecutionContinuation::Batch {
                runtime_tool: None,
                runtime_turn: None,
                remaining_tool_calls: vec![follow_up],
                model_name: "test".into(),
            },
            receiver: result_rx,
            progress: tokio::sync::mpsc::unbounded_channel().1,
            task,
            kill: std::sync::Arc::new(super::super::ai_chat_state::ShellKillHandle::default()),
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
