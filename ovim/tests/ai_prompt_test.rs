mod helpers;

use helpers::EditorTest;
use ovim::editor::handle_mouse_event;
use ovim::mode::Mode;
use ovim_core::ai::{
    AgentLoopConfig, AiProfileConfig, AiProviderKind, ContextGatheringPolicy, EditFormat,
    RetryPolicy,
};
use ovim_core::{KeyCode, Modifiers, MouseButton, MouseEvent, MouseEventKind, Rect};
use std::time::Instant;

/// Helper to build a test profile with common defaults.
fn test_profile(name: &str, provider: AiProviderKind, model: &str) -> AiProfileConfig {
    AiProfileConfig {
        name: name.to_string(),
        provider,
        model: model.to_string(),
        base_url: None,
        api_key: None,
        api_key_env: None,
        temperature: None,
        max_tokens: None,
        system_prompt: None,
        edit_format: EditFormat::Json,
        chat_edit_format: None,
        context: ContextGatheringPolicy::default(),
        agent_loop: AgentLoopConfig::default(),
        tools: vec![],
        scope: ovim_core::ai::ProfileScope::default(),
        edit_prompt: None,
        chat_prompt: None,
        chat_edit_prompt: None,
        reasoning_effort: None,
        verbosity: None,
        syntax_check: None,
        retry: RetryPolicy::default(),
    }
}

fn generated_region(
    id: u64,
    start_char: usize,
    end_char: usize,
    original_text: &str,
    generated_text: &str,
) -> ovim::editor::AiEditRegion {
    let now = Instant::now();
    ovim::editor::AiEditRegion {
        id,
        start_char,
        end_char,
        status: ovim::editor::AiRegionStatus::Generated,
        prompt: "rewrite".to_string(),
        original_text: original_text.to_string(),
        generated_text: generated_text.to_string(),
        profile_name: "alpha".to_string(),
        provider_label: "ollama/model-a".to_string(),
        edit_format: EditFormat::Json,
        reasoning_lines: vec!["reason".to_string()],
        raw_output: Some("{\"replacement\":\"earth\"}".to_string()),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn test_visual_space_space_attaches_selection_to_chat() {
    let mut test = EditorTest::new("hello world\n");

    test.keys("vll<Space><Space>");

    test.assert_mode(Mode::AiChat);
    let attachment = test
        .editor
        .ai_chat_pending_code_attachment()
        .expect("expected attached code selection");
    assert_eq!(attachment.text, "hel");
    assert_eq!(test.editor.ai_chat_input(), "");
}

#[test]
fn test_visual_line_chat_attachment_keeps_indent_and_trailing_newline() {
    let mut test = EditorTest::new("    one\n    two\nnext\n");

    test.keys("Vj<Space><Space>");

    test.assert_mode(Mode::AiChat);
    let attachment = test
        .editor
        .ai_chat_pending_code_attachment()
        .expect("expected attached code selection");
    assert_eq!(attachment.text, "    one\n    two\n");
}

#[test]
fn test_visual_chat_attachment_preserves_existing_draft() {
    let mut test = EditorTest::new("hello world\n");
    test.keys("<Space><Space>");
    test.type_text("Please explain this");
    test.press_esc();

    test.keys("vll<Space><Space>");

    test.assert_mode(Mode::AiChat);
    assert_eq!(test.editor.ai_chat_input(), "Please explain this");
    assert_eq!(
        test.editor
            .ai_chat_pending_code_attachment()
            .expect("expected attached selection")
            .text,
        "hel"
    );
}

#[test]
fn test_visual_space_ai_hotkey_is_removed() {
    let mut test = EditorTest::new("hello world\n");

    test.keys("vll<Space>ai");

    test.assert_mode(Mode::Visual);
    assert!(test.editor.ai_state.active_selection.is_none());
    assert!(test.editor.ai_chat_state().is_none());
}

#[test]
fn test_normal_space_space_opens_ai_chat_with_chat_context_profile() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.ai_state.config.profiles.clear();

    let mut alpha = test_profile("alpha", AiProviderKind::Ollama, "model-a");
    alpha.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), alpha);

    let mut beta = test_profile("beta", AiProviderKind::Ollama, "model-b");
    beta.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("beta".to_string(), beta);

    test.editor.ai_state.active_profile = "alpha".to_string();
    test.editor
        .ai_state
        .config
        .contexts
        .insert("chat".to_string(), "beta".to_string());

    test.keys("<Space><Space>");

    test.assert_mode(Mode::AiChat);
    assert_eq!(test.editor.ai_state.active_profile, "alpha");
    assert_eq!(test.editor.ai_chat_effective_profile(), "beta");
    assert_eq!(
        test.editor
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.as_deref()),
        Some("beta")
    );
}

#[test]
fn test_ai_prompt_escape_clears_state() {
    let mut test = EditorTest::new("hello world\n");

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.type_text("rewrite it");
    test.press_esc();

    test.assert_mode(Mode::Normal);
    assert_eq!(test.editor.ai_prompt_input(), "");
    assert!(test.editor.ai_state.active_selection.is_none());
}

#[test]
fn test_ai_prompt_arrow_navigation_edits_prompt() {
    let mut test = EditorTest::new("hello world\n");

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.type_text("abc");
    test.press_key(KeyCode::Left);
    test.press('X');
    test.press_key(KeyCode::Home);
    test.press('!');
    test.press_key(KeyCode::End);
    test.press('?');

    assert_eq!(test.editor.ai_prompt_input(), "!abXc?");
}

#[test]
fn test_ai_lock_blocks_inside_and_allows_outside_edits() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.buffer_mut().add_ai_lock(1, 6, 11); // lock "world"

    test.set_cursor(0, 7);
    test.keys("ix<Esc>");
    assert_eq!(test.buffer_content(), "hello world\n");
    assert_eq!(
        test.editor.status_message(),
        "AI lock active for selected region"
    );

    test.set_cursor(0, 0);
    test.keys("iX<Esc>");
    assert_eq!(test.buffer_content(), "Xhello world\n");
}

#[test]
fn test_o_at_ai_lock_end_boundary_opens_new_line() {
    editor_flow_test! {
        content "fn main() {\n    do_work();\n}\nlet after = 1;\n";
        setup |test| {
            let start = test.editor.buffer().rope().line_to_char(0);
            let end = test.editor.buffer().rope().line_to_char(3);
            test.editor.buffer_mut().add_ai_lock(11, start, end);
            test.set_cursor(2, 0);
        }
        step "o" => |test| {
            test.assert_mode(Mode::Insert);
            assert_eq!(
                test.buffer_content(),
                "fn main() {\n    do_work();\n}\n\nlet after = 1;\n"
            );
            test.assert_cursor(3, 0);
        }
        step "<Esc>" => |test| {
            test.assert_mode(Mode::Normal);
        }
    }
}

#[test]
fn test_o_blocked_by_ai_lock_stays_normal_mode() {
    editor_flow_test! {
        content "alpha\nbeta\ngamma\n";
        setup |test| {
            let start = test.editor.buffer().rope().line_to_char(0);
            let end = test.editor.buffer().rope().line_to_char(3);
            test.editor.buffer_mut().add_ai_lock(12, start, end);
            test.set_cursor(1, 0);
        }
        step "o" => |test| {
            test.assert_mode(Mode::Normal);
            assert_eq!(test.buffer_content(), "alpha\nbeta\ngamma\n");
            assert_eq!(test.editor.status_message(), "AI lock active for selected region");
        }
    }
}

#[test]
fn test_blocked_ai_lock_insert_does_not_pollute_undo_history() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.buffer_mut().add_ai_lock(13, 6, 11); // lock "world"

    test.keys("iX<Esc>");
    assert_eq!(test.buffer_content(), "Xhello world\n");

    test.set_cursor(0, 7);
    test.keys("ix<Esc>");
    assert_eq!(test.buffer_content(), "Xhello world\n");

    test.keys("u");
    assert_eq!(test.buffer_content(), "hello world\n");
}

#[test]
fn test_visual_block_delete_mixed_ai_lock_lines_keeps_undo_history() {
    let mut test = EditorTest::new("aa_bb_cc\ndd_ee_ff\ngg_hh_ii\n");

    // Lock "bb" on the first line. Visual-block delete should still delete
    // unlocked rows and remain undoable as one operation.
    let lock_start = test.editor.buffer().rope().line_to_char(0) + 3;
    let lock_end = lock_start + 2;
    test.editor
        .buffer_mut()
        .add_ai_lock(14, lock_start, lock_end);

    test.keys("3l<C-v>jjlx");

    assert_eq!(test.buffer_content(), "aa_bb_cc\ndd__ff\ngg__ii\n");
    assert_eq!(
        test.editor.status_message(),
        "AI lock active for selected region"
    );

    test.keys("u");
    assert_eq!(test.buffer_content(), "aa_bb_cc\ndd_ee_ff\ngg_hh_ii\n");
}

#[tokio::test(flavor = "current_thread")]
async fn test_ai_prompt_submit_creates_lock_and_returns_to_normal() {
    let mut test = EditorTest::new("hello world\n");

    test.editor.ai_state.active_profile = "test".to_string();
    test.editor.ai_state.config.profiles.clear();
    let mut profile = test_profile("test", AiProviderKind::OpenAi, "gpt-4o-mini");
    profile.api_key_env = Some("OVIM_TEST_AI_KEY_MISSING".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("test".to_string(), profile);

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.type_text("replace with short word");
    test.press_enter();

    test.assert_mode(Mode::Normal);
    assert_eq!(test.editor.buffer().ai_locks().len(), 1);
    assert_eq!(test.editor.ai_state.pending_jobs.len(), 1);
    assert!(test.editor.ai_state.regions[0]
        .reasoning_lines
        .iter()
        .any(|line| line.contains("context:")));
    assert!(test.editor.ai_state.regions[0]
        .reasoning_lines
        .iter()
        .any(|line| line.contains("waiting for model response")));
}

#[tokio::test(flavor = "current_thread")]
async fn test_ai_prompt_submit_applies_context_budget_trace() {
    let mut test = EditorTest::new("line one\nline two\nline three\n");

    test.editor.ai_state.active_profile = "budget".to_string();
    test.editor.ai_state.config.profiles.clear();
    let mut profile = test_profile("budget", AiProviderKind::OpenAi, "gpt-4o-mini");
    profile.api_key_env = Some("OVIM_TEST_AI_KEY_MISSING".to_string());
    profile.context = ContextGatheringPolicy {
        budget: 1,
        ..ContextGatheringPolicy::default()
    };
    test.editor
        .ai_state
        .config
        .profiles
        .insert("budget".to_string(), profile);

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.type_text("rewrite");
    test.press_enter();

    let trace = &test.editor.ai_state.regions[0].reasoning_lines;
    assert!(trace.iter().any(|line| line.contains("context:")));
    assert!(trace
        .iter()
        .any(|line| line.contains("context pruning applied")));
    assert!(trace
        .iter()
        .any(|line| line.contains("context estimate after pruning")));
}

#[test]
fn test_ai_prompt_keyboard_model_picker_cycles_profiles() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.ai_state.config.profiles.clear();
    let mut alpha = test_profile("alpha", AiProviderKind::Ollama, "model-a");
    alpha.base_url = Some("http://127.0.0.1:11434".to_string());
    alpha.edit_format = EditFormat::Json;
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), alpha);

    let mut beta = test_profile("beta", AiProviderKind::Ollama, "model-b");
    beta.base_url = Some("http://127.0.0.1:11434".to_string());
    beta.edit_format = EditFormat::Codeblock;
    test.editor
        .ai_state
        .config
        .profiles
        .insert("beta".to_string(), beta);

    test.editor.ai_state.active_profile = "alpha".to_string();
    test.editor.ai_state.edit_format = EditFormat::Json;

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.press_key(KeyCode::Tab);
    assert_eq!(test.editor.ai_state.active_profile, "beta");
    assert_eq!(test.editor.ai_state.edit_format, EditFormat::Codeblock);

    test.press_key(KeyCode::BackTab);
    assert_eq!(test.editor.ai_state.active_profile, "alpha");
    assert_eq!(test.editor.ai_state.edit_format, EditFormat::Json);

    test.press_key(KeyCode::Down);
    assert_eq!(test.editor.ai_state.active_profile, "beta");
    test.press_key(KeyCode::Up);
    assert_eq!(test.editor.ai_state.active_profile, "alpha");
}

#[test]
fn test_ai_chat_model_selector_cycle_updates_chat_profile() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.ai_state.config.profiles.clear();

    let mut alpha = test_profile("alpha", AiProviderKind::Ollama, "model-a");
    alpha.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), alpha);

    let mut beta = test_profile("beta", AiProviderKind::Ollama, "model-b");
    beta.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("beta".to_string(), beta);

    test.editor.ai_state.active_profile = "alpha".to_string();
    test.editor
        .ai_state
        .config
        .contexts
        .insert("chat".to_string(), "alpha".to_string());

    test.keys("<Space><Space>");
    assert_eq!(test.editor.ai_chat_effective_profile(), "alpha");

    test.type_text("/model");
    test.press_key(KeyCode::Enter);
    test.press_key(KeyCode::Right);

    assert_eq!(test.editor.ai_state.active_profile, "beta");
    assert_eq!(test.editor.ai_chat_effective_profile(), "beta");
    assert_eq!(
        test.editor
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.as_deref()),
        Some("beta")
    );
}

#[test]
fn test_ai_prompt_mouse_model_picker_selects_profile() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.ai_state.config.profiles.clear();
    let mut alpha = test_profile("alpha", AiProviderKind::Ollama, "model-a");
    alpha.base_url = Some("http://127.0.0.1:11434".to_string());
    alpha.edit_format = EditFormat::Json;
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), alpha);

    let mut beta = test_profile("beta", AiProviderKind::Ollama, "model-b");
    beta.base_url = Some("http://127.0.0.1:11434".to_string());
    beta.edit_format = EditFormat::Codeblock;
    test.editor
        .ai_state
        .config
        .profiles
        .insert("beta".to_string(), beta);

    test.editor.ai_state.active_profile = "alpha".to_string();
    test.editor.ai_state.edit_format = EditFormat::Json;

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.editor.ai_state.prompt.model_picker_open = true;
    test.editor.render_cache.ai_prompt_model_hitboxes = vec![
        (
            Rect {
                x: 10,
                y: 20,
                width: 8,
                height: 1,
            },
            "alpha".to_string(),
        ),
        (
            Rect {
                x: 19,
                y: 20,
                width: 8,
                height: 1,
            },
            "beta".to_string(),
        ),
    ];

    handle_mouse_event(
        &mut test.editor,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 20,
        },
    )
    .expect("mouse click should be handled");

    assert_eq!(test.editor.ai_state.active_profile, "beta");
    assert_eq!(test.editor.ai_state.edit_format, EditFormat::Codeblock);
    assert!(!test.editor.ai_state.prompt.model_picker_open);
}

#[test]
fn test_ai_prompt_mouse_model_picker_trigger_toggles_open_state() {
    let mut test = EditorTest::new("hello world\n");
    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.editor.render_cache.ai_prompt_model_trigger_hitbox = Some(Rect {
        x: 10,
        y: 20,
        width: 16,
        height: 1,
    });

    handle_mouse_event(
        &mut test.editor,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 20,
        },
    )
    .expect("mouse click should open picker");
    assert!(test.editor.ai_state.prompt.model_picker_open);

    handle_mouse_event(
        &mut test.editor,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 20,
        },
    )
    .expect("mouse click should close picker");
    assert!(!test.editor.ai_state.prompt.model_picker_open);
}

#[test]
fn test_ai_prompt_enter_applies_open_picker_selection_instead_of_submitting() {
    let mut test = EditorTest::new("hello world\n");
    test.editor.ai_state.config.profiles.clear();
    let mut alpha = test_profile("alpha", AiProviderKind::Ollama, "model-a");
    alpha.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), alpha);
    let mut beta = test_profile("beta", AiProviderKind::Ollama, "model-b");
    beta.base_url = Some("http://127.0.0.1:11434".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("beta".to_string(), beta);

    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.editor.ai_state.prompt.input = "rewrite".to_string();
    test.editor.ai_state.prompt.cursor = 7;
    test.editor.ai_state.prompt.model_picker_open = true;
    test.editor.ai_state.prompt.model_picker_index = 1;
    test.press_enter();

    assert_eq!(test.editor.ai_state.active_profile, "beta");
    assert!(!test.editor.ai_state.prompt.model_picker_open);
    assert_eq!(test.editor.ai_state.pending_jobs.len(), 0);
}

#[test]
fn test_ai_prompt_ctrl_m_toggles_picker_and_esc_closes_picker_first() {
    let mut test = EditorTest::new("hello world\n");
    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.press_with(KeyCode::Char('m'), Modifiers::CONTROL);
    assert!(test.editor.ai_state.prompt.model_picker_open);

    test.press_esc();
    test.assert_mode(Mode::AiPrompt);
    assert!(!test.editor.ai_state.prompt.model_picker_open);
}

#[test]
fn test_ai_prompt_mouse_click_sets_cursor_on_wrapped_rows() {
    let mut test = EditorTest::new("hello world\n");
    test.keys("vll");
    test.editor.start_ai_prompt_from_visual().unwrap();
    test.editor.ai_state.prompt.input = "abcdefghij".to_string();
    test.editor.ai_state.prompt.cursor = 0;
    test.editor.render_cache.ai_prompt_input_rows = vec![
        (
            Rect {
                x: 20,
                y: 20,
                width: 3,
                height: 1,
            },
            0,
            3,
        ),
        (
            Rect {
                x: 10,
                y: 21,
                width: 4,
                height: 1,
            },
            3,
            7,
        ),
        (
            Rect {
                x: 10,
                y: 22,
                width: 4,
                height: 1,
            },
            7,
            10,
        ),
    ];

    handle_mouse_event(
        &mut test.editor,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 21,
        },
    )
    .expect("mouse click on wrapped row should be handled");
    assert_eq!(test.editor.ai_state.prompt.cursor, 5);

    handle_mouse_event(
        &mut test.editor,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 13,
            row: 22,
        },
    )
    .expect("mouse click on final wrapped row should be handled");
    assert_eq!(test.editor.ai_state.prompt.cursor, 10);
}

#[test]
fn test_ctrl_y_accepts_generated_ai_region() {
    let mut test = EditorTest::new("hello earth\n");
    test.editor
        .ai_state
        .regions
        .push(generated_region(1, 6, 11, "world", "earth"));
    test.editor
        .buffer_mut()
        .add_ai_lock_with_mode(1, 6, 11, false);
    test.editor.ai_state.selected_region_id = Some(1);

    test.keys("<C-y>");

    assert!(test.editor.ai_state.regions.is_empty());
    assert!(test.editor.buffer().ai_locks().is_empty());
    assert_eq!(test.editor.ai_selected_region_id(), None);
}

#[test]
fn test_ctrl_n_reverts_generated_ai_region() {
    let mut test = EditorTest::new("hello earth\n");
    test.editor
        .ai_state
        .regions
        .push(generated_region(1, 6, 11, "world", "earth"));
    test.editor
        .buffer_mut()
        .add_ai_lock_with_mode(1, 6, 11, false);
    test.editor.ai_state.selected_region_id = Some(1);

    test.keys("<C-n>");

    assert_eq!(test.buffer_content(), "hello world\n");
    assert!(test.editor.ai_state.regions.is_empty());
    assert!(test.editor.buffer().ai_locks().is_empty());
}

#[test]
fn test_ctrl_e_shows_ai_reasoning_for_selected_region() {
    let mut test = EditorTest::new("hello earth\n");
    test.editor
        .ai_state
        .regions
        .push(generated_region(1, 6, 11, "world", "earth"));
    test.editor
        .buffer_mut()
        .add_ai_lock_with_mode(1, 6, 11, false);
    test.editor.ai_state.selected_region_id = Some(1);

    test.keys("<C-e>");

    assert_eq!(test.mode(), Mode::HoverPreview);
    assert_eq!(
        test.editor.hover_content_type(),
        ovim::editor::HoverContentType::AiReasoning
    );
    let hover = test.editor.hover_info().unwrap_or_default();
    assert!(hover.contains("AI Edit Details"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_ctrl_space_retries_generation_for_selected_region() {
    let mut test = EditorTest::new("hello earth\n");
    test.editor.ai_state.config.profiles.clear();
    let mut profile = test_profile("alpha", AiProviderKind::OpenAi, "gpt-4o-mini");
    profile.api_key_env = Some("OVIM_TEST_AI_KEY_MISSING".to_string());
    test.editor
        .ai_state
        .config
        .profiles
        .insert("alpha".to_string(), profile);

    test.editor
        .ai_state
        .regions
        .push(generated_region(1, 6, 11, "world", "earth"));
    test.editor
        .buffer_mut()
        .add_ai_lock_with_mode(1, 6, 11, false);
    test.editor.ai_state.selected_region_id = Some(1);

    test.keys("<C- >");

    assert_eq!(test.editor.ai_state.pending_jobs.len(), 1);
    assert_eq!(
        test.editor.ai_state.regions[0].status,
        ovim::editor::AiRegionStatus::Running
    );
    assert!(test.editor.ai_state.regions[0]
        .reasoning_lines
        .iter()
        .any(|line| line.contains("context:")));
    assert!(test.editor.ai_state.regions[0]
        .reasoning_lines
        .iter()
        .any(|line| line.contains("retrying with same prompt")));
    assert_eq!(test.editor.buffer().ai_locks().len(), 1);
    assert!(test.editor.buffer().ai_locks()[0].blocks_edits);
}

#[test]
fn test_editing_generated_region_removes_ai_metadata() {
    let mut test = EditorTest::new("hello earth\n");
    test.editor
        .ai_state
        .regions
        .push(generated_region(1, 6, 11, "world", "earth"));
    test.editor
        .buffer_mut()
        .add_ai_lock_with_mode(1, 6, 11, false);

    test.set_cursor(0, 8);
    test.keys("iX<Esc>");

    assert_eq!(test.buffer_content(), "hello eaXrth\n");
    assert!(test.editor.ai_state.regions.is_empty());
    assert!(test.editor.buffer().ai_locks().is_empty());
}
