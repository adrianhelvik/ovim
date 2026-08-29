/// MCP JSON-RPC request handler
use super::mcp::{self, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use super::state::{ApiRequest, ApiState};
use axum::{
    extract::State,
    response::{IntoResponse, Json, Response},
    Json as JsonExtractor,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Handler for POST /mcp
/// Processes MCP JSON-RPC 2.0 requests
pub async fn handle_mcp(
    State(state): State<ApiState>,
    JsonExtractor(request): JsonExtractor<JsonRpcRequest>,
) -> Response {
    // Validate JSON-RPC version
    if request.jsonrpc != "2.0" {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id.clone(),
            result: None,
            error: Some(JsonRpcError::invalid_request()),
        };
        return Json(response).into_response();
    }

    // Process the request based on method
    let result = match request.method.as_str() {
        "initialize" => mcp::handle_initialize(request.params),
        "tools/list" => mcp::handle_tools_list(),
        "tools/call" => handle_tool_call(state, request.params).await,
        "resources/list" => {
            // Get current file path from editor
            let file_path = get_current_file_path(&state).await;
            mcp::handle_resources_list(file_path.as_deref())
        }
        "resources/read" => handle_resource_read(state, request.params).await,
        "prompts/list" => mcp::handle_prompts_list(),
        _ => Err(JsonRpcError::method_not_found(&request.method)),
    };

    // Build response
    let response = match result {
        Ok(result_value) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(result_value),
            error: None,
        },
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: None,
            error: Some(error),
        },
    };

    Json(response).into_response()
}

/// Handle tools/call method
async fn handle_tool_call(state: ApiState, params: Value) -> Result<Value, JsonRpcError> {
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing 'name' field"))?;

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    if let Some(session_name) = arguments
        .get("session")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        return forward_tool_call_to_session(&session_name, tool_name, arguments).await;
    }

    match tool_name {
        "send_keys" => {
            let keys = arguments
                .get("keys")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'keys' argument"))?;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SendKeys(keys.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::SendKeysResult(result) = response {
                        // Return the context window as the response
                        let json_str = serde_json::to_string_pretty(&result.context)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else if let super::state::ApiResponse::Error(err) = response {
                        Ok(mcp::tool_result(vec![mcp::error_content(&err.error)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to send keys")),
            }
        }
        "paste" => {
            let text = arguments
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'text' argument"))?;
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::Paste(text.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;
            match rx.await {
                Ok(super::state::ApiResponse::Success(_)) => {
                    Ok(mcp::tool_result(vec![mcp::text_content(
                        "Text pasted successfully",
                    )]))
                }
                Ok(super::state::ApiResponse::Error(error)) => {
                    Ok(mcp::tool_result(vec![mcp::error_content(&error.error)]))
                }
                Ok(_) => Err(JsonRpcError::internal_error("Unexpected response type")),
                Err(_) => Err(JsonRpcError::internal_error("Failed to paste text")),
            }
        }
        "resize" => {
            let width = arguments
                .get("width")
                .and_then(|v| v.as_u64())
                .and_then(|v| u16::try_from(v).ok())
                .ok_or_else(|| JsonRpcError::invalid_params("Invalid 'width' argument"))?;
            let height = arguments
                .get("height")
                .and_then(|v| v.as_u64())
                .and_then(|v| u16::try_from(v).ok())
                .ok_or_else(|| JsonRpcError::invalid_params("Invalid 'height' argument"))?;
            if !(10..=500).contains(&width) || !(3..=200).contains(&height) {
                return Err(JsonRpcError::invalid_params(
                    "Dimensions must be within 10x3 and 500x200",
                ));
            }
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::Resize { width, height, tx })
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;
            match rx.await {
                Ok(super::state::ApiResponse::Success(_)) => {
                    Ok(mcp::tool_result(vec![mcp::text_content(&format!(
                        "Resized to {width}x{height}"
                    ))]))
                }
                Ok(super::state::ApiResponse::Error(error)) => {
                    Ok(mcp::tool_result(vec![mcp::error_content(&error.error)]))
                }
                Ok(_) => Err(JsonRpcError::internal_error("Unexpected response type")),
                Err(_) => Err(JsonRpcError::internal_error("Failed to resize editor")),
            }
        }
        "get_buffer" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetBuffer(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Buffer(buffer) = response {
                        Ok(mcp::tool_result(vec![mcp::text_content(&buffer.content)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get buffer")),
            }
        }
        "set_buffer" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'content' argument"))?;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SetBuffer(content.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Success(_) = response {
                        Ok(mcp::tool_result(vec![mcp::text_content(
                            "Buffer set successfully",
                        )]))
                    } else if let super::state::ApiResponse::Error(err) = response {
                        Ok(mcp::tool_result(vec![mcp::error_content(&err.error)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to set buffer")),
            }
        }
        "get_cursor" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetCursor(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Cursor(cursor) = response {
                        let text = format!("Line: {}, Column: {}", cursor.line, cursor.column);
                        Ok(mcp::tool_result(vec![mcp::text_content(&text)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get cursor")),
            }
        }
        "execute_command" => {
            let command = arguments
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'command' argument"))?;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::ExecuteCommand(command.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Success(success) = response {
                        let msg = success
                            .message
                            .unwrap_or_else(|| "Command executed successfully".into());
                        Ok(mcp::tool_result(vec![mcp::text_content(&msg)]))
                    } else if let super::state::ApiResponse::Error(err) = response {
                        Ok(mcp::tool_result(vec![mcp::error_content(&err.error)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to execute command")),
            }
        }
        "lsp_hover" => {
            // Ensure NORMAL mode before triggering hover
            let (mode_tx, mode_rx) = oneshot::channel();
            let _ = state
                .tx
                .send(ApiRequest::SetMode("NORMAL".to_string(), mode_tx))
                .await;
            let _ = mode_rx.await;

            // Send K to trigger hover. SendKeys already blocks on
            // `has_pending_hover()` for up to 5s in the event loop
            // (see `ApiRequest::SendKeys` in event_loop.rs), so by the
            // time this returns the hover slot has either resolved
            // (mode=HoverPreview, hover_info populated) or timed out.
            //
            // Historically (OV-00183) we then polled GetSnapshotLight 10×
            // with a 100ms sleep between each — adding up to 1s of dead
            // wait and 10 channel round-trips on top of the work the
            // event loop had already done. A single snapshot read is
            // sufficient.
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SendKeys("K".to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;
            let _ = rx.await;

            let (snap_tx, snap_rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetSnapshotLight(snap_tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;
            // Only treat hover_info as fresh when mode actually switched
            // to HoverPreview. Otherwise hover_info may be stale from a
            // previous K, or the LSP returned no hover at this position.
            let hover_text = match snap_rx.await {
                Ok(super::state::ApiResponse::Snapshot(snapshot))
                    if snapshot.mode.contains("HOVER") =>
                {
                    snapshot.hover_info
                }
                _ => None,
            };

            // Dismiss hover popup, return to NORMAL mode
            let (esc_tx, esc_rx) = oneshot::channel();
            let _ = state
                .tx
                .send(ApiRequest::SetMode("NORMAL".to_string(), esc_tx))
                .await;
            let _ = esc_rx.await;

            match hover_text {
                Some(text) => Ok(mcp::tool_result(vec![mcp::text_content(&text)])),
                None => Ok(mcp::tool_result(vec![mcp::text_content(
                    "No hover information available at cursor position",
                )])),
            }
        }
        "lsp_goto_definition" => {
            // Ensure NORMAL mode before triggering goto definition
            let (mode_tx, mode_rx) = oneshot::channel();
            let _ = state
                .tx
                .send(ApiRequest::SetMode("NORMAL".to_string(), mode_tx))
                .await;
            let _ = mode_rx.await;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SendKeys("gd".to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::SendKeysResult(result) = response {
                        let json_str = serde_json::to_string_pretty(&result.context)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else if let super::state::ApiResponse::Error(err) = response {
                        Ok(mcp::tool_result(vec![mcp::error_content(&err.error)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error(
                    "Failed to trigger goto definition",
                )),
            }
        }
        "get_snapshot" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetSnapshot(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Snapshot(snapshot) = response {
                        let json_str = serde_json::to_string_pretty(&snapshot)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get snapshot")),
            }
        }
        "get_health" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetHealth(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Health(health) = response {
                        let json_str = serde_json::to_string_pretty(&health)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get health")),
            }
        }
        "get_lsp_status" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetLspStatus(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::LspStatus(lsp_status) = response {
                        let json_str = serde_json::to_string_pretty(&lsp_status)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get LSP status")),
            }
        }
        "list_sessions" => {
            // This tool doesn't require editor state, just return the list of all sessions
            match crate::session::SessionInfo::list_all() {
                Ok(sessions) => {
                    let json_str = serde_json::to_string_pretty(&sessions)
                        .unwrap_or_else(|_| "[]".to_string());
                    Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                }
                Err(e) => Err(JsonRpcError::internal_error(&format!(
                    "Failed to list sessions: {}",
                    e
                ))),
            }
        }
        "set_mode" => {
            let mode_str = arguments
                .get("mode")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'mode' argument"))?;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SetMode(mode_str.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Success(success) = response {
                        let msg = success
                            .message
                            .unwrap_or_else(|| format!("Mode set to {}", mode_str).into());
                        Ok(mcp::tool_result(vec![mcp::text_content(&msg)]))
                    } else if let super::state::ApiResponse::Error(err) = response {
                        Ok(mcp::tool_result(vec![mcp::error_content(&err.error)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to set mode")),
            }
        }
        "get_outline" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetOutline(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Outline(info) = response {
                        let json_str = serde_json::to_string_pretty(&info)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get outline")),
            }
        }
        "search_symbol" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError::invalid_params("Missing 'query' argument"))?;

            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::SearchSymbol(query.to_string(), tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::SymbolSearch(info) = response {
                        let json_str = serde_json::to_string_pretty(&info)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to search symbols")),
            }
        }
        "get_trace" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetTrace(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Trace(info) = response {
                        let json_str = serde_json::to_string_pretty(&info)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get trace")),
            }
        }
        "get_diagnostics" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetDiagnostics(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(super::state::ApiResponse::Diagnostics(info)) => {
                    let json_str =
                        serde_json::to_string_pretty(&info).unwrap_or_else(|_| "{}".to_string());
                    Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                }
                _ => Err(JsonRpcError::internal_error("Failed to get diagnostics")),
            }
        }
        "get_context_window" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetContextWindow(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::ContextWindow(ctx) = response {
                        let json_str =
                            serde_json::to_string_pretty(&ctx).unwrap_or_else(|_| "{}".to_string());
                        Ok(mcp::tool_result(vec![mcp::text_content(&json_str)]))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get context window")),
            }
        }
        _ => Err(JsonRpcError::invalid_params(&format!(
            "Unknown tool: {}",
            tool_name
        ))),
    }
}

/// How long to wait for a TCP connection to a forwarded session.
const FORWARD_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Total round-trip budget for a forwarded tool call. Tool calls can
/// legitimately take a while (LSP waits, renders), but a session whose
/// event loop is wedged must not hang the caller's MCP request forever.
const FORWARD_TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Build the HTTP client used to forward tool calls to another session.
fn forwarding_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(FORWARD_CONNECT_TIMEOUT)
        .timeout(FORWARD_TOTAL_TIMEOUT)
        .build()
}

async fn forward_tool_call_to_session(
    session_name: &str,
    tool_name: &str,
    mut arguments: Value,
) -> Result<Value, JsonRpcError> {
    // The session name flows into a file path inside the session directory;
    // reject traversal-capable names before touching the filesystem.
    crate::session::SessionInfo::validate_session_name(session_name)
        .map_err(|error| JsonRpcError::invalid_params(&error.to_string()))?;
    let session = crate::session::SessionInfo::read(session_name).map_err(|error| {
        JsonRpcError::invalid_params(&format!("Session '{session_name}' not found: {error}"))
    })?;
    if let Some(arguments) = arguments.as_object_mut() {
        arguments.remove("session");
    }
    let client = forwarding_client().map_err(|error| {
        JsonRpcError::internal_error(&format!("Failed to build forwarding HTTP client: {error}"))
    })?;
    let response = client
        .post(format!("http://127.0.0.1:{}/v1/mcp", session.port))
        .bearer_auth(session.capability.expose_secret())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": "ovim-session-forward",
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": arguments },
        }))
        .send()
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(&format!(
                "Failed to contact session '{session_name}': {error}"
            ))
        })?;
    let value: Value = response.json().await.map_err(|error| {
        JsonRpcError::internal_error(&format!(
            "Session '{session_name}' returned invalid MCP JSON: {error}"
        ))
    })?;
    if let Some(error) = value.get("error") {
        return Err(JsonRpcError {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603) as i32,
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Forwarded MCP request failed")
                .to_string(),
            data: error.get("data").cloned(),
        });
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| JsonRpcError::internal_error("Forwarded MCP response omitted result"))
}

/// Handle resources/read method
async fn handle_resource_read(state: ApiState, params: Value) -> Result<Value, JsonRpcError> {
    let uri = params
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing 'uri' field"))?;

    match uri {
        "ovim://context-window" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetContextWindow(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::ContextWindow(ctx) = response {
                        Ok(json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": "text/plain",
                                "text": ctx.context
                            }]
                        }))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get context window")),
            }
        }
        "ovim://buffer" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetBuffer(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Buffer(buffer) = response {
                        Ok(json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": "text/plain",
                                "text": buffer.content
                            }]
                        }))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get buffer")),
            }
        }
        "ovim://snapshot" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetSnapshot(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::Snapshot(snapshot) = response {
                        Ok(json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": "application/json",
                                "text": serde_json::to_string_pretty(&snapshot)
                                    .unwrap_or_else(|_| "{}".to_string())
                            }]
                        }))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get snapshot")),
            }
        }
        "ovim://lsp/status" => {
            let (tx, rx) = oneshot::channel();
            state
                .tx
                .send(ApiRequest::GetLspStatus(tx))
                .await
                .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

            match rx.await {
                Ok(response) => {
                    if let super::state::ApiResponse::LspStatus(lsp_status) = response {
                        Ok(json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": "application/json",
                                "text": serde_json::to_string_pretty(&lsp_status)
                                    .unwrap_or_else(|_| "{}".to_string())
                            }]
                        }))
                    } else {
                        Err(JsonRpcError::internal_error("Unexpected response type"))
                    }
                }
                Err(_) => Err(JsonRpcError::internal_error("Failed to get LSP status")),
            }
        }
        _ if uri.starts_with("file://") => {
            // For file:// URIs, return the buffer content if it matches
            let file_path = get_current_file_path(&state).await;
            let expected_uri = file_path.map(|p| format!("file://{}", p));

            if Some(uri.to_string()) == expected_uri {
                let (tx, rx) = oneshot::channel();
                state
                    .tx
                    .send(ApiRequest::GetBuffer(tx))
                    .await
                    .map_err(|_| JsonRpcError::internal_error("Editor not available"))?;

                match rx.await {
                    Ok(response) => {
                        if let super::state::ApiResponse::Buffer(buffer) = response {
                            Ok(json!({
                                "contents": [{
                                    "uri": uri,
                                    "mimeType": "text/plain",
                                    "text": buffer.content
                                }]
                            }))
                        } else {
                            Err(JsonRpcError::internal_error("Unexpected response type"))
                        }
                    }
                    Err(_) => Err(JsonRpcError::internal_error("Failed to get buffer")),
                }
            } else {
                Err(JsonRpcError::invalid_params(&format!(
                    "File not found: {}",
                    uri
                )))
            }
        }
        _ => Err(JsonRpcError::invalid_params(&format!(
            "Unknown resource URI: {}",
            uri
        ))),
    }
}

/// Get current file path from editor
async fn get_current_file_path(state: &ApiState) -> Option<String> {
    let (tx, rx) = oneshot::channel();
    state.tx.send(ApiRequest::GetBuffer(tx)).await.ok()?;

    match rx.await {
        Ok(super::state::ApiResponse::Buffer(buffer)) => buffer.file_path,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Tests that exercise `handle_tool_call` against a fake event-loop
    //! receiver. The fake receiver records every `ApiRequest` it sees and
    //! responds inline so we can assert on the handler's channel traffic
    //! without needing a real `Editor` or LSP server.
    use super::*;
    use crate::api::state::{
        ApiRequest, ApiResponse, ApiState, BufferInfo, CursorPosition, EditorSnapshot,
        SendKeysResult, SuccessResponse,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn forwarding_rejects_path_traversal_session_names() {
        for name in ["../../etc/passwd", "..", "a/b", "dev.json"] {
            let error = forward_tool_call_to_session(name, "send_keys", json!({}))
                .await
                .expect_err("traversal-capable session name must be rejected");
            assert_eq!(error.code, -32602, "expected invalid params for {name:?}");
            let reason = error
                .data
                .as_ref()
                .and_then(|data| data.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                reason.contains("Invalid session name"),
                "unexpected error for {name:?}: {reason}"
            );
        }
    }

    #[test]
    fn forwarding_client_is_constructed_with_timeouts() {
        // The timeouts themselves are compile-time constants; this guards
        // against the builder combination becoming invalid.
        forwarding_client().expect("forwarding client must build");
        assert!(FORWARD_CONNECT_TIMEOUT < FORWARD_TOTAL_TIMEOUT);
    }

    fn snapshot_with(mode: &str, hover: Option<&str>) -> EditorSnapshot {
        EditorSnapshot {
            schema_version: crate::api::SNAPSHOT_SCHEMA_VERSION,
            buffer: BufferInfo::default(),
            cursor: CursorPosition::default(),
            mode: mode.to_string(),
            visual_selection: None,
            registers: HashMap::new(),
            marks: HashMap::new(),
            picker: None,
            hover_info: hover.map(|s| s.to_string()),
            ai_chat: None,
            decorations: Vec::new(),
            view: crate::api::ViewSnapshot::default(),
        }
    }

    /// Spawn a fake event loop that responds to a fixed set of requests
    /// and records the request kinds it observed (in order).
    fn spawn_fake_loop(
        snapshot: EditorSnapshot,
    ) -> (
        ApiState,
        Arc<Mutex<Vec<&'static str>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, mut rx) = mpsc::channel::<ApiRequest>(32);
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let log_in_task = log.clone();

        let handle = tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                match req {
                    ApiRequest::SetMode(_, reply) => {
                        log_in_task.lock().unwrap().push("SetMode");
                        let _ = reply.send(ApiResponse::Success(SuccessResponse {
                            success: true,
                            message: None,
                            line_count: None,
                        }));
                    }
                    ApiRequest::SendKeys(_, reply) => {
                        log_in_task.lock().unwrap().push("SendKeys");
                        // Real event loop blocks here on hover; we model the
                        // post-resolution state by simply replying success.
                        let _ = reply.send(ApiResponse::SendKeysResult(SendKeysResult {
                            success: true,
                            message: None,
                            context: crate::api::state::ContextWindowInfo {
                                context: String::new(),
                                file: None,
                                mode: snapshot.mode.clone(),
                                line: 0,
                                column: 0,
                            },
                        }));
                    }
                    ApiRequest::GetSnapshotLight(reply) => {
                        log_in_task.lock().unwrap().push("GetSnapshotLight");
                        let _ = reply.send(ApiResponse::Snapshot(snapshot.clone()));
                    }
                    ApiRequest::GetSnapshot(reply) => {
                        log_in_task.lock().unwrap().push("GetSnapshot");
                        let _ = reply.send(ApiResponse::Snapshot(snapshot.clone()));
                    }
                    _ => {
                        // The lsp_hover path should never reach any other request.
                        log_in_task.lock().unwrap().push("UNEXPECTED");
                    }
                }
            }
        });

        (ApiState::new(tx), log, handle)
    }

    /// Happy-path shape check for the post-OV-00183 lsp_hover handler:
    /// returns the hover text straight from a single snapshot read.
    /// (The old polling loop *would* also have stopped at iteration 1
    /// in this case because mode=HOVER triggered the break — the harder
    /// regression is caught by `lsp_hover_reports_no_hover_when_mode_did_not_switch`
    /// below, where the old code burned all 10 polls.)
    #[tokio::test]
    async fn lsp_hover_issues_single_snapshot_request() {
        let snapshot = snapshot_with("HOVER", Some("u32"));
        let (state, log, _handle) = spawn_fake_loop(snapshot);

        let params = json!({ "name": "lsp_hover" });
        let result = handle_tool_call(state, params).await.expect("tool call");

        // Result should expose the hover text.
        let text = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .expect("text content");
        assert_eq!(text, "u32");

        // Channel traffic: the handler should fire SetMode (pre-K),
        // SendKeys, exactly one GetSnapshotLight, then SetMode (cleanup).
        // Crucially, GetSnapshotLight must appear at most once — the old
        // polling loop fired it up to 10× per call.
        let log = log.lock().unwrap().clone();
        assert_eq!(
            log,
            vec!["SetMode", "SendKeys", "GetSnapshotLight", "SetMode"],
            "lsp_hover should not poll GetSnapshotLight (OV-00183)"
        );
    }

    /// Regression for OV-00183: when the LSP returns no hover at this
    /// position, the editor stays in NORMAL mode (`poll_hover_slot`
    /// only switches to HoverPreview on a non-empty result). The pre-fix
    /// handler would then poll `GetSnapshotLight` all 10 times waiting
    /// for `mode.contains("HOVER")` to become true — burning ~1s of
    /// dead time and 10 channel round-trips on every miss. The fix is
    /// to take a single snapshot post-`SendKeys` and rely on the event
    /// loop having already awaited the hover response.
    #[tokio::test]
    async fn lsp_hover_reports_no_hover_when_mode_did_not_switch() {
        // hover_info is Some — simulating leftover state from an earlier
        // K — but mode is still NORMAL because the new request returned
        // no hover. The handler must NOT surface the stale text.
        let snapshot = snapshot_with("NORMAL", Some("stale leftover"));
        let (state, log, _handle) = spawn_fake_loop(snapshot);

        let params = json!({ "name": "lsp_hover" });
        let result = handle_tool_call(state, params).await.expect("tool call");

        let text = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .expect("text content");
        assert_eq!(text, "No hover information available at cursor position");

        let log = log.lock().unwrap().clone();
        assert_eq!(
            log,
            vec!["SetMode", "SendKeys", "GetSnapshotLight", "SetMode"]
        );
    }
}
