//! Request / correlation ID middleware.
//!
//! Every request gets a server-generated [`RequestId`]. It is stored in the request
//! extensions, made available to error responses through a task-local, recorded on the
//! request tracing span and returned as `x-request-id`. A client-supplied `x-request-id`
//! is never trusted as the request id; if it passes [`ClientCorrelationId`] validation it
//! is logged and echoed back as `x-client-request-id`, otherwise it is dropped.
//!
//! The span records method and path only — never headers (in particular never
//! `Authorization`) or query strings. After authentication the platform identifiers are
//! recorded on the same span.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use tracing::Instrument;

use crate::domain::{ClientCorrelationId, RequestId};

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
pub const CLIENT_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-client-request-id");

tokio::task_local! {
    static CURRENT_REQUEST_ID: RequestId;
}

/// Request id of the request being served, if called inside a request scope.
pub fn current_request_id() -> Option<RequestId> {
    CURRENT_REQUEST_ID.try_with(Clone::clone).ok()
}

pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let client_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(ClientCorrelationId::parse);
    let request_id = RequestId::generate();
    request.extensions_mut().insert(request_id.clone());

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        client_request_id = client_id.as_ref().map(|c| c.as_str()).unwrap_or(""),
        method = %request.method(),
        path = %request.uri().path(),
        tenant_id = tracing::field::Empty,
        application_id = tracing::field::Empty,
        environment_id = tracing::field::Empty,
        service_account_id = tracing::field::Empty,
        credential_id = tracing::field::Empty,
    );

    let mut response = CURRENT_REQUEST_ID
        .scope(request_id.clone(), next.run(request).instrument(span))
        .await;
    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    if let Some(value) = client_id.and_then(|c| HeaderValue::from_str(c.as_str()).ok()) {
        response
            .headers_mut()
            .insert(CLIENT_REQUEST_ID_HEADER, value);
    }
    response
}
