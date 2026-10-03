//! Request/correlation ID middleware.
//!
//! Accepts a well-formed inbound `x-request-id` or generates `req_<uuid>`, exposes it to
//! error envelopes and audit/outbox writes via a task-local, and echoes it on the response.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

tokio::task_local! {
    static REQUEST_ID: String;
}

/// Current request ID, when called within a request scope.
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(|id| id.clone()).ok()
}

fn is_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| is_valid(v))
        .map(str::to_string)
        .unwrap_or_else(|| format!("req_{}", uuid::Uuid::new_v4().simple()));

    let mut response = REQUEST_ID.scope(id.clone(), next.run(request)).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
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
}
