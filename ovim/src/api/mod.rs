mod handlers;
pub mod mcp;
mod mcp_handler;
mod routes;
mod security;
mod state;

pub use mcp::{get_resources, get_tools, JsonRpcRequest, JsonRpcResponse};
pub use state::{
    format_context_window, parse_key_string, AgentArtifactHandle, AgentArtifactsResponse,
    AgentControlPlaneSnapshot, AgentControlResponse, AgentControlTarget, AgentEventsResponse,
    AgentSnapshot, AiChatMessageSnapshot, AiChatSnapshot, ApiRequest, ApiResponse, ApiState,
    BufferInfo, CodeExplanationSnapshot, CodexAuthSnapshot, ContextWindowInfo, CursorPosition,
    DecorationInfo, DiagnosticCounts, DiagnosticItem, DiagnosticsInfo, EditorSnapshot,
    ErrorResponse, HealthInfo, ImageAttachmentSnapshot, LineEntry, LinesResponse,
    LspServerInfoItem, LspStatusInfo, MetricsInfo, ModeInfo, OutlineInfo, OutlineSymbol,
    PickerInfo, PickerResultInfo, QueuedChatSnapshot, RenderInfo, SendKeysResult, SuccessResponse,
    SymbolSearchInfo, SymbolSearchResult, ToolCallSnapshot, TraceInfo, TraceNode, ViewSnapshot,
    VisualSelection, AGENT_API_SCHEMA_VERSION, SNAPSHOT_SCHEMA_VERSION,
};

use anyhow::Result;
use axum::{
    http::Request,
    middleware::{self, Next},
    response::Response,
};
use ovim_core::session::SessionCapability;
use routes::create_router;
use tokio::sync::mpsc;

/// Middleware to add deprecation warning for unversioned API routes
async fn deprecation_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();

    // Check if this is an unversioned route (not starting with /v1, /v2, etc.)
    let is_unversioned = !path.starts_with("/v1")
        && !path.starts_with("/v2")
        && path != "/"
        && path != "/favicon.ico";

    let mut response = next.run(req).await;

    if is_unversioned {
        // Add deprecation header
        if let Ok(header_value) = "Unversioned API paths are deprecated. Use /v1/* instead.".parse()
        {
            response
                .headers_mut()
                .insert("X-API-Deprecation", header_value);
        }

        // Add Sunset header (API sunset date - 6 months from now)
        // Note: This is a static date and should be updated periodically
        if let Ok(header_value) = "Wed, 01 Jul 2026 00:00:00 GMT".parse() {
            response.headers_mut().insert("Sunset", header_value);
        }
    }

    response
}

/// Start the API server on the given address
/// Returns the actual port number the server is listening on
pub async fn start_server(
    addr: &str,
    tx: mpsc::Sender<ApiRequest>,
    port_tx: tokio::sync::oneshot::Sender<u16>,
    capability: SessionCapability,
) -> Result<()> {
    anyhow::ensure!(
        capability.is_configured(),
        "automation API requires a configured session capability"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;
    anyhow::ensure!(
        actual_addr.ip().is_loopback(),
        "automation API must bind to a loopback address"
    );
    let security = security::ApiSecurity::new(capability, actual_addr.port());
    let state = ApiState::new(tx);
    let app = security::secure_router(
        create_router(state).layer(middleware::from_fn(deprecation_middleware)),
        security,
    );

    // Log to LSP log file instead of stderr to avoid garbling TUI output
    ovim_core::lsp_info!("API", "REST API server listening on http://{}", actual_addr);

    // Send the actual port back
    let _ = port_tx.send(actual_addr.port());

    axum::serve(listener, app).await?;

    Ok(())
}
