use super::handlers::{
    decide_agent_approval, delete_lines, edit_line, execute_command, followup_agent, get_agent,
    get_agent_artifacts, get_agent_events, get_agents, get_buffer, get_cursor, get_diagnostics,
    get_health, get_lsp_status, get_metrics, get_mode, get_outline, get_prometheus_metrics,
    get_render, get_snapshot, get_trace, insert_lines, interrupt_agent, paste, read_lines, resize,
    search_symbol, send_agent_message, send_keys, set_buffer, set_mode, wait_agent,
};
use super::mcp_handler::handle_mcp;
use super::state::ApiState;
use axum::{
    routing::{get, post, put},
    Router,
};

/// Create the API router with all routes
pub(crate) fn create_router(state: ApiState) -> Router {
    // V1 API routes (current stable API)
    let v1_routes = Router::new()
        .route("/health", get(get_health))
        .route("/snapshot", get(get_snapshot))
        .route("/keys", post(send_keys))
        .route("/paste", post(paste))
        .route("/resize", post(resize))
        .route("/buffer", get(get_buffer))
        .route("/buffer", put(set_buffer))
        .route("/cursor", get(get_cursor))
        .route("/mode", get(get_mode))
        .route("/mode", post(set_mode))
        .route("/command", post(execute_command))
        .route("/render", get(get_render))
        .route("/lsp/status", get(get_lsp_status))
        .route("/metrics", get(get_metrics))
        .route("/prometheus", get(get_prometheus_metrics))
        .route("/outline", get(get_outline))
        .route("/symbol", get(search_symbol))
        .route("/trace", get(get_trace))
        .route("/diagnostics", get(get_diagnostics))
        .route("/agents", get(get_agents))
        .route("/agents/{agent_id}", get(get_agent))
        .route("/agents/{agent_id}/events", get(get_agent_events))
        .route("/agents/{agent_id}/artifacts", get(get_agent_artifacts))
        .route("/agents/{agent_id}/wait", post(wait_agent))
        .route("/agents/{agent_id}/interrupt", post(interrupt_agent))
        .route("/agents/{agent_id}/messages", post(send_agent_message))
        .route("/agents/{agent_id}/followup", post(followup_agent))
        .route(
            "/agents/{agent_id}/approvals/decision",
            post(decide_agent_approval),
        )
        .route("/edit", post(edit_line))
        .route("/insert", post(insert_lines))
        .route("/delete-lines", post(delete_lines))
        .route("/lines", get(read_lines))
        .route("/mcp", post(handle_mcp));

    // Root router with version namespaces
    Router::new()
        // V1 API under /v1 prefix (recommended)
        .nest("/v1", v1_routes.clone())
        // Legacy routes (no prefix) - for backward compatibility
        // These will be removed in ovim v1.0
        .merge(v1_routes)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AgentControlResponse, ApiRequest, ApiResponse, AGENT_API_SCHEMA_VERSION};
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use ovim_core::{
        agent_runtime::{AgentControlPlaneSnapshot, AGENT_CONTROL_SNAPSHOT_VERSION},
        run_log::{AgentId, OperationId, RunId},
    };
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn versioned_agent_list_route_preserves_snapshot_schema() {
        let (tx, mut rx) = mpsc::channel(1);
        let run_id = RunId::new();
        let root_agent_id = AgentId::new();
        let expected_run = run_id.clone();
        let expected_root = root_agent_id.clone();
        tokio::spawn(async move {
            let ApiRequest::GetAgents { run_id, tx } = rx.recv().await.unwrap() else {
                panic!("expected GetAgents")
            };
            assert_eq!(run_id, expected_run);
            tx.send(ApiResponse::Agents(AgentControlPlaneSnapshot {
                schema_version: AGENT_CONTROL_SNAPSHOT_VERSION,
                run_id,
                root_agent_id: expected_root,
                last_sequence: 0,
                agents: Vec::new(),
                pending_attention: 0,
                pending_updates: 0,
                attention: Default::default(),
            }))
            .unwrap();
        });
        let response = create_router(ApiState::new(tx))
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/agents?run_id={run_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["schema_version"], AGENT_CONTROL_SNAPSHOT_VERSION);
        assert_eq!(body["run_id"], run_id.to_string());
    }

    #[tokio::test]
    async fn wait_route_carries_exact_run_agent_generation_and_operation_ids() {
        let (tx, mut rx) = mpsc::channel(1);
        let run_id = RunId::new();
        let agent_id = AgentId::new();
        let operation_id = OperationId::new();
        let expected = (run_id.clone(), agent_id.clone(), operation_id.clone());
        tokio::spawn(async move {
            let ApiRequest::WaitAgent {
                target,
                timeout_millis,
                tx,
            } = rx.recv().await.unwrap()
            else {
                panic!("expected WaitAgent")
            };
            assert_eq!(target.run_id, expected.0);
            assert_eq!(target.agent_id, expected.1);
            assert_eq!(target.operation_id, expected.2);
            assert_eq!(target.turn_generation, 7);
            assert_eq!(timeout_millis, 250);
            tx.send(ApiResponse::AgentControl(AgentControlResponse {
                schema_version: AGENT_API_SCHEMA_VERSION,
                run_id: target.run_id,
                agent_id: target.agent_id,
                operation_id: target.operation_id,
                result: serde_json::json!({ "outcome": "timed_out" }),
            }))
            .unwrap();
        });
        let response = create_router(ApiState::new(tx))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/agents/{agent_id}/wait"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "run_id": run_id,
                            "turn_generation": 7,
                            "operation_id": operation_id,
                            "timeout_millis": 250
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_and_stale_agent_controls_fail_with_distinct_statuses() {
        let (tx, _rx) = mpsc::channel(1);
        let invalid = create_router(ApiState::new(tx))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agents/not-an-agent/wait")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "run_id": "not-a-run",
                            "turn_generation": 0,
                            "operation_id": "not-an-operation",
                            "timeout_millis": 1
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let (tx, mut rx) = mpsc::channel(1);
        tokio::spawn(async move {
            let ApiRequest::WaitAgent { tx, .. } = rx.recv().await.unwrap() else {
                panic!("expected WaitAgent")
            };
            tx.send(ApiResponse::Error(crate::api::ErrorResponse {
                error: "stale agent generation: expected 0, current is 1".into(),
            }))
            .unwrap();
        });
        let stale = create_router(ApiState::new(tx))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/agents/{}/wait", AgentId::new()))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "run_id": RunId::new(),
                            "turn_generation": 0,
                            "operation_id": OperationId::new(),
                            "timeout_millis": 1
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn approval_route_preserves_attributed_operation_and_request_event_ids() {
        use ovim_core::run_log::EventId;

        let (tx, mut rx) = mpsc::channel(1);
        let run_id = RunId::new();
        let agent_id = AgentId::new();
        let operation_id = OperationId::new();
        let request_event_id = EventId::new();
        let expected = (
            run_id.clone(),
            agent_id.clone(),
            operation_id.clone(),
            request_event_id.clone(),
        );
        tokio::spawn(async move {
            let ApiRequest::DecideAgentApproval {
                target,
                request_event_id,
                allow,
                reason,
                tx,
            } = rx.recv().await.unwrap()
            else {
                panic!("expected DecideAgentApproval")
            };
            assert_eq!(target.run_id, expected.0);
            assert_eq!(target.agent_id, expected.1);
            assert_eq!(target.operation_id, expected.2);
            assert_eq!(request_event_id, expected.3);
            assert!(!allow);
            assert_eq!(reason.as_deref(), Some("unsafe target"));
            tx.send(ApiResponse::Error(crate::api::ErrorResponse {
                error: "stale approval response".into(),
            }))
            .unwrap();
        });
        let response = create_router(ApiState::new(tx))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/agents/{agent_id}/approvals/decision"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "run_id": run_id,
                            "turn_generation": 3,
                            "operation_id": operation_id,
                            "request_event_id": request_event_id,
                            "decision": "deny",
                            "reason": "unsafe target"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
