use anyhow::Result;
use crossterm::event::{self, Event, EventStream};
use futures::StreamExt;
use ovim::key_convert::{convert_key_event, convert_mouse_event};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration, Instant};

use ovim::api::ApiRequest;
use ovim::buffer::{BufferId, LineHighlights};
use ovim::editor::{self, handle_mouse_event, Editor, InputHandler};
use ovim::mode::Mode;
use ovim::session::SessionInfo;
use ovim::syntax::{Language, LanguageRegistry, SyntaxHighlighter};
use ovim::ui::UI;

fn emit_agent_attention_bell(output: &mut impl Write) -> io::Result<()> {
    output.write_all(b"\x07")?;
    output.flush()
}

fn emit_new_agent_attention(
    current: u64,
    observed_generation: &mut u64,
    output: &mut impl Write,
) -> io::Result<bool> {
    if current == *observed_generation {
        return Ok(false);
    }
    *observed_generation = current;
    emit_agent_attention_bell(output)?;
    Ok(true)
}

fn notify_new_agent_attention(editor: &Editor, observed_generation: &mut u64) {
    let mut stdout = io::stdout().lock();
    let _ = emit_new_agent_attention(
        editor.ai_chat_attention_generation(),
        observed_generation,
        &mut stdout,
    );
}

fn apply_java_status(editor: &mut Editor, status: String) {
    let ready = status.trim().ends_with(": Ready");
    editor.set_lsp_status(status);
    if ready {
        editor.request_diagnostics_refresh();
    }
}

/// Shared editor tick for both loops.
/// Handles LSP, diagnostics, syntax, DAP, and background tasks.
async fn process_editor_tick(
    editor: &mut Editor,
    java_status_rx: &mut mpsc::Receiver<String>,
    preview_tx: &tokio::sync::mpsc::Sender<(String, editor::PreviewCache)>,
    file_tx: &tokio::sync::mpsc::Sender<editor::PickerResult>,
    syntax_tx: &tokio::sync::mpsc::Sender<(BufferId, Language, Option<LineHighlights>, u64)>,
    syntax_rx: &mut tokio::sync::mpsc::Receiver<(BufferId, Language, Option<LineHighlights>, u64)>,
) {
    // === LSP lifecycle ===
    process_java_status(editor, java_status_rx);
    process_lsp_notifications(editor).await;
    process_lsp_init(editor).await;
    process_lsp_sync_and_inlay_hints(editor).await;

    // === Debug adapter ===
    process_dap_events(editor);
    process_pending_debug_action(editor).await;

    // === Syntax highlighting ===
    spawn_syntax_highlighting(editor, syntax_tx);
    drain_syntax_results(editor, syntax_rx);

    // === LSP responses & intents ===
    if editor.poll_pending_lsp_responses() {
        editor.mark_dirty();
    }
    editor.dispatch_pending_intents().await;

    // === Background tasks ===
    poll_background_tasks(editor).await;
    update_file_list_cache_from_background(editor);

    // === Transient UI state ===
    tick_transient_ui(editor);

    // === Lua ===
    let _ = editor.process_lua_commands();

    // === LSP installs ===
    spawn_pending_installs(editor);
    if editor.poll_install_progress() {
        editor.mark_dirty();
    }

    // === Picker ===
    if editor.mode() == Mode::Picker {
        process_picker_tick(editor, preview_tx, file_tx);
    }

    // File switches queue didClose outside the async input dispatcher. Drive
    // that lifecycle from the shared tick so headless and TUI sessions agree.
    editor.send_lsp_close_if_needed().await;
}

fn tick_transient_ui(editor: &mut Editor) {
    if editor.tick_cat_animation()
        | editor.tick_yank_flash()
        | editor.tick_toasts()
        | editor.tick_ai_chat_working_animation()
        | editor.tick_ai_chat_text_selection_autoscroll()
        | editor.poll_ai_subagent_repaint()
    {
        editor.mark_dirty();
    }
}

/// Shared post-input refresh used by terminal and API input paths.
pub(crate) fn refresh_after_input(editor: &mut Editor) {
    if editor.buffer().needs_rehighlight() {
        editor.process_viewport_rehighlight();
    }
    editor.mark_dirty();
}

/// Shared post-mutation refresh for API endpoints that bypass key dispatch.
pub(crate) fn refresh_after_api_mutation(editor: &mut Editor, force_full_lsp_sync: bool) {
    if force_full_lsp_sync {
        editor.mark_buffer_modified_force_send();
    }
    editor.request_diagnostics_refresh();
    refresh_after_input(editor);
}

/// Reload a clean buffer after an external write, but never discard local
/// edits. This is shared by focus events and periodic polling so headless
/// sessions have the same file-change behavior as the TUI.
fn process_external_file_change(editor: &mut Editor) {
    match editor.buffer().check_external_modification() {
        Ok(false) | Err(_) => {}
        Ok(true) if editor.is_modified() => {
            let status = "File changed on disk; local changes were kept (use :e! to reload)";
            if editor.status_message() != status {
                editor.set_status_message(status);
                editor.mark_dirty();
            }
        }
        Ok(true) => match editor.buffer_mut().reload_if_changed_sync() {
            Ok(true) => {
                editor.mark_saved();
                editor.mark_buffer_modified_force_send();
                editor.request_diagnostics_refresh();
                if editor.buffer().needs_rehighlight() {
                    editor.process_viewport_rehighlight();
                }
                editor.set_status_message("File reloaded after external change");
                editor.mark_dirty();
            }
            Ok(false) => {}
            Err(error) => {
                editor.set_status_message(format!("External file change: {error}"));
                editor.mark_dirty();
            }
        },
    }
}

/// Drain Java/Kotlin LSP status messages from the channel.
fn process_java_status(editor: &mut Editor, java_status_rx: &mut mpsc::Receiver<String>) {
    while let Ok(status) = java_status_rx.try_recv() {
        apply_java_status(editor, status);
    }
}

/// Process LSP notifications and server-initiated workspace edits.
async fn process_lsp_notifications(editor: &mut Editor) {
    if let Some(lsp_manager) = editor.lsp_manager() {
        let notification_count = lsp_manager.process_notifications().await;
        let flush_count = lsp_manager.process_flush_requests().await;

        if notification_count > 0 || flush_count > 0 {
            ovim_core::log_debug!(
                "tick",
                "LSP: {} notifications, {} flushes",
                notification_count,
                flush_count
            );
            editor.mark_dirty();
        }

        let pending_edits = lsp_manager.poll_pending_workspace_edits().await;
        for workspace_edit in pending_edits {
            ovim_core::log_debug!("tick", "Applying workspace edit from LSP server");
            match editor.apply_workspace_edit(workspace_edit) {
                Ok(applied) => {
                    if applied {
                        editor.set_lsp_status("Applied workspace edit".to_string());
                    } else {
                        editor.set_lsp_status("Partially applied workspace edit".to_string());
                    }
                }
                Err(e) => {
                    ovim_core::log_error!("tick", "Failed to apply workspace edit: {}", e);
                    editor.set_lsp_status(format!("Failed to apply edit: {}", e));
                }
            }
            editor.mark_dirty();
        }
    }
}

/// Initialize LSP for a newly opened file if needed.
async fn process_lsp_init(editor: &mut Editor) {
    if let Some(file_path) = editor.needs_lsp_init() {
        ovim_core::log_debug!("tick", "Initializing LSP for {}", file_path);
        crate::lsp_init::initialize_lsp_for_file(editor, &file_path).await;
        editor.clear_lsp_init_flag();
    }
}

/// Sync edits to the LSP server, refresh diagnostics, and poll inlay hints.
/// Colocated to enforce: server always has latest content before we check for fresh diagnostics.
async fn process_lsp_sync_and_inlay_hints(editor: &mut Editor) {
    if editor.sync_lsp_and_refresh_diagnostics().await {
        editor.mark_dirty();
    }
    if let Some(_lsp_manager) = editor.lsp_manager() {
        if editor.poll_pending_inlay_hint_response() {
            editor.mark_dirty();
        }
        if editor.inlay_hints_refresh_needed() {
            editor.request_inlay_hints_refresh().await;
        }
    }
}

/// Poll DAP events and auto-fetch stack trace on stop.
fn process_dap_events(editor: &mut Editor) {
    let dap_count = editor.process_dap_events();
    if dap_count > 0 {
        ovim_core::log_debug!("tick", "Processed {} DAP events", dap_count);
        editor.mark_dirty();
        if editor.debug_state().stopped_thread.is_some()
            && editor.debug_state().stack_frames.is_empty()
        {
            editor.dap_manager_mut().pending_action =
                Some(ovim::dap::PendingDebugAction::FetchState);
        }
    }
}

/// Dispatch the pending debug action (start, stop, step, evaluate, etc.).
async fn process_pending_debug_action(editor: &mut Editor) {
    let Some(action) = editor.dap_manager_mut().pending_action.take() else {
        return;
    };

    use ovim::dap::PendingDebugAction;
    match action {
        PendingDebugAction::Start {
            command,
            args,
            run_config,
        } => {
            editor.dap_manager_mut().run_config = run_config;
            if let Err(e) = editor.start_debug_session(&command, &args).await {
                editor.set_status_message(format!("Debug start failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::Stop => {
            if let Err(e) = editor.stop_debug_session().await {
                editor.set_status_message(format!("Debug stop failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::Continue => {
            if let Err(e) = editor.debug_continue().await {
                editor.set_status_message(format!("Debug continue failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::StepOver => {
            if let Err(e) = editor.debug_step_over().await {
                editor.set_status_message(format!("Debug step failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::StepIn => {
            if let Err(e) = editor.debug_step_in().await {
                editor.set_status_message(format!("Debug step in failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::StepOut => {
            if let Err(e) = editor.debug_step_out().await {
                editor.set_status_message(format!("Debug step out failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::LaunchOrAttach => {
            process_dap_launch_or_attach(editor).await;
        }
        PendingDebugAction::SyncBreakpoints => {
            let paths: Vec<std::path::PathBuf> =
                editor.debug_state().breakpoints.keys().cloned().collect();
            for path in &paths {
                let _ = editor.debug_sync_breakpoints(path).await;
            }
            if let Err(e) = editor.dap_manager_mut().configuration_done().await {
                editor.set_status_message(format!("configurationDone failed: {e}"));
            }
            editor.mark_dirty();
        }
        PendingDebugAction::FetchState => {
            let _ = editor.debug_fetch_stack_trace().await;
            let _ = editor.debug_fetch_scopes().await;
            let scope_refs: Vec<u64> = editor
                .debug_state()
                .scopes
                .iter()
                .filter(|s| !s.expensive)
                .map(|s| s.variables_reference)
                .collect();
            for var_ref in scope_refs {
                let _ = editor.debug_fetch_variables(var_ref).await;
            }
            let expanded: Vec<u64> = editor.debug_state().expanded_refs.iter().copied().collect();
            for var_ref in expanded {
                let _ = editor.debug_fetch_variables(var_ref).await;
            }
            editor.mark_dirty();
        }
        PendingDebugAction::SelectFrame { index: _ } => {
            let _ = editor.debug_fetch_scopes().await;
            let scope_refs: Vec<u64> = editor
                .debug_state()
                .scopes
                .iter()
                .filter(|s| !s.expensive)
                .map(|s| s.variables_reference)
                .collect();
            for var_ref in scope_refs {
                let _ = editor.debug_fetch_variables(var_ref).await;
            }
            editor.mark_dirty();
        }
        PendingDebugAction::Evaluate { expression } => {
            let frame_id = editor.selected_frame_id();
            match editor
                .dap_manager()
                .evaluate(&expression, frame_id, Some("hover"))
                .await
            {
                Ok((result, _type, _var_ref)) => {
                    editor.set_status_message(format!("{expression} = {result}"));
                }
                Err(e) => {
                    editor.set_status_message(format!("Eval error: {e}"));
                }
            }
            editor.mark_dirty();
        }
        PendingDebugAction::FetchVariables { var_ref } => {
            let _ = editor.debug_fetch_variables(var_ref).await;
            editor.mark_dirty();
        }
        PendingDebugAction::FetchRunConfigs => {
            process_dap_fetch_run_configs(editor).await;
        }
    }
}

/// Handle DAP launch/attach based on the stored run config.
async fn process_dap_launch_or_attach(editor: &mut Editor) {
    use ovim::dap::PendingDebugAction;
    use ovim::debug_config::DebugRunKind;

    let result = if let Some(run_cfg) = editor.dap_manager_mut().run_config.clone() {
        let default_root = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .to_string_lossy()
            .to_string();
        match run_cfg.kind {
            DebugRunKind::Gradle {
                task,
                args,
                project_root,
            } => {
                let root = project_root.unwrap_or_else(|| default_root.clone());
                editor.set_status_message(format!("Running gradle {task} --debug-jvm..."));
                editor.mark_dirty();
                match spawn_gradle_and_wait(&task, &args, &root).await {
                    Ok(child) => {
                        editor.dap_manager_mut().gradle_child = Some(child);
                        let attach_config = serde_json::json!({
                            "host": "127.0.0.1",
                            "port": 5005,
                            "projectRoot": root,
                        });
                        editor.dap_manager_mut().attach(attach_config).await
                    }
                    Err(e) => Err(e),
                }
            }
            DebugRunKind::Attach {
                host,
                port,
                project_root,
            } => {
                let root = project_root.unwrap_or(default_root);
                let attach_cfg = serde_json::json!({
                    "host": host,
                    "port": port,
                    "projectRoot": root,
                });
                editor.dap_manager_mut().attach(attach_cfg).await
            }
            DebugRunKind::Launch {
                main_class,
                classpath,
                args,
                jvm_args,
                cwd,
                project_root,
            } => {
                let root = project_root.unwrap_or(default_root);
                let mut launch_cfg = serde_json::json!({
                    "mainClass": main_class,
                    "projectRoot": root,
                });
                if let Some(cp) = classpath {
                    launch_cfg["classpath"] = serde_json::json!(cp);
                }
                if !args.is_empty() {
                    launch_cfg["args"] = serde_json::json!(args);
                }
                if !jvm_args.is_empty() {
                    launch_cfg["jvmArgs"] = serde_json::json!(jvm_args);
                }
                if let Some(cwd) = cwd {
                    launch_cfg["cwd"] = serde_json::json!(cwd);
                }
                editor.dap_manager_mut().launch(launch_cfg).await
            }
        }
    } else {
        Ok(())
    };

    match result {
        Ok(()) => {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::SyncBreakpoints);
        }
        Err(e) => {
            editor.set_status_message(format!("Debug launch/attach failed: {e}"));
        }
    }
    editor.mark_dirty();
}

/// Fetch debug run configs from TOML and LSP, then start or open picker.
async fn process_dap_fetch_run_configs(editor: &mut Editor) {
    use ovim::dap::PendingDebugAction;

    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let mut configs = ovim::debug_config::load_debug_configs(&project_root);

    if let Some(lsp_manager) = editor.lsp_manager() {
        let lsp_configs = lsp_manager.run_configurations().await;
        configs.extend(ovim::debug_config::parse_lsp_run_configs(&lsp_configs));
    }

    editor.clear_status_message();

    if configs.is_empty() {
        let dap_start = editor
            .buffer()
            .file_path()
            .and_then(|fp| {
                ovim::language_config::LanguageRegistry::try_get().and_then(|reg| reg.detect(fp))
            })
            .and_then(|lang| lang.dap.as_ref())
            .and_then(|config| {
                ovim::language_config::find_dap_command(config)
                    .map(|cmd| (cmd, config.args.clone()))
            });
        if let Some((command, args)) = dap_start {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Start {
                command,
                args,
                run_config: None,
            });
        } else {
            editor.set_status_message(
                "No debug configs found. Create .ovim/debug.toml or configure a DAP adapter.",
            );
        }
    } else if configs.len() == 1 {
        let config = configs.into_iter().next().unwrap();
        let dap_start = editor
            .buffer()
            .file_path()
            .and_then(|fp| {
                ovim::language_config::LanguageRegistry::try_get().and_then(|reg| reg.detect(fp))
            })
            .and_then(|lang| lang.dap.as_ref())
            .and_then(|dap_config| {
                ovim::language_config::find_dap_command(dap_config)
                    .map(|cmd| (cmd, dap_config.args.clone()))
            });
        if let Some((command, args)) = dap_start {
            editor.dap_manager_mut().pending_action = Some(PendingDebugAction::Start {
                command,
                args,
                run_config: Some(config),
            });
        }
    } else {
        let names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
        editor.dap_manager_mut().available_debug_configs = configs;
        let picker = ovim::editor::picker::Picker::new_debug_config(project_root, names);
        editor.set_picker(picker);
        editor.set_mode(ovim::mode::Mode::Picker);
        editor.mark_picker_selection_changed();
    }
    editor.mark_dirty();
}

/// Spawn background syntax highlighting if the buffer needs it.
fn spawn_syntax_highlighting(
    editor: &mut Editor,
    syntax_tx: &tokio::sync::mpsc::Sender<(BufferId, Language, Option<LineHighlights>, u64)>,
) {
    if !editor.buffer().should_init_syntax() {
        return;
    }
    let buf = editor.buffer();
    let buffer_id = buf.id();
    let source = buf.rope().to_string();
    let version = buf.highlight_version();
    if let Some(path) = buf.file_path() {
        if let Some(lang) = LanguageRegistry::detect_from_path(path) {
            editor.buffer_mut().mark_syntax_loading();
            let tx = syntax_tx.clone();
            tokio::task::spawn_blocking(move || {
                let highlights = if let Ok(mut h) = SyntaxHighlighter::new(lang) {
                    h.parse(&source);
                    Some(h.highlights_for_all_lines(&source))
                } else {
                    None
                };
                let _ = tx.blocking_send((buffer_id, lang, highlights, version));
            });
        } else if buf
            .language_catalog()
            .detect(path)
            .and_then(|language| language.syntax.clone())
            .is_some()
        {
            // Plugin parsers are already validated at startup. Keep the v1
            // handoff simple and initialize them on first display.
            editor.buffer_mut().enable_syntax_highlighting();
        }
    }
}

/// Drain completed background syntax results into buffers.
fn drain_syntax_results(
    editor: &mut Editor,
    syntax_rx: &mut tokio::sync::mpsc::Receiver<(BufferId, Language, Option<LineHighlights>, u64)>,
) {
    while let Ok((buffer_id, lang, highlights, version)) = syntax_rx.try_recv() {
        let is_current = editor.buffer().id() == buffer_id;
        if let Some(buffer) = editor.get_buffer_by_id_mut(buffer_id) {
            let applied = if let Some(highlights) = highlights {
                buffer.apply_background_syntax(lang, highlights, version)
            } else {
                buffer.clear_syntax_loading();
                false
            };

            if is_current && applied {
                editor.mark_dirty();
            }
        }
    }
}

/// Poll all independent background tasks (AI, make, git, chat, workflows).
async fn poll_background_tasks(editor: &mut Editor) {
    if let Some(url) = editor.take_pending_external_url() {
        let _ = open::that_in_background(&url);
    }
    if editor.poll_pending_codex_auth() {
        editor.mark_dirty();
    }
    if editor.poll_pending_ai_jobs() {
        editor.mark_dirty();
    }
    if editor.poll_pending_make() {
        editor.mark_dirty();
    }
    if editor.poll_git_refresh() {
        editor.mark_dirty();
    }
    if editor.has_approved_lsp_install() {
        crate::lsp_init::handle_approved_lsp_install(editor).await;
        editor.mark_dirty();
    }
    if editor.poll_pending_ai_chat_job() {
        editor.mark_dirty();
    }
    if editor.poll_pending_workflow_jobs() {
        editor.mark_dirty();
    }
}

/// Drive the picker: nucleo matching, grep drain, debounced filter, preview/file loading.
fn process_picker_tick(
    editor: &mut Editor,
    preview_tx: &tokio::sync::mpsc::Sender<(String, editor::PreviewCache)>,
    file_tx: &tokio::sync::mpsc::Sender<editor::PickerResult>,
) {
    let mut picker_changed = false;
    if let Some(picker) = editor.picker_mut() {
        if picker.tick() {
            picker_changed = true;
        }
        if picker.drain_grep_results() {
            picker_changed = true;
        }
    }
    if picker_changed {
        editor.mark_dirty();
    }
    if editor.apply_pending_picker_filter(50) {
        editor.mark_dirty();
    }
    spawn_picker_preview_loading(editor, preview_tx);
    spawn_file_finder_loading(editor, file_tx);
    if editor.picker_rapid_scrolling_just_stopped() {
        editor.mark_dirty();
    }
}

/// Spawn background tasks for pending LSP install requests
fn spawn_pending_installs(editor: &mut Editor) {
    use ovim::editor::lsp_manager_panel::{InstallProgress, InstallStatus};

    let pending = editor.take_pending_installs();
    if pending.is_empty() {
        return;
    }

    let tx = editor.install_progress_tx().cloned();
    let Some(tx) = tx else { return };

    for request in pending {
        let tx = tx.clone();
        let lang_name = request.language_name.clone();
        let lang_id = request.language_id.clone();
        let config = request.auto_install_config.clone();
        let command = request.lsp_command.clone();

        tokio::spawn(async move {
            let _ = tx.send(InstallProgress {
                language_id: lang_id.clone(),
                status: InstallStatus::Installing(format!("Installing {lang_name}...")),
            });

            let result =
                crate::lsp_init::auto_install::attempt_auto_install(&lang_name, &command, &config)
                    .await;

            let status = match result {
                crate::lsp_init::auto_install::InstallResult::Success(_) => InstallStatus::Success,
                crate::lsp_init::auto_install::InstallResult::Failed(msg) => {
                    InstallStatus::Failed(msg)
                }
                crate::lsp_init::auto_install::InstallResult::PrerequisitesMissing(msg) => {
                    InstallStatus::Failed(msg)
                }
            };

            let _ = tx.send(InstallProgress {
                language_id: lang_id,
                status,
            });
        });
    }
}

/// Helper to process preview and file picker results
fn process_picker_results(
    editor: &mut Editor,
    preview_rx: &mut tokio::sync::mpsc::Receiver<(String, editor::PreviewCache)>,
    file_rx: &mut tokio::sync::mpsc::Receiver<editor::PickerResult>,
) {
    // Try to drain pending preview loads (single mark_dirty after batch)
    let mut previews_loaded = false;
    while let Ok((path, cache)) = preview_rx.try_recv() {
        editor.insert_preview(path, cache);
        previews_loaded = true;
    }
    if previews_loaded {
        editor.mark_dirty();
    }
    // Drain pending file results with a time budget to avoid stalling input
    let mut files_added = false;
    let drain_start = std::time::Instant::now();
    let drain_budget = std::time::Duration::from_millis(2);
    loop {
        if drain_start.elapsed() >= drain_budget {
            break;
        }
        match file_rx.try_recv() {
            Ok(result) => {
                if let Some(picker) = editor.picker_mut() {
                    picker.add_file_result(result);
                    files_added = true;
                }
            }
            Err(_) => break,
        }
    }
    if files_added {
        editor.mark_dirty();
    }
    // Update file list cache from background task (if completed)
    update_file_list_cache_from_background(editor);
}

/// Headless (API-only) event loop.
pub async fn run_headless_loop(
    editor: &mut Editor,
    mut api_rx: mpsc::Receiver<ApiRequest>,
    mut java_status_rx: mpsc::Receiver<String>,
    start_time: SystemTime,
    session_info: Arc<Mutex<SessionInfo>>,
    initial_dimensions: (u16, u16),
    mut shutdown_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let (preview_tx, mut preview_rx) =
        tokio::sync::mpsc::channel::<(String, editor::PreviewCache)>(100);
    let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<editor::PickerResult>(1000);
    let (syntax_tx, mut syntax_rx) =
        tokio::sync::mpsc::channel::<(BufferId, Language, Option<LineHighlights>, u64)>(16);
    let mut lsp_interval = interval(Duration::from_millis(50));
    lsp_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Reused across `GetRender` requests so identical-dimension polls
    // skip the full ratatui+highlight pipeline (OV-00181).
    let mut render_cache = ovim::ui::AnsiRenderCache::new();
    let mut last_edit = Instant::now();
    let mut last_external_file_check = Instant::now();

    // A TUI paints its first frame before a person can type. Establish the
    // same layout and viewport contract before accepting headless requests.
    handle_terminal_resize(editor, initial_dimensions.0, initial_dimensions.1)?;
    let _ = render_cache.render(editor, initial_dimensions.0, initial_dimensions.1, false)?;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                break;
            }
            Some(request) = api_rx.recv() => {
                let version_before = editor.buffer().version();
                crate::api_dispatch::handle_api_request(editor, request, start_time, &session_info, &mut render_cache).await;
                if editor.buffer().version() != version_before {
                    last_edit = Instant::now();
                }
                if editor.should_quit() { break; }
            }
            Some((path, cache)) = preview_rx.recv() => {
                editor.insert_preview(path, cache);
                editor.mark_dirty();
            }
            Some(result) = file_rx.recv() => {
                let added = if let Some(picker) = editor.picker_mut() {
                    picker.add_file_result(result);
                    true
                } else {
                    false
                };
                if added {
                    editor.mark_dirty();
                }
            }
            _ = lsp_interval.tick() => {
                process_editor_tick(editor, &mut java_status_rx, &preview_tx, &file_tx, &syntax_tx, &mut syntax_rx).await;
                if last_external_file_check.elapsed() >= Duration::from_millis(500) {
                    process_external_file_change(editor);
                    last_external_file_check = Instant::now();
                }
                if editor.buffer().needs_rehighlight()
                    && last_edit.elapsed() >= Duration::from_millis(200)
                {
                    editor.process_pending_rehighlight().await;
                }
            }
        }
    }
    editor.close_current_file_lsp().await;
    Ok(())
}

/// Execute a shell command with full terminal access.
///
/// Leaves the alternate screen so the command's output is visible on the
/// normal terminal, runs the command with inherited stdio, then waits for
/// the user to press Enter before restoring the editor UI.
fn execute_shell_command(ui: &mut UI, editor: &mut Editor, command: &str) {
    use std::io::Write;
    use std::process::Command;

    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_arg = if cfg!(windows) { "/C" } else { "-c" };

    // Leave the TUI so the command gets a normal terminal
    if let Err(e) = ui.terminal_mut().suspend() {
        editor.set_status_message(format!("Failed to suspend terminal: {e}"));
        return;
    }

    // Show which command we're running (like Vim does)
    let _ = writeln!(std::io::stdout(), "\x1b[1m:!{command}\x1b[0m");
    let _ = std::io::stdout().flush();

    // Run the command
    let status = Command::new(shell)
        .arg(shell_arg)
        .arg(command)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .stdin(std::process::Stdio::inherit())
        .status();

    // Show result and wait for Enter
    let _ = std::io::stdout().flush();
    match &status {
        Ok(s) if !s.success() => {
            let _ = writeln!(std::io::stdout(), "\n\x1b[33mshell returned {}\x1b[0m", s);
        }
        Err(e) => {
            let _ = writeln!(
                std::io::stdout(),
                "\n\x1b[31mFailed to run command: {e}\x1b[0m"
            );
        }
        _ => {}
    }
    let _ = write!(std::io::stdout(), "\n\x1b[7mPress ENTER to continue\x1b[0m");
    let _ = std::io::stdout().flush();

    // Wait for Enter (read raw bytes since we're not in raw mode)
    let _ = std::io::stdin().read_line(&mut String::new());

    // Restore the TUI
    if let Err(e) = ui.terminal_mut().resume() {
        // If resume fails, the Drop impl will try to clean up
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Failed to resume terminal: {e}");
        }
    }

    // Force full redraw
    editor.mark_dirty();

    match status {
        Ok(s) if s.success() => {
            editor.set_status_message(format!(":!{command}"));
        }
        Ok(s) => {
            editor.set_status_message(format!("shell returned {s}"));
        }
        Err(e) => {
            editor.set_status_message(format!("Failed to run command: {e}"));
        }
    }
}

/// Process a batch of terminal input events.
/// Returns true if any events were edit-related (for debounce tracking).
fn process_input_events(editor: &mut Editor, events: Vec<Event>) -> Result<bool> {
    let mut had_edit = false;
    for event in events {
        match event {
            Event::Key(key_event) => {
                let key = convert_key_event(key_event);
                InputHandler::handle_key_event_no_dirty(editor, key)?;
                had_edit = true;
            }
            Event::Paste(text) => {
                editor.handle_paste_event(&text)?;
                had_edit = true;
            }
            Event::Resize(w, h) => {
                // Keep cached viewport geometry in sync with the terminal size so vertical
                // scrolling (especially near EOF) works correctly after pane resizes.
                //
                // Without this, rapid post-resize navigation can use stale viewport/wrap
                // dimensions until the next render pass updates them, which can make the
                // cursor move past the visible buffer without the viewport following.
                handle_terminal_resize(editor, w, h)?;
                editor.startle_cat();
            }
            Event::FocusGained => {
                editor.render_cache.terminal_image_refresh_requested = true;
                process_external_file_change(editor);
            }
            Event::Mouse(mouse_event) => {
                if editor.has_codex_auth_dialog() {
                    continue;
                }
                // Skip mouse-move events — they don't change editor state and
                // would otherwise trigger unnecessary redraws on every movement.
                if matches!(mouse_event.kind, crossterm::event::MouseEventKind::Moved) {
                    continue;
                }
                let mouse = convert_mouse_event(mouse_event);
                if let Some(url) = handle_mouse_event(editor, mouse)? {
                    let _ = open::that_in_background(&url);
                    continue;
                }
                had_edit = true;
            }
            _ => {}
        }
    }
    Ok(had_edit)
}

pub(crate) fn handle_terminal_resize(editor: &mut Editor, width: u16, height: u16) -> Result<()> {
    // Recompute the buffer chunk dimensions using the same high-level rules as the renderer.
    // This is intentionally approximate (position doesn't matter here) — it just needs to
    // keep viewport height and wrap width correct for scroll calculations.
    let mut content_width = width;
    let mut content_height = height;

    // Tab bar consumes one row when multiple tabs are present.
    if editor.tab_count() > 1 {
        content_height = content_height.saturating_sub(1);
    }

    // File tree consumes fixed width when visible.
    if editor.file_tree().is_visible() {
        content_width = content_width.saturating_sub(50);
    }

    // Progress line consumes one row when present.
    if editor.lsp_progress_message().is_some() {
        content_height = content_height.saturating_sub(1);
    }

    // Status line + command/message line.
    content_height = content_height.saturating_sub(2);

    // Apply textwidth centering: narrowing changes wrap width, but not viewport height.
    if let Some(textwidth) = editor.options.textwidth {
        let max_width = textwidth as u16;
        if content_width > max_width {
            content_width = max_width;
        }
    }

    // Update cached viewport dimensions for scroll calculations.
    editor.set_viewport_height(content_height as usize);

    // Update window sizes so horizontal scrolling calculations use the latest width.
    if let Some(wm) = editor.window_manager_mut() {
        wm.update_dimensions(content_width, content_height);
    } else {
        editor.init_window_manager(content_width, content_height);
    }

    // Keep the wrap map in sync with the new width so vertical scrolling stays accurate
    // in wrap mode.
    if editor.options.wrap {
        let text_width = compute_text_width(editor, content_width);
        editor.ensure_wrap_map(text_width);
    }

    // Re-run scroll update so the cursor remains visible in the resized viewport.
    editor.update_scroll_offset();

    Ok(())
}

fn compute_text_width(editor: &Editor, content_width: u16) -> usize {
    // Keep in sync with `ovim::ui::renderer::layout::BufferLayout::compute`.
    const SIGN_WIDTH: usize = 2;
    const GUTTER_SPACING: usize = 1;

    let show_numbers = editor.options.number || editor.options.relative_number;
    let line_count = editor.buffer().line_count();
    let line_num_width = if show_numbers {
        line_count.to_string().len().max(3)
    } else {
        0
    };

    let blame_width = if editor.options.blame {
        if let Some(blame) = editor.buffer().git_blame() {
            let author_len = blame.max_author_len().min(15);
            1 + 1 + 5 + 1 + author_len.max(3) + 1
        } else {
            0
        }
    } else {
        0
    };

    let gutter_width = blame_width + SIGN_WIDTH + line_num_width + GUTTER_SPACING;

    // Apply textwidth narrowing (OV-00019: must match renderer's BufferLayout
    // which narrows buffer_area to textwidth before computing text_width).
    let effective_width = if let Some(textwidth) = editor.options.textwidth {
        let max = textwidth as u16;
        if content_width > max {
            max
        } else {
            content_width
        }
    } else {
        content_width
    };

    (effective_width as usize).saturating_sub(gutter_width)
}

fn api_session_info(editor: &Editor) -> SessionInfo {
    let file = editor.buffer().file_path().map(str::to_string);
    let Some(name) = editor.active_session() else {
        return SessionInfo::new(0, file, "tui".into());
    };

    let mut info = SessionInfo::read(name).unwrap_or_else(|_| {
        SessionInfo::new(
            editor.api_port().unwrap_or(0),
            file.clone(),
            name.to_string(),
        )
    });
    info.file = file;
    info
}

/// TUI event loop (optionally with API).
pub async fn run_event_loop(
    ui: &mut UI,
    editor: &mut Editor,
    mut api_rx: Option<mpsc::Receiver<ApiRequest>>,
    mut java_status_rx: mpsc::Receiver<String>,
    start_time: SystemTime,
) -> Result<()> {
    let mut last_edit = Instant::now();
    let debounce_delay = Duration::from_millis(200);
    let mut last_input_time: Option<Instant> = None;
    let (preview_tx, mut preview_rx) =
        tokio::sync::mpsc::channel::<(String, editor::PreviewCache)>(100);
    let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<editor::PickerResult>(1000);
    let (syntax_tx, mut syntax_rx) =
        tokio::sync::mpsc::channel::<(BufferId, Language, Option<LineHighlights>, u64)>(16);

    let mut event_stream = EventStream::new();
    let mut tick_interval = interval(Duration::from_millis(16));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Reused across `GetRender` requests so identical-dimension polls
    // skip the full ratatui+highlight pipeline (OV-00181). Live for the
    // entire TUI session even when `api_rx` is `None` — cheap and keeps
    // the call sites uniform.
    let mut render_cache = ovim::ui::AnsiRenderCache::new();
    let mut last_external_file_check = Instant::now();
    let mut observed_ai_attention_generation = editor.ai_chat_attention_generation();
    let mut last_terminal_mode_refresh = Instant::now();

    while !editor.should_quit() {
        // Wait for input, API request, or tick — input has priority via `biased`
        tokio::select! {
            biased;

            // Terminal input (highest priority)
            maybe_event = event_stream.next() => {
                if let Some(Ok(first_event)) = maybe_event {
                    last_input_time = Some(Instant::now());

                    // A focus transition is a common time for terminal-global
                    // mouse/bracketed-paste modes to have been disturbed. Do
                    // this before handling the batch so a FocusGained event
                    // repairs interaction modes before a following drop.
                    let _ = ui.terminal_mut().ensure_interaction_modes();
                    last_terminal_mode_refresh = Instant::now();

                    // Batch: collect first event + drain all queued events
                    let mut events = vec![first_event];
                    while event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                        if let Ok(ev) = event::read() {
                            events.push(ev);
                        }
                    }

                    let had_edit = process_input_events(editor, events)?;
                    if had_edit {
                        last_edit = Instant::now();
                    }

                    // Mark dirty and immediately refresh the visible syntax
                    // once after all events processed.
                    refresh_after_input(editor);

                    // Immediately process LSP actions triggered by input
                    editor.dispatch_pending_intents().await;

                    // If more input queued, skip render to keep input flowing
                    if crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                        continue;
                    }
                }
            }

            // API requests
            Some(request) = async {
                if let Some(ref mut rx) = api_rx { rx.recv().await } else { std::future::pending().await }
            } => {
                let api_session = Arc::new(Mutex::new(api_session_info(editor)));
                let version_before = editor.buffer().version();
                crate::api_dispatch::handle_api_request(editor, request, start_time, &api_session, &mut render_cache).await;
                if editor.buffer().version() != version_before {
                    last_edit = Instant::now();
                }
                // Drain remaining queued API requests
                if let Some(ref mut rx) = api_rx {
                    while let Ok(req) = rx.try_recv() {
                        let version_before = editor.buffer().version();
                        crate::api_dispatch::handle_api_request(editor, req, start_time, &api_session, &mut render_cache).await;
                        if editor.buffer().version() != version_before {
                            last_edit = Instant::now();
                        }
                    }
                }
                // Offscreen API rendering shares the frame renderer, which
                // updates geometry and hit-test caches. Repaint the real TUI
                // before accepting more terminal input so those caches always
                // describe the visible terminal surface.
                editor.mark_dirty();
            }

            // Tick timer — background work (LSP, picker, animations)
            _ = tick_interval.tick() => {
                process_editor_tick(editor, &mut java_status_rx, &preview_tx, &file_tx, &syntax_tx, &mut syntax_rx).await;
                process_picker_results(editor, &mut preview_rx, &mut file_rx);
                if last_external_file_check.elapsed() >= Duration::from_millis(500) {
                    process_external_file_change(editor);
                    last_external_file_check = Instant::now();
                }

            }
        }

        // Approval prompts are created by background polling as well as input
        // dispatch. Notify on the core's edge signal, outside rendering, so a
        // paused agent rings once even while the screen continues to redraw.
        notify_new_agent_attention(editor, &mut observed_ai_attention_generation);

        // Mouse capture and bracketed paste are terminal-global modes, not
        // durable application state. If another terminal participant clears
        // them, scrolling escapes into terminal scrollback and image drops
        // arrive as ordinary keystrokes. Periodically reassert both so the TUI
        // recovers without requiring a restart (or even an input event).
        if last_terminal_mode_refresh.elapsed() >= Duration::from_secs(1) {
            let _ = ui.terminal_mut().ensure_interaction_modes();
            last_terminal_mode_refresh = Instant::now();
        }

        // Execute pending shell command with full terminal access
        if let Some(pending) = editor.take_pending_shell_command() {
            execute_shell_command(ui, editor, &pending.command);
        }

        // Render after any select branch (if dirty)
        if editor.is_dirty() {
            let start = Instant::now();
            ui.renderer_mut().render(editor)?;
            editor.record_render_duration(start.elapsed().as_micros() as u64);
            editor.increment_render_count();
            editor.mark_clean();
            if let Some(input_time) = last_input_time.take() {
                editor.record_input_latency(input_time.elapsed().as_micros() as u64);
            }
        }

        // Debounced rehighlight
        if editor.buffer().needs_rehighlight() && last_edit.elapsed() >= debounce_delay {
            editor.process_pending_rehighlight().await;
        }
    }

    editor.close_current_file_lsp().await;
    Ok(())
}

/// Spawns a background task to load picker preview if debounce time has elapsed
/// Returns immediately without blocking input handling
fn spawn_picker_preview_loading(
    editor: &mut Editor,
    preview_tx: &tokio::sync::mpsc::Sender<(String, editor::PreviewCache)>,
) {
    if !editor.should_load_picker_preview(50) {
        return;
    }

    // Get the file to load (returns None if already cached/loading)
    if let Some(file_path) = editor.get_preview_to_load() {
        let tx = preview_tx.clone();

        // Spawn background task - doesn't block!
        tokio::spawn(async move {
            // Load preview asynchronously
            if let Some(cache) = load_preview_async(&file_path).await {
                // Send result back (non-blocking)
                let _ = tx.send((file_path, cache)).await;
            }
        });
    }
}

/// Spawns a background task to load files for file finder picker
/// Returns immediately without blocking - files are sent via channel as they're discovered
/// Uses cache when available to speed up repeated picker opens
fn spawn_file_finder_loading(
    editor: &mut Editor,
    file_tx: &tokio::sync::mpsc::Sender<editor::PickerResult>,
) {
    // Check if we should spawn file loading
    if let Some(picker) = editor.picker() {
        if !picker.should_spawn_file_loading() {
            return;
        }

        // Get the base directory for file search (git root when available)
        let base_dir = picker.base_dir().to_path_buf();
        // Preferred directory for local-first ordering (typically current file's folder)
        let preferred_dir = picker.preferred_dir().to_path_buf();

        // Check for cached file list (5-minute TTL)
        if let Some(cached_files) = editor.get_cached_file_list(&base_dir, &preferred_dir) {
            // Use cache! Send all files via channel immediately
            let cached_files: Vec<editor::PickerResult> = cached_files.to_vec();
            let tx = file_tx.clone();

            // Mark as spawned to avoid spawning multiple tasks
            if let Some(picker_mut) = editor.picker_mut() {
                picker_mut.mark_loading_spawned();
            }

            // Spawn quick task to send cached results
            tokio::spawn(async move {
                for result in cached_files {
                    if tx.send(result).await.is_err() {
                        break;
                    }
                }
            });
            return;
        }

        // Mark as spawned to avoid spawning multiple tasks
        if let Some(picker_mut) = editor.picker_mut() {
            picker_mut.mark_loading_spawned();
        }

        let tx = file_tx.clone();
        let base_dir_clone = base_dir.clone();
        let preferred_dir_clone = preferred_dir.clone();

        // Spawn background task - doesn't block!
        // Also collects results for cache update
        tokio::spawn(async move {
            use ignore::WalkBuilder;

            let mut collected_files = Vec::new();

            let mut roots: Vec<(std::path::PathBuf, bool)> = Vec::new();
            if preferred_dir_clone != base_dir_clone
                && preferred_dir_clone.starts_with(&base_dir_clone)
            {
                roots.push((preferred_dir_clone.clone(), true));
            }
            roots.push((base_dir_clone.clone(), false));

            // Walk preferred subtree first, then base (excluding preferred) for local-first ordering.
            for (root, is_preferred_root) in roots {
                let base_dir_for_strip = base_dir_clone.clone();
                let preferred_for_filter = preferred_dir_clone.clone();

                // Use ignore crate's WalkBuilder which respects .gitignore
                let walker = WalkBuilder::new(&root)
                    .hidden(false) // Don't automatically skip hidden files (keep .env, .eslintrc, etc.)
                    .git_ignore(true) // Respect .gitignore files
                    .git_global(true) // Respect global gitignore
                    .git_exclude(true) // Respect .git/info/exclude
                    .filter_entry(move |entry| {
                        // Skip .git directory (not in .gitignore but shouldn't be shown)
                        if entry.file_name() == ".git" {
                            return false;
                        }
                        // For the base-dir pass, skip the preferred subtree entirely to avoid duplicates.
                        if !is_preferred_root
                            && preferred_for_filter != base_dir_for_strip
                            && entry.path().starts_with(&preferred_for_filter)
                        {
                            return false;
                        }
                        true
                    })
                    .build();

                // Walk the directory tree and send files as we find them
                for entry in walker.filter_map(|e| e.ok()) {
                    let path = entry.path();

                    if path.is_file() {
                        if let Ok(relative_path) = path.strip_prefix(&base_dir_clone) {
                            let display_path = relative_path.to_string_lossy().to_string();
                            let result = editor::PickerResult {
                                display: display_path,
                                location: path.to_string_lossy().to_string(),
                                line: 0,
                                col: 0,
                                match_positions: Vec::new(),
                                content: None,
                            };

                            // Collect for cache
                            collected_files.push(result.clone());

                            // Send result back (non-blocking)
                            // If channel is closed (picker was closed), task will exit
                            if tx.send(result).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }

            // Store collected files in a static to be picked up by cache update
            // This is a workaround since we can't update Editor state from within a spawned task
            FILE_LIST_CACHE_RESULTS.lock().await.replace((
                base_dir_clone,
                preferred_dir_clone,
                collected_files,
            ));
        });
    }
}

/// Temporary storage for file list results from background task
/// The main event loop will pick these up and update the Editor cache
static FILE_LIST_CACHE_RESULTS: tokio::sync::Mutex<
    Option<(
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<editor::PickerResult>,
    )>,
> = tokio::sync::Mutex::const_new(None);

/// Picks up cached file list results from the background task and updates Editor cache
pub fn update_file_list_cache_from_background(editor: &mut Editor) {
    // Non-blocking try_lock to avoid any contention with background task
    if let Ok(mut guard) = FILE_LIST_CACHE_RESULTS.try_lock() {
        if let Some((base_dir, preferred_dir, files)) = guard.take() {
            editor.update_file_list_cache(base_dir, preferred_dir, files);
        }
    }
}

/// Spawn `gradle <task> --debug-jvm [extra_args]` and wait for the JVM to start listening.
///
/// Reads stderr lines until "Listening for transport dt_socket at address:" appears,
/// then returns the child process (caller stores it for cleanup). Times out after 60s.
async fn spawn_gradle_and_wait(
    task: &str,
    extra_args: &[String],
    cwd: &str,
) -> anyhow::Result<tokio::process::Child> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let gradle_cmd = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "./gradlew"
    };
    // Fall back to system gradle if wrapper doesn't exist.
    let cmd = if std::path::Path::new(cwd).join(gradle_cmd).exists() {
        gradle_cmd
    } else {
        "gradle"
    };

    let mut child = Command::new(cmd)
        .arg(task)
        .arg("--debug-jvm")
        .args(extra_args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn gradle: {e}"))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stderr from gradle"))?;

    let mut reader = BufReader::new(stderr).lines();

    let listening = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Ok(Some(line)) = reader.next_line().await {
            if line.contains("Listening for transport dt_socket at address:") {
                return Ok(());
            }
        }
        Err(anyhow::anyhow!(
            "gradle process exited before JVM started listening"
        ))
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for gradle --debug-jvm to start"))?;

    listening?;
    Ok(child)
}

/// Loads a file preview asynchronously (can be called from background task)
async fn load_preview_async(file_path: &str) -> Option<editor::PreviewCache> {
    // Check file size before loading (max 1MB for preview)
    const MAX_PREVIEW_SIZE: u64 = 1024 * 1024;
    if let Ok(metadata) = tokio::fs::metadata(file_path).await {
        if metadata.len() > MAX_PREVIEW_SIZE {
            // File too large, create a placeholder
            return Some(editor::PreviewCache {
                content: format!("File too large for preview ({} bytes)", metadata.len()),
                highlighted_lines: std::cell::RefCell::new(HashMap::new()),
                language: None,
            });
        }
    }

    // Load the file
    let content = tokio::fs::read_to_string(file_path).await.ok()?;

    // Detect language
    let language = LanguageRegistry::detect_from_path(file_path);

    // Parse syntax highlights in a background thread so the render thread doesn't block
    let highlighted_lines = if let Some(lang) = language {
        let content_for_parse = content.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(mut h) = SyntaxHighlighter::new(lang) {
                h.parse(&content_for_parse);
                let all = h.highlights_for_all_lines(&content_for_parse);
                let mut map = HashMap::new();
                for (i, line_h) in all.into_iter().enumerate() {
                    if !line_h.is_empty() {
                        map.insert(i, line_h);
                    }
                }
                map
            } else {
                HashMap::new()
            }
        })
        .await
        .unwrap_or_default()
    } else {
        HashMap::new()
    };

    // Create cache entry with pre-populated highlights
    Some(editor::PreviewCache {
        content,
        highlighted_lines: std::cell::RefCell::new(highlighted_lines),
        language,
    })
}

#[cfg(test)]
mod tests {
    use super::apply_java_status;
    use super::compute_text_width;
    use super::emit_new_agent_attention;
    use super::handle_terminal_resize;
    use super::process_input_events;
    use super::{refresh_after_input, tick_transient_ui};
    use crate::api_dispatch::handle_edit_line;
    use crate::api_dispatch::spawn_agent_control;
    use crate::api_dispatch::{create_snapshot, create_snapshot_with_dimensions};
    use ovim::api::AgentControlTarget;
    use ovim::api::SNAPSHOT_SCHEMA_VERSION;
    use ovim::api::{ApiRequest, ApiResponse};
    use ovim::editor::{Editor, InputHandler, PreparedHeadlessAgentControl};
    use ovim::mode::Mode;
    use ovim::session::SessionInfo;
    use ovim::ui::AnsiRenderCache;
    use ovim_core::agent_runtime::AgentMailbox;
    use ovim_core::ai::chat_types::ChatOpts;
    use ovim_core::run_log::{AgentId, InMemoryRunEventSink, OperationId, RunId};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};
    use tokio::sync::oneshot;

    fn test_session() -> Arc<Mutex<SessionInfo>> {
        Arc::new(Mutex::new(SessionInfo::new(0, None, "test".into())))
    }

    #[tokio::test]
    async fn headless_agent_wait_is_spawned_and_bounded_off_the_editor_loop() {
        let run_id = RunId::new();
        let root_agent_id = AgentId::new();
        let child_agent_id = AgentId::new();
        let mailbox = AgentMailbox::new(
            run_id.clone(),
            root_agent_id,
            Arc::new(InMemoryRunEventSink::new()),
        )
        .unwrap();
        let target = AgentControlTarget {
            run_id,
            agent_id: child_agent_id.clone(),
            turn_generation: 0,
            operation_id: OperationId::new(),
        };
        let (tx, mut rx) = oneshot::channel();
        spawn_agent_control(
            Ok(PreparedHeadlessAgentControl::Wait {
                mailbox,
                agent_id: child_agent_id,
                timeout: Duration::from_millis(25),
            }),
            target,
            tx,
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let response = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("agent wait must be bounded")
            .unwrap();
        let ApiResponse::AgentControl(response) = response else {
            panic!("expected agent control response")
        };
        assert_eq!(response.result["outcome"], "timed_out");
    }

    #[tokio::test]
    async fn get_health_does_not_write_session_file_for_unregistered_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-threaded mutation of a test-only variable; no other
        // test in this binary reads OVIM_SESSION_DIR.
        unsafe { std::env::set_var("OVIM_SESSION_DIR", dir.path()) };

        let mut editor = Editor::default();
        let mut cache = AnsiRenderCache::new();
        // Port 0 is the placeholder for a TUI without a registered session.
        let session = test_session();

        let (tx, rx) = oneshot::channel();
        crate::api_dispatch::handle_api_request(
            &mut editor,
            ApiRequest::GetHealth(tx),
            SystemTime::now(),
            &session,
            &mut cache,
        )
        .await;

        // The health response still works...
        assert!(matches!(rx.await.unwrap(), ApiResponse::Health(_)));
        // ...and the in-memory flag is updated (no LSP servers => ready)...
        assert!(session.lock().unwrap().lsp_ready);
        // ...but no phantom session file may appear on disk.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read session dir")
            .collect();
        assert!(
            entries.is_empty(),
            "unregistered session must not be written to disk: {entries:?}"
        );

        // SAFETY: see above.
        unsafe { std::env::remove_var("OVIM_SESSION_DIR") };
    }

    #[test]
    fn agent_attention_bell_emits_once_for_each_generation() {
        let mut observed = 0;
        let mut output = Vec::new();

        assert!(emit_new_agent_attention(1, &mut observed, &mut output).unwrap());
        assert!(!emit_new_agent_attention(1, &mut observed, &mut output).unwrap());
        assert!(emit_new_agent_attention(2, &mut observed, &mut output).unwrap());

        assert_eq!(observed, 2);
        assert_eq!(output, b"\x07\x07");
    }

    #[test]
    fn focus_gain_requests_terminal_image_surface_refresh() {
        let mut editor = Editor::default();

        process_input_events(&mut editor, vec![crossterm::event::Event::FocusGained])
            .expect("focus event");

        assert!(editor.render_cache.terminal_image_refresh_requested);
    }

    #[tokio::test]
    async fn set_buffer_invalidates_cached_render() {
        let mut editor = Editor::with_content("before\n");
        let mut cache = AnsiRenderCache::new();
        cache.render(&mut editor, 80, 20, true).unwrap();
        assert!(cache.would_hit(&editor, 80, 20, true));

        let (tx, rx) = oneshot::channel();
        crate::api_dispatch::handle_api_request(
            &mut editor,
            ApiRequest::SetBuffer("PARITY_SENTINEL\n".into(), tx),
            SystemTime::now(),
            &test_session(),
            &mut cache,
        )
        .await;
        assert!(matches!(rx.await.unwrap(), ApiResponse::Success(_)));
        assert!(!cache.would_hit(&editor, 80, 20, true));
        let rendered = cache.render(&mut editor, 80, 20, true).unwrap();
        assert!(rendered.contains("PARITY_SENTINEL"));
    }

    #[tokio::test]
    async fn paste_api_delivers_multiline_text_as_one_event() {
        let mut editor = Editor::with_content("");
        let mut cache = AnsiRenderCache::new();
        let (mode_tx, mode_rx) = oneshot::channel();
        crate::api_dispatch::handle_api_request(
            &mut editor,
            ApiRequest::SetMode("INSERT".into(), mode_tx),
            SystemTime::now(),
            &test_session(),
            &mut cache,
        )
        .await;
        assert!(matches!(mode_rx.await.unwrap(), ApiResponse::Success(_)));
        assert_eq!(editor.mode(), Mode::Insert);

        let (tx, rx) = oneshot::channel();
        crate::api_dispatch::handle_api_request(
            &mut editor,
            ApiRequest::Paste("first\nsecond".into(), tx),
            SystemTime::now(),
            &test_session(),
            &mut cache,
        )
        .await;
        assert!(matches!(rx.await.unwrap(), ApiResponse::Success(_)));
        assert_eq!(editor.buffer().rope().to_string(), "first\nsecond");
    }

    #[test]
    fn snapshot_exposes_active_ai_chat_state() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                // Ovim is built as a dependency of this binary test, so its
                // durable-history code is not compiled with `cfg(test)`. Use
                // a fixture-specific conversation instead of accidentally
                // restoring the user's real default chat.
                name: "snapshot-schema-test".into(),
                ..ChatOpts::default()
            })
            .unwrap();

        let snapshot = create_snapshot(&editor);
        assert_eq!(snapshot.schema_version, SNAPSHOT_SCHEMA_VERSION);
        let chat = snapshot.ai_chat.expect("active chat snapshot");
        assert_eq!(chat.activity, "idle");
        assert!(!chat.waiting);
        assert!(chat.input.is_empty());
        assert!(chat.queued.is_empty());
        assert!(chat.messages.is_empty());
        assert_eq!(chat.focus, "text_input");
        assert_eq!(chat.input_cursor, 0);
    }

    #[tokio::test]
    async fn api_keys_match_direct_input_state_and_render() {
        let sequence = "jA!<Esc>gg0";
        let dimensions = (72, 20);
        let mut direct = Editor::with_content("alpha\nbeta\ngamma\n");
        let mut via_api = Editor::with_content("alpha\nbeta\ngamma\n");
        handle_terminal_resize(&mut direct, dimensions.0, dimensions.1).unwrap();
        handle_terminal_resize(&mut via_api, dimensions.0, dimensions.1).unwrap();

        for event in ovim::api::parse_key_string(sequence).unwrap() {
            InputHandler::handle_key_event_no_dirty(&mut direct, event).unwrap();
        }
        refresh_after_input(&mut direct);

        let (tx, rx) = oneshot::channel();
        let mut api_cache = AnsiRenderCache::new();
        let session = Arc::new(Mutex::new(
            SessionInfo::new(12345, None, "parity".to_string())
                .with_dimensions(dimensions.0, dimensions.1),
        ));
        crate::api_dispatch::handle_api_request(
            &mut via_api,
            ApiRequest::SendKeys(sequence.to_string(), tx),
            SystemTime::now(),
            &session,
            &mut api_cache,
        )
        .await;
        assert!(matches!(rx.await.unwrap(), ApiResponse::SendKeysResult(_)));

        let direct_snapshot = create_snapshot_with_dimensions(&direct, Some(dimensions));
        let api_snapshot = create_snapshot_with_dimensions(&via_api, Some(dimensions));
        assert_eq!(
            serde_json::to_value(direct_snapshot).unwrap(),
            serde_json::to_value(api_snapshot).unwrap()
        );

        let mut direct_cache = AnsiRenderCache::new();
        let direct_render = direct_cache
            .render(&mut direct, dimensions.0, dimensions.1, true)
            .unwrap();
        let api_render = api_cache
            .render(&mut via_api, dimensions.0, dimensions.1, true)
            .unwrap();
        assert_eq!(direct_render, api_render);
    }

    #[test]
    fn working_animation_tick_invalidates_the_render_without_input() {
        let mut editor = Editor::with_content("hello\n");
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor.ai_state.chat.as_mut().unwrap().waiting = true;
        editor.render_cache.ai_chat_working_animation_tick = u128::MAX;
        editor.mark_clean();

        tick_transient_ui(&mut editor);

        assert!(editor.is_dirty());
    }

    #[test]
    fn resize_updates_viewport_and_wrap_map_and_keeps_cursor_visible() {
        // 200 logical lines, wide enough to exercise gutter sizing.
        let content: String = (1..=200)
            .map(|i| format!("line {i}: {}\n", "x".repeat(120)))
            .collect();

        let mut editor = Editor::with_content(&content);
        editor.options.number = true;
        editor.options.wrap = true;
        editor.options.scrolloff = 0;

        // Initial size.
        handle_terminal_resize(&mut editor, 80, 20).unwrap();
        assert_eq!(editor.viewport_height(), 18);

        // Move cursor to EOF and ensure scroll offset is set.
        let last_line = editor.buffer().line_count().saturating_sub(1);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(last_line, ovim_core::unicode::GraphemeCol::ZERO);
        editor.update_scroll_offset();

        // Shrink the pane; cursor should remain visible in the new viewport.
        handle_terminal_resize(&mut editor, 80, 10).unwrap();
        assert_eq!(editor.viewport_height(), 8);

        let cursor_line = editor.buffer().cursor().line();
        let scroll_offset = editor.scroll_offset();
        let visible = editor.viewport_height().max(1);
        assert!(
            cursor_line >= scroll_offset && cursor_line < scroll_offset + visible,
            "cursor should remain visible after resize: cursor_line={cursor_line} scroll_offset={scroll_offset} viewport={visible}"
        );

        // Wrap map should match the new text width (buffer width minus gutter).
        let wrap_width = editor.wrap_map().map(|m| m.wrap_width()).unwrap_or(0);
        assert_eq!(wrap_width, compute_text_width(&editor, 80).max(1));
    }

    #[test]
    fn java_ready_status_requests_diagnostics_refresh() {
        let mut editor = Editor::with_content("class Test {}\n");

        apply_java_status(&mut editor, "Java: Ready".to_string());

        assert_eq!(editor.status_message(), "Java: Ready");
        assert!(editor.take_diagnostics_refresh_request());
    }

    #[test]
    fn kotlin_ready_status_requests_diagnostics_refresh() {
        let mut editor = Editor::with_content("fun main() {}\n");

        apply_java_status(&mut editor, "Kotlin: Ready".to_string());

        assert_eq!(editor.status_message(), "Kotlin: Ready");
        assert!(editor.take_diagnostics_refresh_request());
    }

    #[test]
    fn java_non_ready_status_does_not_request_diagnostics_refresh() {
        let mut editor = Editor::with_content("class Test {}\n");

        apply_java_status(&mut editor, "Java: Starting Hyperion LSP...".to_string());

        assert_eq!(editor.status_message(), "Java: Starting Hyperion LSP...");
        assert!(!editor.take_diagnostics_refresh_request());
    }

    // ==================== OV-00243: byte/char mix in handle_edit_line ====================
    // (find_char_positions unit tests moved to edit_engine.rs with the
    // function; the handler-level regressions below still cover the path.)

    #[test]
    fn handle_edit_line_replaces_match_after_non_ascii_prefix() {
        // Pre-OV-00243: `find()` returned byte offset 3 for "bar" after "é ",
        // and `CharCol(3)` pointed past the "b" — corrupting the substitution.
        // Post-fix: char offset 2 is correct.
        let mut editor = Editor::with_content("é bar baz\n");
        let resp = handle_edit_line(&mut editor, Some(0), "bar", "qux");
        assert!(matches!(resp, ApiResponse::Success(_)));
        let line = editor.buffer().rope().line(0).to_string();
        assert_eq!(line.trim_end_matches('\n'), "é qux baz");
    }

    #[test]
    fn handle_edit_line_redo_lands_cursor_in_grapheme_space() {
        // The recorded `cursor_after` on the undo entry is what redo restores.
        // Pre-OV-00243: cursor_after was `GraphemeCol(byte_offset + byte_len)`
        // — a byte-quantity smuggled into a grapheme newtype. After substituting
        // "old" → "NEW" on a line prefixed with a 25-byte / 7-char / 1-grapheme
        // family emoji, redo must place the cursor at grapheme col 1 + 3 = 4,
        // not at byte 25 + 3 = 28 (off the end of the line).
        let mut editor = Editor::with_content("👨‍👩‍👧‍👦old after\n");
        let resp = handle_edit_line(&mut editor, Some(0), "old", "NEW");
        assert!(matches!(resp, ApiResponse::Success(_)));
        editor.undo();
        editor.redo();
        let cursor = editor.buffer().cursor();
        assert_eq!(cursor.line(), 0);
        assert_eq!(
            cursor.col().0,
            4,
            "redo should place cursor at grapheme col 4 (1 emoji + 3 letters of 'NEW')"
        );
    }

    #[test]
    fn handle_edit_line_undo_restores_non_ascii_line() {
        // Round-trip undo through a non-ASCII substitution: pre-OV-00243 this
        // would corrupt the line because the recorded edit positions were
        // wrong, so undo couldn't restore the original bytes correctly.
        let original = "café déjà vu\n";
        let mut editor = Editor::with_content(original);
        let resp = handle_edit_line(&mut editor, Some(0), "déjà", "now");
        assert!(matches!(resp, ApiResponse::Success(_)));
        assert_eq!(
            editor
                .buffer()
                .rope()
                .line(0)
                .to_string()
                .trim_end_matches('\n'),
            "café now vu"
        );
        editor.undo();
        assert_eq!(editor.buffer().rope().line(0).to_string(), original);
    }
}
