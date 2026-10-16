//! Request / correlation ID middleware.
//!
//! Every customer-facing request gets a server-generated [`RequestContext`]: a new
//! request id and a new correlation id (the operation starts here). Both are stored in the
//! request extensions, made available to error responses through a task-local, recorded
//! on the request tracing span, and the request id is returned as `x-request-id`. A
//! client-supplied `x-request-id` is never trusted as either identifier; if it passes
//! [`ClientCorrelationId`] validation it is logged and echoed back as
//! `x-client-request-id`, otherwise it is dropped.
//!
//! Authenticated internal requests are re-scoped by the internal authentication
//! middleware with the *signed* hop request id and correlation id ([`scoped`]).
//!
//! The span records method and path only — never headers (in particular never
//! `Authorization` / `X-CF-Service-Authorization`) or query strings. After
//! authentication the platform identifiers are recorded on the same span.

use std::future::Future;

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use tracing::Instrument;

use crate::domain::{ClientCorrelationId, RequestContext, RequestId};

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
pub const CLIENT_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-client-request-id");

tokio::task_local! {
    static CURRENT: RequestContext;
}

/// Request id of the request being served, if called inside a request scope.
pub fn current_request_id() -> Option<RequestId> {
    CURRENT.try_with(|c| c.request_id.clone()).ok()
}

/// Request and correlation ids of the request being served.
pub fn current_request_context() -> Option<RequestContext> {
    CURRENT.try_with(Clone::clone).ok()
}

/// Runs `fut` with `context` as the current request context.
pub async fn scoped<F: Future>(context: RequestContext, fut: F) -> F::Output {
    CURRENT.scope(context, fut).await
}

pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let client_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(ClientCorrelationId::parse);
    let context = RequestContext::generate();
    request.extensions_mut().insert(context.clone());

    let span = tracing::info_span!(
        "request",
        request_id = %context.request_id,
        correlation_id = %context.correlation_id,
        client_request_id = client_id.as_ref().map(|c| c.as_str()).unwrap_or(""),
        method = %request.method(),
        path = %request.uri().path(),
        tenant_id = tracing::field::Empty,
        application_id = tracing::field::Empty,
        environment_id = tracing::field::Empty,
        service_account_id = tracing::field::Empty,
        credential_id = tracing::field::Empty,
    );

    let request_id = context.request_id.clone();
    let mut response = scoped(context, next.run(request).instrument(span)).await;
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
