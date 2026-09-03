use tokio::sync::mpsc;

use crate::buffer::{BufferId, LineHighlights};
use crate::editor::Editor;
use crate::mode::Mode;
use crate::syntax::{Language, LanguageRegistry, SyntaxHighlighter};

use super::channels::FrontendChannels;
use super::loading::{
    spawn_file_finder_loading, spawn_picker_preview_loading, update_file_list_cache_from_background,
};

fn apply_java_status(editor: &mut Editor, status: String) {
    let ready = status.trim().ends_with(": Ready");
    editor.set_lsp_status(status);
    if ready {
        editor.request_diagnostics_refresh();
    }
}

/// Drives one round of background work: LSP, DAP, syntax highlighting,
/// picker, and installs. Call on a periodic interval from any frontend.
pub async fn process_editor_tick(editor: &mut Editor, channels: &mut FrontendChannels) {
    // Do not let a slow LSP initialization trap a yank flash on screen. Keep
    // deferring LSP while the flash is visible and for the tick that clears
    // it, giving the frontend one complete tick to paint the clear frame.
    let defer_lsp_for_yank_flash = process_yank_flash(editor);

    // Syntax must get a complete tick before LSP startup. Starting a language
    // server can take several seconds, and awaiting it first used to leave a
    // newly opened file unhighlighted for the entire startup window.
    let defer_lsp_init = process_syntax_highlighting(editor, channels) || defer_lsp_for_yank_flash;

    // === LSP lifecycle ===
    process_java_status(editor, &mut channels.java_status_rx);
    process_lsp_notifications(editor).await;
    if !defer_lsp_init {
        process_lsp_init(editor).await;
    }
    process_lsp_sync_and_inlay_hints(editor).await;

    // === Debug adapter ===
    process_dap_events(editor);
    process_pending_debug_action(editor).await;

    // === LSP responses & intents ===
    if editor.poll_pending_lsp_responses() {
        editor.mark_dirty();
    }
    editor.dispatch_pending_intents().await;

    // === Background tasks ===
    poll_background_tasks(editor).await;
    update_file_list_cache_from_background(editor, channels);

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
        process_picker_tick(editor, channels);
    }

    // File switches queue didClose outside the async input dispatcher. Drive
    // that lifecycle from the shared tick so headless and TUI sessions agree.
    editor.send_lsp_close_if_needed().await;
}

/// Start or finish initial syntax work and report whether LSP initialization
/// should wait until a later tick. The extra tick lets the frontend paint the
/// completed syntax cache before a slow language-server startup is awaited.
fn process_syntax_highlighting(editor: &mut Editor, channels: &mut FrontendChannels) -> bool {
    let defer_lsp_init =
        editor.buffer().should_init_syntax() || editor.buffer().syntax_highlighting_is_loading();
    spawn_syntax_highlighting(editor, &channels.syntax_tx);
    drain_syntax_results(editor, &mut channels.syntax_rx);
    defer_lsp_init
}

/// Expire the yank flash without allowing slow LSP startup to delay the frame
/// that removes it. Returns true while LSP initialization should be deferred.
fn process_yank_flash(editor: &mut Editor) -> bool {
    let expired = editor.tick_yank_flash();
    if expired {
        editor.mark_dirty();
    }
    expired || editor.yank_flash().is_some()
}

fn tick_transient_ui(editor: &mut Editor) {
    if editor.tick_cat_animation()
        | editor.tick_toasts()
        | editor.tick_ai_chat_working_animation()
        | editor.tick_ai_chat_text_selection_autoscroll()
        | editor.poll_ai_subagent_repaint()
    {
        editor.mark_dirty();
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
                Some(crate::dap::PendingDebugAction::FetchState);
        }
    }
}

/// Dispatch the pending debug action (start, stop, step, evaluate, etc.).
async fn process_pending_debug_action(editor: &mut Editor) {
    let Some(action) = editor.dap_manager_mut().pending_action.take() else {
        return;
    };

    use crate::dap::PendingDebugAction;
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
    use crate::dap::PendingDebugAction;
    use crate::debug_config::DebugRunKind;

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
    use crate::dap::PendingDebugAction;

    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let mut configs = crate::debug_config::load_debug_configs(&project_root);

    if let Some(lsp_manager) = editor.lsp_manager() {
        let lsp_configs = lsp_manager.run_configurations().await;
        configs.extend(crate::debug_config::parse_lsp_run_configs(&lsp_configs));
    }

    editor.clear_status_message();

    if configs.is_empty() {
        let dap_start = editor
            .buffer()
            .file_path()
            .and_then(|fp| {
                crate::language_config::LanguageRegistry::try_get().and_then(|reg| reg.detect(fp))
            })
            .and_then(|lang| lang.dap.as_ref())
            .and_then(|config| {
                crate::language_config::find_dap_command(config)
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
                crate::language_config::LanguageRegistry::try_get().and_then(|reg| reg.detect(fp))
            })
            .and_then(|lang| lang.dap.as_ref())
            .and_then(|dap_config| {
                crate::language_config::find_dap_command(dap_config)
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
        let picker = crate::editor::picker::Picker::new_debug_config(project_root, names);
        editor.set_picker(picker);
        editor.set_mode(crate::mode::Mode::Picker);
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
    if editor.poll_pending_make() {
        editor.mark_dirty();
    }
    if editor.poll_pending_test() {
        editor.mark_dirty();
    }
    if editor.poll_git_refresh() {
        editor.mark_dirty();
    }
    if editor.poll_git_fetch() {
        editor.mark_dirty();
    }
    // The side-by-side diff review is laid out to a fixed width, so it has to
    // re-flow when the window changes size.
    if editor.relayout_diff_review() {
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
fn process_picker_tick(editor: &mut Editor, channels: &mut FrontendChannels) {
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
    spawn_picker_preview_loading(editor, &channels.preview_tx);
    spawn_file_finder_loading(editor, &channels.file_tx, &channels.file_list_cache_tx);
    if editor.picker_rapid_scrolling_just_stopped() {
        editor.mark_dirty();
    }
}

/// Spawn background tasks for pending LSP install requests
fn spawn_pending_installs(editor: &mut Editor) {
    use crate::editor::lsp_manager_panel::{InstallProgress, InstallStatus};

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

#[cfg(test)]
mod tests {
    use super::{
        apply_java_status, process_syntax_highlighting, process_yank_flash, tick_transient_ui,
    };
    use crate::editor::Editor;
    use crate::frontend::FrontendChannels;
    use ovim_core::ai::chat_types::ChatOpts;
    use tokio::sync::mpsc;

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
    fn yank_flash_defers_slow_work_until_its_clear_frame_can_paint() {
        let mut editor = Editor::with_content("copy me\n");
        editor.set_yank_flash_lines(0, 0);

        assert!(process_yank_flash(&mut editor));
        assert!(editor.yank_flash().is_some());

        std::thread::sleep(std::time::Duration::from_millis(175));
        editor.mark_clean();

        assert!(process_yank_flash(&mut editor));
        assert!(editor.yank_flash().is_none());
        assert!(editor.is_dirty());
        assert!(
            !process_yank_flash(&mut editor),
            "slow work may start only after the clear frame has been deferred once"
        );
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yaml_syntax_gets_a_paint_tick_before_lsp_initialization() {
        let mut editor = Editor::with_content("name: ovim\nenabled: true\n");
        editor.set_file_path("config.yaml".to_string());
        let (_java_status_tx, java_status_rx) = mpsc::channel(1);
        let mut channels = FrontendChannels::new(java_status_rx);

        assert!(process_syntax_highlighting(&mut editor, &mut channels));

        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let defer_lsp = process_syntax_highlighting(&mut editor, &mut channels);
            if editor.buffer().has_syntax_highlighting() {
                assert!(
                    defer_lsp,
                    "the completion tick must still defer LSP so the frontend can paint"
                );
                assert!(!editor.buffer().highlights_for_line(0).is_empty());
                assert!(
                    !process_syntax_highlighting(&mut editor, &mut channels),
                    "LSP may initialize on the tick after syntax is ready"
                );
                return;
            }
        }

        panic!("YAML syntax highlighting did not finish");
    }
}
