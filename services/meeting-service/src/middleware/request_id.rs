//! Request/correlation ID middleware.
//!
//! The authoritative request ID is ALWAYS generated server-side (`req_<uuid>`): it is the
//! value carried by error envelopes, audit rows, outbox rows / realtime `correlationId`
//! and the request tracing span. A client-supplied `x-request-id` is never trusted as
//! that authority; when well-formed it is kept only as the *client* correlation hint
//! (logged on the span and echoed back as `x-client-request-id`) so callers can still
//! join their logs with ours.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use tracing::Instrument;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
pub const CLIENT_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-client-request-id");

tokio::task_local! {
    static REQUEST_ID: String;
}

/// Current request ID, when called within a request scope.
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(|id| id.clone()).ok()
}

/// Run `fut` with `id` as the current request ID (workers re-entering the correlation
/// of an outbox row, tests).
pub async fn with_request_id<F: std::future::Future>(id: String, fut: F) -> F::Output {
    REQUEST_ID.scope(id, fut).await
}

fn is_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub fn new_request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    let client_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| is_valid(v))
        .map(str::to_string);
    let id = new_request_id();
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        client_request_id = client_id.as_deref().unwrap_or(""),
        method = %request.method(),
        path = %request.uri().path(),
    );

    let mut response = REQUEST_ID
        .scope(id.clone(), next.run(request).instrument(span))
        .await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    if let Some(value) = client_id.and_then(|c| HeaderValue::from_str(&c).ok()) {
        response
            .headers_mut()
            .insert(CLIENT_REQUEST_ID_HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_inbound_ids() {
        assert!(is_valid("req_abc-123"));
        assert!(!is_valid(""));
        assert!(!is_valid("bad id"));
        assert!(!is_valid(&"x".repeat(65)));
    }

    #[test]
    fn server_ids_are_unique_and_prefixed() {
        let a = new_request_id();
        let b = new_request_id();
        assert!(a.starts_with("req_") && is_valid(&a));
        assert_ne!(a, b);
    }
}
