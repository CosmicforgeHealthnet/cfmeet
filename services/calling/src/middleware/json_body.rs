//! Request bodies are JSON OBJECTS (API contract): serde would otherwise read a JSON
//! array positionally into a request struct (`[6]` as `{"expectedVersion": 6}`), so a
//! malformed body could change state. Every non-empty body of a mutating customer /
//! endpoint request whose first non-whitespace byte is not `{` is refused with the
//! normalized validation error BEFORE any handler deserializes it. The body is
//! re-inserted unchanged (signatures over raw bytes keep verifying).

use axum::{
    body::Body,
    extract::Request,
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::error::CallingError;

/// Upper bound read here (route-specific limits still apply inside).
const MAX_INSPECTED_BYTES: usize = 2 * 1024 * 1024;

pub async fn require_object_body(request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_INSPECTED_BYTES).await else {
        return CallingError::Validation("The request body is invalid.").into_response();
    };
    if let Some(first) = bytes.iter().find(|b| !b.is_ascii_whitespace()) {
        if *first != b'{' {
            return CallingError::Validation("The request body must be a JSON object.")
                .into_response();
        }
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}
