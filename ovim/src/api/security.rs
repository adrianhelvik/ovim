use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json, Router,
};
use ovim_core::session::SessionCapability;
use serde_json::json;

/// Security boundary for one loopback automation session.
#[derive(Clone)]
pub(super) struct ApiSecurity {
    capability: SessionCapability,
    port: u16,
}

impl ApiSecurity {
    pub(super) fn new(capability: SessionCapability, port: u16) -> Self {
        Self { capability, port }
    }

    fn host_is_allowed(&self, request: &Request<axum::body::Body>) -> bool {
        let Ok(host) = request
            .headers()
            .get(header::HOST)
            .ok_or(())
            .and_then(|value| value.to_str().map_err(|_| ()))
        else {
            return false;
        };

        host == format!("127.0.0.1:{}", self.port)
            || host.eq_ignore_ascii_case(&format!("localhost:{}", self.port))
    }

    fn bearer_is_valid(&self, request: &Request<axum::body::Body>) -> bool {
        let Some(value) = request.headers().get(header::AUTHORIZATION) else {
            return false;
        };
        let Ok(value) = value.to_str() else {
            return false;
        };
        let Some(candidate) = value.strip_prefix("Bearer ") else {
            return false;
        };

        constant_time_eq(
            candidate.as_bytes(),
            self.capability.expose_secret().as_bytes(),
        )
    }
}

fn constant_time_eq(candidate: &[u8], expected: &[u8]) -> bool {
    let mut difference = candidate.len() ^ expected.len();
    let width = candidate.len().max(expected.len());
    for index in 0..width {
        let left = candidate.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn rejection(status: StatusCode, message: &'static str) -> Response {
    let mut response = (status, Json(json!({ "error": message }))).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    response
}

/// Reject untrusted browser and network requests before route extraction or
/// body parsing can dispatch work to the editor event loop.
async fn require_session_capability(
    State(security): State<ApiSecurity>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if !security.host_is_allowed(&request) {
        return rejection(StatusCode::FORBIDDEN, "request host is not allowed");
    }
    if request.headers().contains_key(header::ORIGIN) {
        return rejection(
            StatusCode::FORBIDDEN,
            "browser-origin requests are not allowed",
        );
    }
    if !security.bearer_is_valid(&request) {
        return rejection(StatusCode::UNAUTHORIZED, "authorization required");
    }

    next.run(request).await
}

pub(super) fn secure_router(router: Router, security: ApiSecurity) -> Router {
    router.layer(axum::middleware::from_fn_with_state(
        security,
        require_session_capability,
    ))
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, secure_router, ApiSecurity};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use ovim_core::session::SessionCapability;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[test]
    fn constant_time_comparison_handles_equal_wrong_and_different_lengths() {
        assert!(constant_time_eq(b"synthetic", b"synthetic"));
        assert!(!constant_time_eq(b"synthetiC", b"synthetic"));
        assert!(!constant_time_eq(b"short", b"synthetic"));
        assert!(!constant_time_eq(b"synthetic-extra", b"synthetic"));
    }

    #[tokio::test]
    async fn rejects_untrusted_requests_before_dispatch_and_accepts_session_client() {
        let capability = SessionCapability::generate();
        let secret = capability.expose_secret().to_string();
        let dispatches = Arc::new(AtomicUsize::new(0));
        let handler_dispatches = Arc::clone(&dispatches);
        let router = Router::new().route(
            "/mutate",
            post(move || {
                let handler_dispatches = Arc::clone(&handler_dispatches);
                async move {
                    handler_dispatches.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let app = secure_router(router, ApiSecurity::new(capability, 4242));

        for request in [
            Request::builder()
                .method("POST")
                .uri("/mutate")
                .header("host", "127.0.0.1:4242")
                .body(Body::from("not-json"))
                .unwrap(),
            Request::builder()
                .method("POST")
                .uri("/mutate")
                .header("host", "127.0.0.1:4242")
                .header("authorization", "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let invalid_host = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header("host", "attacker.invalid")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_host.status(), StatusCode::FORBIDDEN);

        let browser_origin = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header("host", "localhost:4242")
                    .header("origin", "http://localhost:3000")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(browser_origin.status(), StatusCode::FORBIDDEN);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);

        let authorized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header("host", "127.0.0.1:4242")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::NO_CONTENT);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }
}
