use anyhow::Result;
use crossterm::event::{self, Event, EventStream};
use futures::StreamExt;
use ovim::key_convert::{convert_key_event, convert_mouse_event};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration, Instant};

use ovim::api::ApiRequest;
use ovim::editor::{handle_mouse_event, Editor, InputHandler};
use ovim::frontend::{
    handle_viewport_resize, process_editor_tick, process_external_file_change,
    process_picker_results, refresh_after_input, FrontendChannels,
};
use ovim::session::SessionInfo;
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

/// Headless (API-only) event loop.
pub async fn run_headless_loop(
    editor: &mut Editor,
    mut api_rx: mpsc::Receiver<ApiRequest>,
    java_status_rx: mpsc::Receiver<String>,
    start_time: SystemTime,
    session_info: Arc<Mutex<SessionInfo>>,
    initial_dimensions: (u16, u16),
    mut shutdown_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let mut channels = FrontendChannels::new(java_status_rx);
    let mut lsp_interval = interval(Duration::from_millis(50));
    lsp_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Reused across `GetRender` requests so identical-dimension polls
    // skip the full ratatui+highlight pipeline (OV-00181).
    let mut render_cache = ovim::ui::AnsiRenderCache::new();
    let mut last_edit = Instant::now();
    let mut last_external_file_check = Instant::now();

    // A TUI paints its first frame before a person can type. Establish the
    // same layout and viewport contract before accepting headless requests.
    handle_viewport_resize(editor, initial_dimensions.0, initial_dimensions.1);
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
            Some((path, cache)) = channels.preview_rx.recv() => {
                editor.insert_preview(path, cache);
                editor.mark_dirty();
            }
            Some(result) = channels.file_rx.recv() => {
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
                process_editor_tick(editor, &mut channels).await;
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
                handle_viewport_resize(editor, w, h);
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
    java_status_rx: mpsc::Receiver<String>,
    start_time: SystemTime,
) -> Result<()> {
    let mut last_edit = Instant::now();
    let debounce_delay = Duration::from_millis(200);
    let mut last_input_time: Option<Instant> = None;
    let mut channels = FrontendChannels::new(java_status_rx);

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
                process_editor_tick(editor, &mut channels).await;
                process_picker_results(editor, &mut channels);
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

#[cfg(test)]
mod tests {
    use super::emit_new_agent_attention;
    use super::process_input_events;
    use super::refresh_after_input;
    use crate::api_dispatch::handle_edit_line;
    use crate::api_dispatch::spawn_agent_control;
    use crate::api_dispatch::{create_snapshot, create_snapshot_with_dimensions};
    use ovim::api::AgentControlTarget;
    use ovim::api::SNAPSHOT_SCHEMA_VERSION;
    use ovim::api::{ApiRequest, ApiResponse};
    use ovim::editor::{Editor, InputHandler, PreparedHeadlessAgentControl};
    use ovim::frontend::handle_viewport_resize;
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
        handle_viewport_resize(&mut direct, dimensions.0, dimensions.1);
        handle_viewport_resize(&mut via_api, dimensions.0, dimensions.1);

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
