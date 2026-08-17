//! API request dispatch: the bridge between the HTTP/MCP server tasks and
//! the single-threaded editor state.
//!
//! `handle_api_request` receives `ApiRequest`s from the Axum server (via the
//! channel wired up in the event loops), mutates the editor on the main
//! thread, and answers through the request's oneshot channel. The snapshot
//! builders serialize editor state for `/v1/snapshot` and friends.
//!
//! Split out of `event_loop.rs` (OV-00302): the loops own scheduling and
//! terminal I/O; this module owns the API surface's semantics. Line-based
//! edit requests delegate to `ovim::edit_engine`, which file mode shares
//! (OV-00298).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::time::Duration;

use ovim::api::{
    parse_key_string, AgentArtifactsResponse, AgentControlResponse, AgentControlTarget,
    AgentEventsResponse, ApiRequest, ApiResponse, BufferInfo, CursorPosition, DecorationInfo,
    DiagnosticCounts, DiagnosticItem, DiagnosticsInfo, EditorSnapshot, ErrorResponse, HealthInfo,
    LineEntry, LinesResponse, LspServerInfoItem, LspStatusInfo, ModeInfo, PickerInfo,
    PickerResultInfo, RenderInfo, SuccessResponse, ViewSnapshot, VisualSelection,
    AGENT_API_SCHEMA_VERSION, SNAPSHOT_SCHEMA_VERSION,
};
use ovim::editor::{self, Editor, InputHandler};
use ovim::mode::Mode;
use ovim::session::SessionInfo;

use ovim::frontend::{handle_viewport_resize, refresh_after_api_mutation, refresh_after_input};

pub(crate) async fn handle_api_request(
    editor: &mut Editor,
    request: ApiRequest,
    start_time: SystemTime,
    session_info: &Arc<Mutex<SessionInfo>>,
    render_cache: &mut ovim::ui::AnsiRenderCache,
) {
    match request {
        ApiRequest::GetSnapshot(tx) => {
            let dimensions = session_info.lock().ok().and_then(|info| info.dimensions());
            let snapshot = create_snapshot_with_dimensions(editor, dimensions);
            let _ = tx.send(ApiResponse::Snapshot(snapshot));
        }
        ApiRequest::GetSnapshotLight(tx) => {
            let dimensions = session_info.lock().ok().and_then(|info| info.dimensions());
            let snapshot = create_snapshot_light(editor, dimensions);
            let _ = tx.send(ApiResponse::Snapshot(snapshot));
        }
        ApiRequest::SendKeys(keys, tx) => {
            let events_result = parse_key_string(&keys);
            let response = match events_result {
                Ok(events) => {
                    let mut input_error = None;

                    for event in events {
                        if let Err(error) = InputHandler::handle_key_event_no_dirty(editor, event) {
                            input_error = Some(error.to_string());
                            break;
                        }
                    }

                    refresh_after_input(editor);

                    // Process any LSP actions that were triggered by the keys
                    editor.dispatch_pending_intents().await;

                    // If a hover request was just spawned, wait for the LSP to respond
                    // so the caller gets the result instead of null
                    if editor.has_pending_hover() {
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                        while editor.has_pending_hover() {
                            if tokio::time::Instant::now() >= deadline {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                            editor.poll_pending_lsp_responses();
                        }
                    }

                    if input_error.is_none() {
                        // Create context window showing the result of the key operation
                        let buffer = editor.buffer();
                        let cursor = buffer.cursor();
                        let buffer_content = buffer.rope().to_string();
                        let file_path = buffer.file_path();
                        let mode_str = editor.mode().display_name().to_string();

                        let context_str = ovim::api::format_context_window(
                            &buffer_content,
                            cursor.line(),
                            cursor.col().0,
                            file_path,
                            &mode_str,
                        );

                        let context_info = ovim::api::ContextWindowInfo {
                            context: context_str,
                            file: file_path.map(|s| s.to_string()),
                            mode: mode_str,
                            line: cursor.line(),
                            column: cursor.col().0,
                        };

                        ApiResponse::SendKeysResult(ovim::api::SendKeysResult {
                            success: true,
                            message: None,
                            context: context_info,
                        })
                    } else {
                        ApiResponse::Error(ErrorResponse {
                            error: format!(
                                "Failed to process keys: {}",
                                input_error.as_deref().unwrap_or("unknown input error")
                            ),
                        })
                    }
                }
                Err(parse_error) => ApiResponse::Error(ErrorResponse {
                    error: format!("Failed to parse keys: {}", parse_error),
                }),
            };
            let _ = tx.send(response);
        }
        ApiRequest::Paste(text, tx) => {
            let response = match editor.handle_paste_event(&text) {
                Ok(()) => {
                    refresh_after_input(editor);
                    editor.dispatch_pending_intents().await;
                    ApiResponse::Success(SuccessResponse {
                        success: true,
                        message: Some("Pasted text".into()),
                        line_count: Some(editor.buffer().rope().len_lines()),
                    })
                }
                Err(error) => ApiResponse::Error(ErrorResponse {
                    error: format!("Failed to paste text: {error}"),
                }),
            };
            let _ = tx.send(response);
        }
        ApiRequest::Resize { width, height, tx } => {
            handle_viewport_resize(editor, width, height);
            editor.mark_dirty();
            if let Ok(mut session) = session_info.lock() {
                if session.port != 0 {
                    let _ = session.set_dimensions(width, height);
                }
            }
            let response = ApiResponse::Success(SuccessResponse {
                success: true,
                message: Some(format!("Resized to {width}x{height}").into()),
                line_count: None,
            });
            let _ = tx.send(response);
        }
        ApiRequest::GetBuffer(tx) => {
            let buffer_info = create_buffer_info(editor);
            let _ = tx.send(ApiResponse::Buffer(buffer_info));
        }
        ApiRequest::SetBuffer(content, tx) => {
            editor.buffer_mut().replace_all(&content);
            refresh_after_api_mutation(editor, true);
            let line_count = editor.buffer().rope().len_lines();

            let response = ApiResponse::Success(SuccessResponse {
                success: true,
                message: None,
                line_count: Some(line_count),
            });

            let _ = tx.send(response);
        }
        ApiRequest::GetCursor(tx) => {
            let cursor = editor.buffer().cursor();
            let pos = CursorPosition {
                line: cursor.line(),
                column: cursor.col().0,
            };
            let _ = tx.send(ApiResponse::Cursor(pos));
        }
        ApiRequest::GetMode(tx) => {
            let mode_info = ModeInfo {
                mode: editor.mode().display_name().to_string(),
            };
            let _ = tx.send(ApiResponse::Mode(mode_info));
        }
        ApiRequest::SetMode(mode_str, tx) => {
            let new_mode = match mode_str.to_uppercase().as_str() {
                "NORMAL" => Mode::Normal,
                "INSERT" => Mode::Insert,
                "VISUAL" => Mode::Visual,
                "VISUAL_LINE" => Mode::VisualLine,
                "VISUAL_BLOCK" => Mode::VisualBlock,
                "COMMAND" => Mode::Command,
                "SEARCH" => Mode::Search,
                "PICKER" => Mode::Picker,
                "AI_CHAT" => Mode::AiChat,
                _ => {
                    let _ = tx.send(ApiResponse::Error(ErrorResponse {
                        error: format!("Invalid mode: {}. Valid modes: NORMAL, INSERT, VISUAL, VISUAL_LINE, VISUAL_BLOCK, COMMAND, SEARCH, PICKER, AI_PROMPT, AI_CHAT", mode_str),
                    }));
                    return;
                }
            };

            let old_mode = editor.mode();
            if old_mode == Mode::Insert && new_mode != Mode::Insert {
                editor.finalize_change_building();
            } else if old_mode != Mode::Insert && new_mode == Mode::Insert {
                editor.start_change_building(editor.cursor_position());
            }
            editor.set_mode(new_mode);
            editor.mark_dirty();
            let _ = tx.send(ApiResponse::Success(SuccessResponse {
                success: true,
                message: Some(format!("Mode set to {}", mode_str.to_uppercase()).into()),
                line_count: None,
            }));
        }
        ApiRequest::ExecuteCommand(command, tx) => {
            // Route through the full interactive dispatcher so headless `exec`
            // has parity with the command line (substitute, global, ranges, …),
            // not just the standard commands module.
            let response: ApiResponse = InputHandler::execute_command_api(editor, &command).into();
            refresh_after_input(editor);
            let _ = tx.send(response);
        }
        ApiRequest::GetRender {
            width,
            height,
            plain,
            tx,
        } => match render_cache.render(editor, width, height, plain) {
            Ok(output) => {
                let render_info = RenderInfo {
                    width,
                    height,
                    ansi: output,
                };
                let _ = tx.send(ApiResponse::Render(render_info));
            }
            Err(e) => {
                let _ = tx.send(ApiResponse::Error(ErrorResponse {
                    error: format!("Failed to render: {}", e),
                }));
            }
        },
        ApiRequest::GetLspStatus(tx) => {
            // Get LSP status from the editor's LSP manager
            if let Some(lsp_manager_arc) = editor.lsp_manager() {
                let servers = lsp_manager_arc.get_lsp_status().await;

                let lsp_status_info = LspStatusInfo {
                    servers: servers
                        .into_iter()
                        .map(|s| LspServerInfoItem {
                            language: s.language,
                            command: s.command,
                            state: s.state,
                            pending_requests: s.pending_requests,
                            has_capabilities: s.has_capabilities,
                        })
                        .collect(),
                    progress: editor.lsp_progress_message(),
                };

                let _ = tx.send(ApiResponse::LspStatus(lsp_status_info));
            } else {
                // No LSP manager available
                let lsp_status_info = LspStatusInfo {
                    servers: vec![],
                    progress: editor.lsp_progress_message(),
                };
                let _ = tx.send(ApiResponse::LspStatus(lsp_status_info));
            }
        }
        ApiRequest::GetHealth(tx) => {
            // Calculate uptime
            let uptime = start_time.elapsed().unwrap_or_default().as_secs();

            // Get file being edited
            let file = editor.buffer().file_path().map(|p| p.to_string());

            // Get LSP server statuses
            let mut lsp_servers = HashMap::new();
            if let Some(lsp_manager_arc) = editor.lsp_manager() {
                let servers = lsp_manager_arc.get_lsp_status().await;

                for server in servers {
                    let state = if server.has_capabilities {
                        "ready"
                    } else if server.state.contains("Initializing") {
                        "initializing"
                    } else {
                        "unknown"
                    };
                    lsp_servers.insert(server.language, state.to_string());
                }
            }

            // Determine if the system is ready
            let ready = lsp_servers.values().all(|s| s == "ready") || lsp_servers.is_empty();

            // Update session info with LSP ready status. Sessions are opt-in:
            // port 0 marks the placeholder used when no session is registered
            // (plain TUI), and writing it would fabricate a phantom session
            // file on disk. Same gate as the Resize handler.
            if let Ok(mut session) = session_info.lock() {
                if session.port != 0 {
                    let _ = session.set_lsp_ready(ready);
                } else {
                    session.lsp_ready = ready;
                }
            }

            let health_info = HealthInfo {
                status: "healthy".to_string(),
                uptime_seconds: uptime,
                file,
                lsp_servers,
                ready,
            };

            let _ = tx.send(ApiResponse::Health(health_info));
        }
        ApiRequest::GetMetrics(tx) => {
            // Get memory usage (approximate)
            let buffer = editor.buffer();
            let buffer_byte_size = buffer.rope().len_bytes();
            let buffer_line_count = buffer.rope().len_lines();

            // Memory usage estimate in MB (rough approximation)
            let memory_usage_mb = (buffer_byte_size as f64) / (1024.0 * 1024.0);

            let metrics_info = ovim::api::MetricsInfo {
                buffer_line_count,
                buffer_byte_size,
                syntax_enabled: buffer.has_syntax_highlighting(),
                is_large_file: buffer_line_count > 50_000, // Threshold for "large file"
                render_count: editor.render_count(),
                last_render_duration_micros: editor.last_render_duration_micros(),
                last_syntax_duration_micros: editor.last_syntax_duration_micros(),
                memory_usage_mb,
                // Input latency percentiles
                input_latency_p50_micros: editor.input_latency_p50_micros(),
                input_latency_p95_micros: editor.input_latency_p95_micros(),
                input_latency_p99_micros: editor.input_latency_p99_micros(),
                input_latency_samples: editor.input_latency_sample_count(),
                // Operation timings
                last_lsp_serialize_micros: editor.last_lsp_serialize_micros(),
                last_git_status_micros: editor.last_git_status_micros(),
                last_fold_calc_micros: editor.last_fold_calc_micros(),
                last_diagnostic_query_micros: editor.last_diagnostic_query_micros(),
            };

            let _ = tx.send(ApiResponse::Metrics(metrics_info));
        }
        ApiRequest::GetOutline(tx) => {
            let info = editor.get_outline().await;
            let _ = tx.send(ApiResponse::Outline(info));
        }
        ApiRequest::SearchSymbol(query, tx) => {
            let info = editor.search_symbols(&query).await;
            let _ = tx.send(ApiResponse::SymbolSearch(info));
        }
        ApiRequest::GetTrace(tx) => {
            let info = editor.get_trace().await;
            let _ = tx.send(ApiResponse::Trace(info));
        }
        ApiRequest::GetDiagnostics(tx) => {
            let file = editor.buffer().file_path().map(|s| s.to_string());
            let raw_diagnostics = editor.all_diagnostics();
            let (errors, warnings, info_count, hints) = editor.cached_diagnostic_count();

            let diagnostics: Vec<DiagnosticItem> = raw_diagnostics
                .iter()
                .map(|d| {
                    let severity = match d.severity {
                        Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
                        Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
                        Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                        Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
                        _ => "unknown",
                    };
                    let code = d.code.as_ref().map(|c| match c {
                        lsp_types::NumberOrString::Number(n) => n.to_string(),
                        lsp_types::NumberOrString::String(s) => s.clone(),
                    });
                    DiagnosticItem {
                        line: d.range.start.line as usize + 1,
                        column: d.range.start.character as usize + 1,
                        end_line: d.range.end.line as usize + 1,
                        end_column: d.range.end.character as usize + 1,
                        severity: severity.to_string(),
                        message: d.message.clone(),
                        source: d.source.clone(),
                        code,
                    }
                })
                .collect();

            let info = DiagnosticsInfo {
                file,
                diagnostics,
                counts: DiagnosticCounts {
                    errors,
                    warnings,
                    info: info_count,
                    hints,
                },
            };
            let _ = tx.send(ApiResponse::Diagnostics(info));
        }
        ApiRequest::GetContextWindow(tx) => {
            let buffer = editor.buffer();
            let cursor = buffer.cursor();
            let cursor_line = cursor.line();
            let cursor_column = cursor.col().0;

            let buffer_content = buffer.rope().to_string();
            let file_path = buffer.file_path();
            let mode_str = editor.mode().display_name().to_string();

            let context_str = ovim::api::format_context_window(
                &buffer_content,
                cursor_line,
                cursor_column,
                file_path,
                &mode_str,
            );

            let context_info = ovim::api::ContextWindowInfo {
                context: context_str,
                file: file_path.map(|s| s.to_string()),
                mode: mode_str,
                line: cursor_line,
                column: cursor_column,
            };

            let _ = tx.send(ApiResponse::ContextWindow(context_info));
        }
        ApiRequest::EditLine { line, old, new, tx } => {
            let response = handle_edit_line(editor, line, &old, &new);
            if matches!(&response, ApiResponse::Success(_)) {
                refresh_after_api_mutation(editor, false);
            }
            let _ = tx.send(response);
        }
        ApiRequest::InsertLines {
            line,
            before,
            text,
            tx,
        } => {
            let response = handle_insert_lines(editor, line, before, &text);
            if matches!(&response, ApiResponse::Success(_)) {
                refresh_after_api_mutation(editor, false);
            }
            let _ = tx.send(response);
        }
        ApiRequest::DeleteLines { from, to, tx } => {
            let response = handle_delete_lines(editor, from, to);
            if matches!(&response, ApiResponse::Success(_)) {
                refresh_after_api_mutation(editor, false);
            }
            let _ = tx.send(response);
        }
        ApiRequest::ReadLines { from, to, tx } => {
            let response = handle_read_lines(editor, from, to);
            let _ = tx.send(response);
        }
        ApiRequest::GetAgents { run_id, tx } => {
            let response = editor
                .ai_agent_snapshot(&run_id)
                .map(ApiResponse::Agents)
                .unwrap_or_else(api_agent_error);
            let _ = tx.send(response);
        }
        ApiRequest::GetAgent {
            run_id,
            agent_id,
            tx,
        } => {
            let response = editor
                .ai_agent_snapshot(&run_id)
                .and_then(|snapshot| {
                    snapshot
                        .agents
                        .into_iter()
                        .find(|agent| agent.agent_id == agent_id)
                        .ok_or_else(|| format!("agent {agent_id} does not belong to run {run_id}"))
                })
                .map(ApiResponse::Agent)
                .unwrap_or_else(api_agent_error);
            let _ = tx.send(response);
        }
        ApiRequest::GetAgentEvents {
            run_id,
            agent_id,
            after_sequence,
            limit,
            tx,
        } => {
            let response = editor
                .ai_agent_events(&run_id, &agent_id, after_sequence, limit)
                .map(|events| {
                    ApiResponse::AgentEvents(AgentEventsResponse {
                        schema_version: AGENT_API_SCHEMA_VERSION,
                        run_id,
                        agent_id,
                        after_sequence,
                        events,
                    })
                })
                .unwrap_or_else(api_agent_error);
            let _ = tx.send(response);
        }
        ApiRequest::GetAgentArtifacts {
            run_id,
            agent_id,
            tx,
        } => {
            let response = editor
                .ai_agent_snapshot(&run_id)
                .and_then(|snapshot| {
                    snapshot
                        .agents
                        .into_iter()
                        .find(|agent| agent.agent_id == agent_id)
                        .map(|agent| agent.artifact_handles)
                        .ok_or_else(|| format!("agent {agent_id} does not belong to run {run_id}"))
                })
                .map(|artifacts| {
                    ApiResponse::AgentArtifacts(AgentArtifactsResponse {
                        schema_version: AGENT_API_SCHEMA_VERSION,
                        run_id,
                        agent_id,
                        artifacts,
                    })
                })
                .unwrap_or_else(api_agent_error);
            let _ = tx.send(response);
        }
        ApiRequest::WaitAgent {
            target,
            timeout_millis,
            tx,
        } => {
            let prepared = editor.prepare_ai_agent_wait(
                &target.run_id,
                &target.agent_id,
                target.turn_generation,
                Duration::from_millis(timeout_millis),
            );
            spawn_agent_control(prepared, target, tx);
        }
        ApiRequest::InterruptAgent { target, reason, tx } => {
            let prepared = editor.prepare_ai_agent_interrupt(
                &target.run_id,
                &target.agent_id,
                target.turn_generation,
                reason,
            );
            spawn_agent_control(prepared, target, tx);
        }
        ApiRequest::SendAgentMessage {
            target,
            parent_agent_id,
            causing_turn_id,
            caused_by_event_id,
            message,
            tx,
        } => {
            let result = editor.ai_agent_send_message(
                &target.run_id,
                &target.agent_id,
                target.turn_generation,
                parent_agent_id,
                causing_turn_id,
                caused_by_event_id,
                message,
            );
            let _ = tx.send(agent_control_response(&target, result));
        }
        ApiRequest::FollowupAgent {
            target,
            parent_agent_id,
            causing_turn_id,
            caused_by_event_id,
            objective,
            tx,
        } => {
            let prepared = editor.prepare_ai_agent_followup(
                &target.run_id,
                &target.agent_id,
                target.turn_generation,
                parent_agent_id,
                causing_turn_id,
                caused_by_event_id,
                objective,
            );
            spawn_agent_control(prepared, target, tx);
        }
        ApiRequest::DecideAgentApproval {
            target,
            request_event_id,
            allow,
            reason,
            tx,
        } => {
            let result = editor.ai_agent_respond_approval(
                &target.run_id,
                &target.agent_id,
                target.turn_generation,
                target.operation_id.clone(),
                request_event_id,
                allow,
                reason,
            );
            let _ = tx.send(agent_control_response(&target, result));
        }
    }
}

pub(crate) fn spawn_agent_control(
    prepared: Result<ovim::editor::PreparedHeadlessAgentControl, String>,
    target: AgentControlTarget,
    tx: tokio::sync::oneshot::Sender<ApiResponse>,
) {
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = tx.send(api_agent_error(error));
            return;
        }
    };
    tokio::spawn(async move {
        let result = prepared.execute().await;
        let _ = tx.send(agent_control_response(&target, result));
    });
}

fn agent_control_response(
    target: &AgentControlTarget,
    result: Result<serde_json::Value, String>,
) -> ApiResponse {
    result
        .map(|result| {
            ApiResponse::AgentControl(AgentControlResponse {
                schema_version: AGENT_API_SCHEMA_VERSION,
                run_id: target.run_id.clone(),
                agent_id: target.agent_id.clone(),
                operation_id: target.operation_id.clone(),
                result,
            })
        })
        .unwrap_or_else(api_agent_error)
}

fn api_agent_error(error: String) -> ApiResponse {
    ApiResponse::Error(ErrorResponse { error })
}

/// Handle edit-line API request: find and replace text on a specific line
/// or the whole buffer.
///
/// Matching, validation, and error wording are shared with file mode via
/// `ovim::edit_engine` (OV-00298) — the plan's char offsets apply directly
/// to the rope. `line` arrives 0-indexed from the API layer; the engine
/// speaks 1-indexed CLI lines.
pub(crate) fn handle_edit_line(
    editor: &mut Editor,
    line: Option<usize>,
    old: &str,
    new: &str,
) -> ApiResponse {
    let content = editor.buffer().rope().to_string();
    let (splice, _match_line) =
        match ovim::edit_engine::plan_edit(&content, line.map(|l| l + 1), old, new) {
            Ok(plan) => plan,
            Err(error) => {
                return ApiResponse::Error(ErrorResponse {
                    error: error.to_string(),
                })
            }
        };

    let rope = editor.buffer().rope();
    let match_line = rope.char_to_line(splice.start_char);
    let match_col_chars = splice.start_char - rope.line_to_char(match_line);
    let end_line = rope.char_to_line(splice.end_char);
    let end_col_chars = splice.end_char - rope.line_to_char(end_line);

    // Capture grapheme prefix length on the *pre-edit* line so cursor_after
    // can be computed in grapheme-space without re-scanning the post-edit rope.
    let pre_edit_content = ovim_core::display::line_content(rope, match_line);
    let prefix_text: String = pre_edit_content.chars().take(match_col_chars).collect();
    let prefix_graphemes = ovim_core::unicode::grapheme_count(&prefix_text);

    // Record cursor position before change (grapheme-space)
    let cursor_before = {
        let c = editor.buffer().cursor();
        ovim::editor::CursorPos::new(c.line(), c.col())
    };

    // Perform the edit (delete + insert) inside a `record()` session so the
    // edits land on `edit_log` and feed a single `Change::Recorded` undo
    // entry. Mark buffer modified so LSP didChange fires.
    let ((), edits) = editor.buffer_mut().record(|buf| {
        buf.delete_range(
            match_line,
            ovim_core::unicode::CharCol(match_col_chars),
            end_line,
            ovim_core::unicode::CharCol(end_col_chars),
        );
        buf.insert_text_at(
            match_line,
            ovim_core::unicode::CharCol(match_col_chars),
            new,
        );
    });

    if !edits.is_empty() {
        let (cursor_line_after, cursor_grapheme_col) = match new.rfind('\n') {
            Some(pos) => (
                match_line + new.matches('\n').count(),
                ovim_core::unicode::grapheme_count(&new[pos + 1..]),
            ),
            None => (
                match_line,
                prefix_graphemes + ovim_core::unicode::grapheme_count(new),
            ),
        };
        let cursor_after = ovim::editor::CursorPos::new(
            cursor_line_after,
            ovim_core::unicode::GraphemeCol(cursor_grapheme_col),
        );
        editor.push_recorded_undo(edits, cursor_before, cursor_after);
    }

    ApiResponse::Success(SuccessResponse {
        success: true,
        message: Some(format!("Replaced on line {}", match_line + 1).into()),
        line_count: Some(editor.buffer().rope().len_lines()),
    })
}

/// Handle insert-lines API request: insert text before a specific line.
///
/// `line` is the 0-indexed insert position (== "after 1-indexed line N"),
/// which is exactly the engine's `InsertAt::After(N)`. Position math,
/// newline policy (OV-00279), and line-ending style are shared with file
/// mode via `ovim::edit_engine` (OV-00298).
fn handle_insert_lines(editor: &mut Editor, line: usize, _before: bool, text: &str) -> ApiResponse {
    let content = editor.buffer().rope().to_string();
    let (splice, _count) = match ovim::edit_engine::plan_insert(
        &content,
        ovim::edit_engine::InsertAt::After(line),
        text,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            return ApiResponse::Error(ErrorResponse {
                error: error.to_string(),
            })
        }
    };

    let cursor_before = {
        let c = editor.buffer().cursor();
        ovim::editor::CursorPos::new(c.line(), c.col())
    };

    // Convert the splice's char offset to line/col for insert_text_at
    let rope = editor.buffer().rope();
    let ins_line = rope.char_to_line(splice.start_char);
    let ins_col = splice.start_char - rope.line_to_char(ins_line);

    // Record change for undo via `buffer.record()` + `push_recorded_undo`.
    let ((), edits) = editor.buffer_mut().record(|buf| {
        buf.insert_text_at(ins_line, ovim_core::unicode::CharCol(ins_col), &splice.text);
    });

    if !edits.is_empty() {
        let cursor_after = {
            let c = editor.buffer().cursor();
            ovim::editor::CursorPos::new(c.line(), c.col())
        };
        editor.push_recorded_undo(edits, cursor_before, cursor_after);
    }

    ApiResponse::Success(SuccessResponse {
        success: true,
        message: Some(format!("Inserted at line {}", line + 1).into()),
        line_count: Some(editor.buffer().rope().len_lines()),
    })
}

/// Handle delete-lines API request: delete a range of lines (0-indexed,
/// inclusive). Validation and range math are shared with file mode via
/// `ovim::edit_engine` (OV-00298) — including strict rejection of
/// past-the-end ranges, which this handler previously clamped.
fn handle_delete_lines(editor: &mut Editor, from: usize, to: usize) -> ApiResponse {
    let content = editor.buffer().rope().to_string();
    let (splice, _count) = match ovim::edit_engine::plan_delete_lines(&content, from + 1, to + 1) {
        Ok(plan) => plan,
        Err(error) => {
            return ApiResponse::Error(ErrorResponse {
                error: error.to_string(),
            })
        }
    };

    let cursor_before = {
        let c = editor.buffer().cursor();
        ovim::editor::CursorPos::new(c.line(), c.col())
    };

    // Convert the splice's char offsets to line/col for delete_range
    let rope = editor.buffer().rope();
    let start_line = rope.char_to_line(splice.start_char);
    let start_col = splice.start_char - rope.line_to_char(start_line);
    let end_line = rope.char_to_line(splice.end_char);
    let end_col = splice.end_char - rope.line_to_char(end_line);

    // Record delete via `buffer.record()` + `push_recorded_undo`.
    let (_deleted, edits) = editor.buffer_mut().record(|buf| {
        buf.delete_range(
            start_line,
            ovim_core::unicode::CharCol(start_col),
            end_line,
            ovim_core::unicode::CharCol(end_col),
        )
    });

    // Adjust cursor if it was in deleted range
    let new_total = editor.buffer().rope().len_lines();
    let cursor = editor.buffer().cursor();
    if cursor.line() >= new_total && new_total > 0 {
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(new_total - 1, ovim_core::unicode::GraphemeCol::ZERO);
    }

    if !edits.is_empty() {
        let cursor_after = {
            let c = editor.buffer().cursor();
            ovim::editor::CursorPos::new(c.line(), c.col())
        };
        editor.push_recorded_undo(edits, cursor_before, cursor_after);
    }

    ApiResponse::Success(SuccessResponse {
        success: true,
        message: Some(format!("Deleted lines {}-{}", from + 1, to + 1).into()),
        line_count: Some(new_total),
    })
}

/// Handle read-lines API request: read a range of lines (0-indexed, inclusive)
fn handle_read_lines(editor: &Editor, from: usize, to: usize) -> ApiResponse {
    let rope = editor.buffer().rope();
    let total_lines = rope.len_lines();

    if from >= total_lines {
        return ApiResponse::Error(ErrorResponse {
            error: format!(
                "Line {} out of range (buffer has {} lines)",
                from + 1,
                total_lines
            ),
        });
    }

    let to = to.min(total_lines.saturating_sub(1));

    let mut lines = Vec::new();
    for idx in from..=to {
        lines.push(LineEntry {
            number: idx + 1, // 1-indexed for display
            text: ovim_core::display::line_content(rope, idx),
        });
    }

    ApiResponse::Lines(LinesResponse { lines, total_lines })
}

#[cfg(test)]
pub(crate) fn create_snapshot(editor: &Editor) -> EditorSnapshot {
    create_snapshot_with_dimensions(editor, None)
}

fn create_view_snapshot(editor: &Editor, dimensions: Option<(u16, u16)>) -> ViewSnapshot {
    ViewSnapshot {
        viewport_width: dimensions.map(|(width, _)| width),
        viewport_height: dimensions
            .map(|(_, height)| height)
            .or_else(|| u16::try_from(editor.viewport_height()).ok()),
        scroll_offset: editor.scroll_offset(),
        scroll_subrow: editor.scroll_subrow(),
        tab_count: editor.tab_count(),
        current_tab: editor.current_tab_index(),
        window_count: editor.window_count(),
        file_tree_visible: editor.file_tree().is_visible(),
        command_line: editor.command_line().to_string(),
        command_cursor: editor.command_cursor(),
        search_query: editor.search_buffer().to_string(),
        search_forward: editor.search_forward(),
        status: editor.status_message().to_string(),
        active_session: editor.active_session().map(str::to_string),
    }
}

pub(crate) fn create_snapshot_with_dimensions(
    editor: &Editor,
    dimensions: Option<(u16, u16)>,
) -> EditorSnapshot {
    let buffer_info = create_buffer_info(editor);
    let cursor = editor.buffer().cursor();

    let cursor_pos = CursorPosition {
        line: cursor.line(),
        column: cursor.col().0,
    };

    let visual_selection =
        editor
            .visual_selection()
            .map(
                |((start_line, start_col), (end_line, end_col))| VisualSelection {
                    start: CursorPosition {
                        line: start_line,
                        column: start_col,
                    },
                    end: CursorPosition {
                        line: end_line,
                        column: end_col,
                    },
                },
            );

    // Get registers content
    let mut registers = HashMap::new();
    let reg_manager = editor.registers();
    for reg_name in &[
        '"', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
        'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y',
        'z',
    ] {
        let content = reg_manager.get(Some(*reg_name));
        if !content.is_empty() {
            registers.insert(reg_name.to_string(), content);
        }
    }

    // Get marks
    let mut marks = HashMap::new();
    let mark_manager = editor.marks();
    for (name, mark) in mark_manager.iter() {
        marks.insert(
            name.to_string(),
            CursorPosition {
                line: mark.line,
                column: mark.col,
            },
        );
    }

    // Get picker state if in picker mode
    let picker = editor.picker().map(|p| PickerInfo {
        mode: match p.mode() {
            editor::PickerMode::FindFiles => "FindFiles".to_string(),
            editor::PickerMode::LiveGrep => "LiveGrep".to_string(),
            editor::PickerMode::Custom => "Custom".to_string(),
            editor::PickerMode::Completion => "Completion".to_string(),
            editor::PickerMode::LspLocations => "LspLocations".to_string(),
        },
        query: p.query().to_string(),
        results: p
            .collect_filtered_results(usize::MAX)
            .into_iter()
            .map(|r| PickerResultInfo {
                display: r.display.clone(),
                location: r.location.clone(),
                line: r.line,
                col: r.col,
            })
            .collect(),
        selected_index: p.selected_index(),
        total_results: p.filtered_result_count(),
        loading: p.is_loading(),
    });

    // Project decorations into the snapshot. Phase-05 Step F: each stored
    // decoration holds a source-version `char_offset`; we project it through
    // `edit_log.edits_since(source_version)` so clients see the **live**
    // position (what the renderer would show), not the stale placement-time
    // anchor. `line` and `col` are derived from the projected offset against
    // the current rope. Decorations whose anchors were engulfed by a delete
    // since placement are dropped from the snapshot.
    let rope = editor.buffer().rope();
    let edit_log = editor.buffer().edit_log();
    let decorations: Vec<DecorationInfo> = editor
        .decorations
        .iter_all()
        .filter_map(|(stored_line, dec)| {
            use ovim_core::editor::decoration::{
                project_offset, DecorationPlacement, DecorationSource,
            };
            let stored_offset = dec.placement.char_offset();
            let projected_offset = match edit_log.edits_since(dec.source_version) {
                Some(edits) => match project_offset(stored_offset, &edits) {
                    Some(off) => off,
                    None => return None, // anchor engulfed by a delete
                },
                // History evicted — fall back to the stored offset. Stale is
                // better than blank; the next LSP refresh will replace it.
                None => stored_offset,
            };
            let clamped = projected_offset.min(rope.len_chars());
            let live_line = rope.char_to_line(clamped);
            let line_start = rope.line_to_char(live_line);
            let col = clamped - line_start;

            // Fall back to `stored_line` only if projection landed past EOF.
            let line = if projected_offset > rope.len_chars() {
                stored_line
            } else {
                live_line
            };

            let source = match dec.source {
                DecorationSource::InlayHint => "inlay_hint",
                DecorationSource::Diagnostic => "diagnostic",
            }
            .to_string();
            let placement = match dec.placement {
                DecorationPlacement::Inline { .. } => "inline",
                DecorationPlacement::EndOfLine { .. } => "eol",
            }
            .to_string();
            Some(DecorationInfo {
                line,
                char_offset: clamped,
                col,
                text: dec.text.clone(),
                source,
                placement,
                source_version: dec.source_version,
            })
        })
        .collect();

    EditorSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        buffer: buffer_info,
        cursor: cursor_pos,
        mode: editor.mode().display_name().to_string(),
        visual_selection,
        registers,
        marks,
        picker,
        hover_info: editor.hover_info().map(|s| s.to_string()),
        ai_chat: create_ai_chat_snapshot(editor),
        decorations,
        view: create_view_snapshot(editor, dimensions),
    }
}

fn create_ai_chat_snapshot(editor: &Editor) -> Option<ovim::api::AiChatSnapshot> {
    use ovim::api::{
        AiChatMessageSnapshot, AiChatSnapshot, CodeExplanationSnapshot, QueuedChatSnapshot,
        ToolCallSnapshot,
    };
    use ovim_core::ai::chat_types::{ChatFocus, ChatRole};
    use ovim_core::editor::QueuedChatInputKind;

    editor.ai_chat_state()?;
    let pending_approval = editor
        .ai_chat_pending_tool_approval_summary()
        .or_else(|| editor.ai_chat_pending_no_repo_folder_approval_summary());
    let queued = editor
        .ai_chat_queued_inputs()
        .map(|item| QueuedChatSnapshot {
            kind: match item.kind {
                QueuedChatInputKind::Steer => "steer",
                QueuedChatInputKind::FollowUp => "follow_up",
                QueuedChatInputKind::Command => "command",
            }
            .to_string(),
            content: item.content.clone(),
            images: item.images.iter().map(image_snapshot).collect(),
        })
        .collect();
    let messages = editor
        .ai_chat_messages()
        .iter()
        .map(|message| AiChatMessageSnapshot {
            role: match message.role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::Thinking => "thinking",
                ChatRole::Tool => "tool",
                ChatRole::Error => "error",
            }
            .to_string(),
            content: message.content.clone(),
            tool_call_id: message.tool_call_id.clone(),
            tool: message.tool_call_id.as_deref().and_then(|id| {
                let summary = editor.ai_chat_tool_event_summary(id)?;
                let expanded = editor.ai_chat_is_tool_event_expanded(id);
                Some(ToolCallSnapshot {
                    name: summary.call.name.clone(),
                    summary: summary.label.clone(),
                    expanded,
                    arguments: expanded.then(|| summary.call.arguments.clone()),
                })
            }),
            images: message.images.iter().map(image_snapshot).collect(),
        })
        .collect();
    Some(AiChatSnapshot {
        activity: editor.ai_chat_activity().as_str().to_string(),
        waiting: editor.ai_chat_waiting(),
        attention_generation: editor.ai_chat_attention_generation(),
        input: editor.ai_chat_input().to_string(),
        input_cursor: editor.ai_chat_input_cursor(),
        focus: match editor.ai_chat_focus() {
            ChatFocus::TextInput => "text_input",
            ChatFocus::MessageHistory => "message_history",
            ChatFocus::ModelSelector => "model_selector",
            ChatFocus::TreePanel => "tree_panel",
        }
        .to_string(),
        streaming: editor.ai_chat_is_streaming(),
        review_mode: editor.ai_chat_review_mode(),
        tree_panel_open: editor.ai_chat_tree_panel_open(),
        yolo_mode: editor.ai_chat_yolo_mode(),
        comprehension_policy: editor
            .ai_chat_comprehension_policy()
            .as_str()
            .to_string(),
        comprehension_checkpoint: editor
            .ai_chat_comprehension_checkpoint_summary()
            .map(str::to_string),
        pending_images: editor
            .ai_chat_pending_images()
            .iter()
            .map(image_snapshot)
            .collect(),
        pending_approval,
        pending_setup: editor
            .ai_chat_exa_setup_summary()
            .map(|(_, _, error, environment_override)| {
                let source = if environment_override {
                    " EXA_API_KEY is currently taking precedence."
                } else {
                    ""
                };
                let error = error
                    .map(|message| format!(" Error: {message}"))
                    .unwrap_or_default();
                format!(
                    "Exa web search setup. Add a key from {} or dismiss with Escape; reopen later with /exa.{source}{error}",
                    editor.ai_chat_exa_dashboard_url()
                )
            }),
        code_explanation: editor
            .ai_code_explanation_view()
            .map(|view| {
                let (discussion_state, question_count, question, answer, draft) =
                    match view.discussion {
                        ovim_core::editor::CodeExplanationDiscussionView::Navigating {
                            question_count,
                            latest_question,
                            latest_answer,
                            ..
                        } => (
                            "navigating".to_string(),
                            question_count,
                            latest_question,
                            latest_answer,
                            None,
                        ),
                        ovim_core::editor::CodeExplanationDiscussionView::Composing {
                            input,
                            question_count,
                            ..
                        } => (
                            "composing".to_string(),
                            question_count,
                            None,
                            None,
                            Some(input),
                        ),
                        ovim_core::editor::CodeExplanationDiscussionView::Answering {
                            question,
                            answer,
                            question_count,
                        } => (
                            "answering".to_string(),
                            question_count,
                            Some(question),
                            Some(answer),
                            None,
                        ),
                    };
                let (page_type, title, path, start_line, end_line, comment) = match view.page {
                    ovim_core::editor::CodeExplanationPageView::Concept { title, body } => {
                        ("concept".to_string(), Some(title), String::new(), 0, 0, body)
                    }
                    ovim_core::editor::CodeExplanationPageView::Code {
                        path,
                        start_line,
                        end_line,
                        comment,
                    } => (
                        "code".to_string(),
                        None,
                        path,
                        start_line,
                        end_line,
                        comment,
                    ),
                };
                CodeExplanationSnapshot {
                    current: view.current,
                    total: view.total,
                    page_type,
                    title,
                    path,
                    start_line,
                    end_line,
                    comment,
                    discussion_state,
                    question_count,
                    question,
                    answer,
                    draft,
                }
            }),
        queued,
        messages,
    })
}

fn image_snapshot(
    image: &ovim_core::ai::chat_types::ImageAttachment,
) -> ovim::api::ImageAttachmentSnapshot {
    ovim::api::ImageAttachmentSnapshot {
        path: image.path.to_string_lossy().to_string(),
        name: image.file_name(),
        mime_type: image.mime_type.clone(),
        size_bytes: image.data.len(),
    }
}

/// Lightweight snapshot: skips buffer content, registers, marks, and picker.
/// Used by MCP polling and other callers that only need mode/cursor/hover.
fn create_snapshot_light(editor: &Editor, dimensions: Option<(u16, u16)>) -> EditorSnapshot {
    let cursor = editor.buffer().cursor();
    let cursor_pos = CursorPosition {
        line: cursor.line(),
        column: cursor.col().0,
    };

    EditorSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        buffer: BufferInfo {
            content: String::new(),
            line_count: editor.buffer().rope().len_lines(),
            file_path: editor.buffer().file_path().map(|s| s.to_string()),
        },
        cursor: cursor_pos,
        mode: editor.mode().display_name().to_string(),
        visual_selection: None,
        registers: HashMap::new(),
        marks: HashMap::new(),
        picker: None,
        hover_info: editor.hover_info().map(|s| s.to_string()),
        ai_chat: create_ai_chat_snapshot(editor),
        // Lightweight snapshot deliberately omits decorations to keep polling
        // cheap; callers that need them should hit the full `/v1/snapshot`.
        decorations: Vec::new(),
        view: create_view_snapshot(editor, dimensions),
    }
}

fn create_buffer_info(editor: &Editor) -> BufferInfo {
    let buffer = editor.buffer();
    let rope = buffer.rope();

    // Write rope chunks directly into a pre-allocated String.
    // This avoids the intermediate allocations that rope.to_string() can cause.
    let byte_len = rope.len_bytes();
    let mut content = String::with_capacity(byte_len);
    for chunk in rope.chunks() {
        content.push_str(chunk);
    }

    let line_count = rope.len_lines();
    let file_path = buffer.file_path().map(|s| s.to_string());

    BufferInfo {
        content,
        line_count,
        file_path,
    }
}
